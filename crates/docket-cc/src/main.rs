//! `docket-cc`: the Claude Code adapter (layer 3, see docs/architecture.md).
//!
//! This binary currently implements one piece of that layer's job — file
//! representation + identifier mapping: `sync` projects every item in a
//! worker's owned topics onto a local `.md` file, so a Claude Code session
//! can read its work without going through the HTTP API directly. Like
//! `docket-mcp`, it never links `docket_core` as a library — it only speaks
//! `docket-core`'s HTTP/JSON contract (docs/architecture.md "Four layers").
//!
//! The projection root lives outside any repo, per
//! [ADR-0008](../../docs/decisions/ADR-0008-file-representation-location.md);
//! layout mirrors the topic path (`<root>/<topic>/...`). Each item's `body`
//! becomes the file's markdown body (docs/glossary.md: `body` <-> `.md
//! file`) with the rest of the item as frontmatter.
//!
//! Also implements the `hook` subcommand a Claude Code `SessionStart` hook
//! calls, and `topic` — deriving the `topic` a given directory belongs to
//! (nearest `.git` remote, walked up through monorepo/submodule/worktree
//! structure; see [`topic`]) so a caller doesn't have to invent that string
//! by hand. Not yet implemented: the local daemon `hook` could run behind
//! instead of a one-shot `sync`. `sync` also only ever writes/updates
//! files; it never removes a stale projection (an item that closed, or a
//! topic the worker no longer owns) — deliberately left open rather than
//! picked here, since it is its own design question, not an implementation
//! detail of `sync`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

mod topic;

#[derive(Debug, Deserialize)]
struct ItemDto {
    id: String,
    topic: String,
    title: String,
    body: Option<String>,
    state: String,
    resolution: Option<String>,
    /// Absent from servers older than ADR-0010. Defaulted so an old-server
    /// response still deserializes.
    #[serde(default)]
    requester: Option<String>,
    /// Was `owner` before ADR-0010.
    #[serde(default)]
    assignee: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn http_client() -> reqwest::Client {
    // Same rationale as docket-mcp: an unreachable docket-core must surface
    // as an error, not hang the sync forever.
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client builds with a plain timeout")
}

fn default_root() -> PathBuf {
    dirs::data_dir()
        .expect("no user data directory available on this platform")
        .join("docket")
}

/// Windows-reserved device names (case-insensitive) — unsafe as a path
/// segment even though every individual character in them is otherwise
/// fine. Checked defensively on every platform so a projection created on
/// Windows and read on Linux (or vice versa, e.g. over a synced drive)
/// doesn't depend on which OS wrote it.
fn is_reserved_windows_name(upper: &str) -> bool {
    matches!(
        upper,
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Encodes one path segment (one topic segment, or an item id) so it is
/// safe to use as a filesystem name on any of the platforms `docket-core`
/// itself supports. `docket-core` does not constrain topic segment
/// characters — this only has to not crash or corrupt on whatever the core
/// currently allows, not decide what *should* be allowed.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for ch in segment.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            for byte in ch.to_string().as_bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    let upper = out.to_ascii_uppercase();
    // Checked against the *original* segment, not `out` — by this point a
    // trailing space or dot has already been percent-encoded away, so
    // checking `out` would never see it.
    let unsafe_name = segment.is_empty()
        || segment.ends_with('.')
        || segment.ends_with(' ')
        || is_reserved_windows_name(&upper);
    // The escape marker is `%5F` (a percent-encoded `_`), not a literal
    // `_` — a literal leading underscore is itself a safe passthrough
    // character above, so it never becomes `%5F` on its own. Using a bare
    // `_` here would make "CON" and "_CON" collide onto the same encoded
    // segment; `%5F` can only ever come from this escape, so it can't.
    if unsafe_name {
        format!("%5F{out}")
    } else {
        out
    }
}

/// Maps a topic onto a directory under `root`, mirroring the topic path
/// (ADR-0008) with each `/`-separated segment encoded independently.
fn topic_to_path(root: &Path, topic: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in topic.split('/') {
        path.push(encode_segment(segment));
    }
    path
}

/// Renders an item as frontmatter + markdown body. Write-only for now —
/// whether editing a file can change state is a separate, deliberately
/// unresolved design question (it is in tension with `docket-core` being
/// the sole source of truth), so nothing reads this format back yet.
fn render_item_file(item: &ItemDto) -> String {
    format!(
        "---\nid: {}\ntopic: {}\nstate: {}\nresolution: {}\nrequester: {}\nassignee: {}\ncreated_at: {}\nupdated_at: {}\n---\n\n# {}\n\n{}\n",
        item.id,
        item.topic,
        item.state,
        item.resolution.as_deref().unwrap_or("null"),
        item.requester.as_deref().unwrap_or("null"),
        item.assignee.as_deref().unwrap_or("null"),
        item.created_at,
        item.updated_at,
        item.title,
        item.body.as_deref().unwrap_or(""),
    )
}

/// Writes an item's projection, replacing any prior version. Writes to a
/// sibling temp file and renames into place (`fs::rename` is atomic within
/// the same directory on both Windows and Unix) so a session reading the
/// file mid-write never sees a partial file.
fn write_item_file(root: &Path, item: &ItemDto) -> std::io::Result<PathBuf> {
    let dir = topic_to_path(root, &item.topic);
    std::fs::create_dir_all(&dir)?;
    let final_path = dir.join(format!("{}.md", encode_segment(&item.id)));
    let tmp_path = dir.join(format!(
        ".{}.tmp-{}",
        encode_segment(&item.id),
        std::process::id()
    ));
    std::fs::write(&tmp_path, render_item_file(item))?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

/// Fetches every item in `worker_id`'s owned topics and projects each to a
/// file. Returns the projected items. `worker_id` must already be
/// registered with `docket-core` — sync doesn't register on the caller's
/// behalf, since which topics to own is a decision `docket-cc` doesn't get
/// to make for the caller.
async fn sync(
    client: &reqwest::Client,
    base_url: &str,
    worker_id: &str,
    root: &Path,
) -> anyhow::Result<Vec<ItemDto>> {
    // `GET /items?topic_scope=` no longer 404s for an unregistered worker —
    // it's a read filter, and an unregistered worker just has no matching
    // topics (see docs/usage.md §4's read/write not-found asymmetry). So
    // registration has to be confirmed separately, via the one endpoint
    // that fetches a specific worker by id and does 404 when it's missing.
    let worker_resp = client
        .get(format!("{base_url}/workers/{worker_id}"))
        .send()
        .await?;
    if !worker_resp.status().is_success() {
        anyhow::bail!(
            "docket-core returned {} fetching worker '{worker_id}' — is it registered?",
            worker_resp.status()
        );
    }

    let resp = client
        .get(format!("{base_url}/items"))
        .query(&[("topic_scope", worker_id)])
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "docket-core returned {} listing items owned by '{worker_id}'",
            resp.status()
        );
    }
    let items: Vec<ItemDto> = resp.json().await?;
    for item in &items {
        write_item_file(root, item)?;
    }
    Ok(items)
}

/// Renders the open items among `items` as a short human-readable summary,
/// or an empty string when there is nothing open — a `SessionStart` hook's
/// stdout is injected into context verbatim, so "nothing to report" must
/// be silence, not a "0 items" line every session.
fn format_hook_summary(items: &[ItemDto]) -> String {
    let open: Vec<&ItemDto> = items.iter().filter(|i| i.state == "open").collect();
    if open.is_empty() {
        return String::new();
    }
    let mut out = format!("docket: {} open item(s) waiting.\n", open.len());
    for item in &open {
        out.push_str(&format!(
            "- {} — {} ({})\n",
            item.topic, item.title, item.id
        ));
    }
    out
}

/// `sync`s and formats the result for a `SessionStart` hook. Never returns
/// an error — a hook's output goes straight into context, so a sync
/// failure (core unreachable, worker not registered, ...) is reported to
/// stderr (not injected) and summarized as nothing to report, rather than
/// propagated as a process failure that could disrupt session startup.
async fn hook_summary(
    client: &reqwest::Client,
    base_url: &str,
    worker_id: &str,
    root: &Path,
) -> String {
    match sync(client, base_url, worker_id, root).await {
        Ok(items) => format_hook_summary(&items),
        Err(e) => {
            eprintln!("docket-cc hook: sync failed, reporting nothing this session: {e}");
            String::new()
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `topic` is purely local (no docket-core connectivity needed), so it
    // is dispatched before the env vars below are required — a caller
    // asking "what topic am I in" shouldn't need a worker already
    // registered to get an answer.
    if std::env::args().nth(1).as_deref() == Some("topic") {
        let cwd = std::env::current_dir()?;
        println!("{}", topic::derive_topic(&cwd));
        return Ok(());
    }

    let base_url =
        std::env::var("DOCKET_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:8420".to_string());
    let worker_id = std::env::var("DOCKET_WORKER_ID").map_err(|_| {
        anyhow::anyhow!(
            "DOCKET_WORKER_ID must be set to a worker id already registered with docket-core"
        )
    })?;
    let root = std::env::var("DOCKET_CC_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_root());
    let client = http_client();

    // `hook` is meant to be invoked from a Claude Code `SessionStart` hook
    // (see the module doc) — bare invocation (no args) keeps the original
    // `sync` behavior so existing setups (README "Running it") don't break.
    if std::env::args().nth(1).as_deref() == Some("hook") {
        print!(
            "{}",
            hook_summary(&client, &base_url, &worker_id, &root).await
        );
        return Ok(());
    }

    let items = sync(&client, &base_url, &worker_id, &root).await?;
    println!(
        "docket-cc: projected {} item(s) for worker '{worker_id}' under {}",
        items.len(),
        root.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_segment_passes_through() {
        assert_eq!(encode_segment("iyulab"), "iyulab");
        assert_eq!(encode_segment("docket-cc_v1.0"), "docket-cc_v1.0");
    }

    #[test]
    fn unsafe_characters_are_percent_encoded() {
        assert_eq!(encode_segment("a:b"), "a%3Ab");
        assert_eq!(encode_segment("a<b>c"), "a%3Cb%3Ec");
    }

    #[test]
    fn trailing_dot_or_space_gets_prefixed() {
        assert_eq!(encode_segment("trailing."), "%5Ftrailing.");
        assert_eq!(encode_segment("trailing "), "%5Ftrailing%20");
    }

    #[test]
    fn reserved_windows_names_get_prefixed() {
        assert_eq!(encode_segment("CON"), "%5FCON");
        assert_eq!(encode_segment("con"), "%5Fcon");
        assert_eq!(encode_segment("COM1"), "%5FCOM1");
        assert_eq!(encode_segment("COM10"), "COM10"); // not actually reserved
    }

    #[test]
    fn empty_segment_gets_prefixed_not_dropped() {
        assert_eq!(encode_segment(""), "%5F");
    }

    /// The escape marker must not collide with a segment that legitimately
    /// starts with `_` — found in review: an earlier version used a bare
    /// `_` marker, so `encode_segment("CON")` and `encode_segment("_CON")`
    /// both produced `"_CON"`, silently merging two different topics'
    /// projections into one directory.
    #[test]
    fn escaped_and_literal_underscore_prefixed_segments_do_not_collide() {
        assert_ne!(encode_segment("CON"), encode_segment("_CON"));
        assert_eq!(encode_segment("_CON"), "_CON"); // literal underscore passes through
    }

    #[test]
    fn topic_path_mirrors_segments_under_root() {
        let root = Path::new("/root");
        let path = topic_to_path(root, "iyulab/docket");
        assert_eq!(path, PathBuf::from("/root/iyulab/docket"));
    }

    fn sample_item() -> ItemDto {
        ItemDto {
            id: "abc123".to_string(),
            topic: "iyulab/docket".to_string(),
            title: "fix the thing".to_string(),
            body: Some("some detail".to_string()),
            state: "open".to_string(),
            resolution: None,
            requester: None,
            assignee: None,
            created_at: 1,
            updated_at: 2,
        }
    }

    #[test]
    fn rendered_file_has_frontmatter_and_body() {
        let rendered = render_item_file(&sample_item());
        assert!(rendered.starts_with("---\nid: abc123\n"));
        assert!(rendered.contains("state: open\n"));
        assert!(rendered.contains("resolution: null\n"));
        assert!(rendered.contains("# fix the thing"));
        assert!(rendered.contains("some detail"));
    }

    #[test]
    fn hook_summary_is_empty_when_nothing_open() {
        let mut claimed = sample_item();
        claimed.state = "claimed".to_string();
        assert_eq!(format_hook_summary(&[claimed]), "");
        assert_eq!(format_hook_summary(&[]), "");
    }

    #[test]
    fn hook_summary_lists_open_items_only() {
        let open = sample_item();
        let mut claimed = sample_item();
        claimed.id = "def456".to_string();
        claimed.state = "claimed".to_string();
        let summary = format_hook_summary(&[open, claimed]);
        assert!(summary.contains("1 open item(s)"));
        assert!(summary.contains("abc123"));
        assert!(!summary.contains("def456"));
    }

    #[test]
    fn write_then_overwrite_leaves_only_final_file_no_tmp_residue() {
        let dir = std::env::temp_dir().join(format!("docket-cc-write-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let item = sample_item();

        let path1 = write_item_file(&dir, &item).unwrap();
        assert!(path1.exists());
        assert!(
            std::fs::read_to_string(&path1)
                .unwrap()
                .contains("fix the thing")
        );

        let mut updated = sample_item();
        updated.state = "claimed".to_string();
        updated.assignee = Some("w1".to_string());
        let path2 = write_item_file(&dir, &updated).unwrap();
        assert_eq!(path1, path2, "same item id must overwrite the same file");
        let contents = std::fs::read_to_string(&path2).unwrap();
        assert!(contents.contains("state: claimed"));
        assert!(contents.contains("assignee: w1"));

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file should survive a successful write"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    struct CoreProcess {
        child: std::process::Child,
        base_url: String,
    }

    impl Drop for CoreProcess {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Sibling binary in the same workspace `target/` — same approach as
    /// docket-mcp's integration tests (Cargo only exposes
    /// `CARGO_BIN_EXE_*` for a package's own binaries).
    fn docket_core_binary() -> PathBuf {
        let mut path = std::env::current_exe().expect("current test executable path");
        path.pop(); // .../target/debug/deps
        path.pop(); // .../target/debug
        path.push(if cfg!(windows) {
            "docket-core.exe"
        } else {
            "docket-core"
        });
        path
    }

    async fn spawn_core(port: u16, db_path: &Path) -> CoreProcess {
        let binary = docket_core_binary();
        let child = std::process::Command::new(&binary)
            .env("DOCKET_PORT", port.to_string())
            .env("DOCKET_DB_PATH", db_path)
            .spawn()
            .unwrap_or_else(|e| {
                panic!("failed to spawn {binary:?} (run `cargo build -p docket-core` first): {e}")
            });
        let base_url = format!("http://127.0.0.1:{port}");
        let process = CoreProcess { child, base_url };
        let client = reqwest::Client::new();
        for _ in 0..50 {
            if client
                .get(format!("{}/items", process.base_url))
                .send()
                .await
                .is_ok()
            {
                return process;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("docket-core did not become ready within 5s");
    }

    /// End to end: create an item, register a worker owning its topic, run
    /// `sync`, and check the projected file exists with the right content
    /// — exercising the real cc -> core HTTP hop, not just the pure
    /// path/render helpers above.
    #[tokio::test]
    async fn sync_projects_owned_items_to_files() {
        let test_dir =
            std::env::temp_dir().join(format!("docket-cc-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).unwrap();
        let db_path = test_dir.join("sync.db");
        let projection_root = test_dir.join("projection");
        let core = spawn_core(18430, &db_path).await;
        let client = http_client();

        let created: serde_json::Value = client
            .post(format!("{}/items", core.base_url))
            .json(&serde_json::json!({"topic": "iyulab/docket", "title": "fix the thing", "body": "detail"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let item_id = created["id"].as_str().unwrap();

        client
            .post(format!("{}/workers", core.base_url))
            .json(&serde_json::json!({"id": "w1", "topics": ["iyulab"]}))
            .send()
            .await
            .unwrap();

        let synced = sync(&client, &core.base_url, "w1", &projection_root)
            .await
            .unwrap();
        assert_eq!(synced.len(), 1);

        let expected_path = projection_root
            .join("iyulab")
            .join("docket")
            .join(format!("{item_id}.md"));
        let contents = std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("expected projection at {expected_path:?}: {e}"));
        assert!(contents.contains(&format!("id: {item_id}")));
        assert!(contents.contains("state: open"));
        assert!(contents.contains("# fix the thing"));
        assert!(contents.contains("detail"));

        // Drop the still-running core process (and its open handle on
        // `sync.db`) before cleanup — Windows, unlike Unix, refuses to
        // remove a directory containing a file another process has open.
        drop(core);
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    /// A worker that was never registered must surface as a clear error,
    /// not a panic or a silently-empty projection.
    #[tokio::test]
    async fn sync_for_unregistered_worker_is_an_error() {
        let test_dir = std::env::temp_dir().join(format!(
            "docket-cc-sync-unregistered-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        let db_path = test_dir.join("sync.db");
        let core = spawn_core(18431, &db_path).await;
        let client = http_client();

        let result = sync(
            &client,
            &core.base_url,
            "ghost",
            &test_dir.join("projection"),
        )
        .await;
        assert!(result.is_err());

        drop(core);
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    /// The `hook` subcommand's actual output: real core, real item, real
    /// worker — not just the pure `format_hook_summary` helper.
    #[tokio::test]
    async fn hook_summary_reports_open_items_end_to_end() {
        let test_dir =
            std::env::temp_dir().join(format!("docket-cc-hook-test-{}", std::process::id()));
        std::fs::create_dir_all(&test_dir).unwrap();
        let db_path = test_dir.join("hook.db");
        let core = spawn_core(18432, &db_path).await;
        let client = http_client();

        client
            .post(format!("{}/items", core.base_url))
            .json(&serde_json::json!({"topic": "iyulab/docket", "title": "hook me up"}))
            .send()
            .await
            .unwrap();
        client
            .post(format!("{}/workers", core.base_url))
            .json(&serde_json::json!({"id": "w1", "topics": ["iyulab"]}))
            .send()
            .await
            .unwrap();

        let summary =
            hook_summary(&client, &core.base_url, "w1", &test_dir.join("projection")).await;
        assert!(summary.contains("hook me up"));
        assert!(summary.contains("iyulab/docket"));

        drop(core);
        std::fs::remove_dir_all(&test_dir).unwrap();
    }

    /// A sync failure inside `hook_summary` must come back as an empty
    /// string (silently nothing to report), never a panic or a propagated
    /// error — that is the whole point of the hook / sync split.
    #[tokio::test]
    async fn hook_summary_is_empty_not_an_error_when_sync_fails() {
        let test_dir = std::env::temp_dir().join(format!(
            "docket-cc-hook-unregistered-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).unwrap();
        let db_path = test_dir.join("hook.db");
        let core = spawn_core(18433, &db_path).await;
        let client = http_client();

        let summary = hook_summary(
            &client,
            &core.base_url,
            "ghost",
            &test_dir.join("projection"),
        )
        .await;
        assert_eq!(summary, "");

        drop(core);
        std::fs::remove_dir_all(&test_dir).unwrap();
    }
}
