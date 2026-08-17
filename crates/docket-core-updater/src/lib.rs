//! Orchestrates a single check-and-maybe-update pass for `docket-core`:
//! fetch its local `/status`, compare against the latest GitHub release,
//! and — if warranted — download+verify+swap+restart+smoke, rolling back
//! on smoke failure. The 15-minute repeat comes from a Windows Scheduled
//! Task registered outside this crate (docket-works-private install
//! script, design §4) — this binary itself does not loop or sleep between
//! checks.

pub mod decision;
pub mod deploy;
pub mod download;
pub mod smoke;
pub mod status;
pub mod task_control;

use std::path::{Path, PathBuf};
use std::time::Duration;

const USER_AGENT: &str = "docket-core-updater";
const OWNER: &str = "iyulab";
const REPO: &str = "docket";

#[cfg(windows)]
const EXE_NAME: &str = "docket-core.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "docket-core";

/// Bounds on the two polling waits `run()` does around the actual swap —
/// both are safety margins for a Windows scheduled-task stop/start, whose
/// exit code only confirms the request was accepted, not that the old
/// process has released its port/file handles or that the new one has
/// finished starting.
const STOP_POLL_ATTEMPTS: u32 = 15;
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(500);
const HEALTH_POLL_ATTEMPTS: u32 = 30;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct UpdateContext<'a> {
    pub status_url: &'a str,
    pub smoke_url: &'a str,
    pub api_base: &'a str,
    pub idle_threshold_secs: u64,
    pub deploy_root: &'a Path,
    pub task_name: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    NotIdleEnough {
        idle_seconds: u64,
    },
    /// `tag` previously failed `wait_for_healthy` and was marked via
    /// `known_bad_tag_path` — skipped without touching the network or the
    /// deployment, so a genuinely broken release doesn't cost a full
    /// stop/swap/restart/rollback cycle (and its outage window) every 15
    /// minutes until a newer release appears.
    SkippedKnownBad {
        tag: String,
    },
    Updated {
        from: String,
        to: String,
    },
    RolledBack {
        attempted: String,
        reason: String,
    },
}

fn known_bad_tag_path(deploy_root: &Path) -> PathBuf {
    deploy_root.join(".docket-core-updater-last-failed-tag")
}

/// The tag of the most recent release whose swap failed health
/// verification, if any — read fresh every `run()` so a later manual fix
/// (or a newer release) converges the next time this file's content
/// differs from the one being considered.
fn known_bad_tag(deploy_root: &Path) -> Option<String> {
    std::fs::read_to_string(known_bad_tag_path(deploy_root))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Runs one check: fetch local `/status` + the latest GitHub release, and
/// either skip (already current, not idle long enough, or a known-bad
/// release) or download, verify, swap, restart, and verify the restarted
/// process actually serves the target version — rolling back and
/// restarting on any failure in that sequence rather than leaving
/// `docket-core` down.
pub async fn run(ctx: &UpdateContext<'_>) -> anyhow::Result<Outcome> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?;

    let local = status::fetch_status(&client, ctx.status_url).await?;
    let release =
        docket_launcher_core::release_client::latest_release(&client, ctx.api_base, OWNER, REPO)
            .await?;

    if !decision::should_update(
        &local.version,
        &release.tag_name,
        local.idle_seconds,
        ctx.idle_threshold_secs,
    ) {
        return Ok(if local.version == release.tag_name {
            Outcome::UpToDate
        } else {
            Outcome::NotIdleEnough {
                idle_seconds: local.idle_seconds,
            }
        });
    }

    if known_bad_tag(ctx.deploy_root).as_deref() == Some(release.tag_name.as_str()) {
        return Ok(Outcome::SkippedKnownBad {
            tag: release.tag_name,
        });
    }

    let paths = deploy::DeployPaths::under(ctx.deploy_root, EXE_NAME);
    if !paths.exe.exists() {
        // A Scheduled Task with no working directory configured runs in
        // C:\Windows\System32 — DOCKET_UPDATER_DEPLOY_DIR defaulting to "."
        // would otherwise make that look like "nothing deployed here yet,
        // treat as a fresh install" instead of a misconfiguration, silently
        // writing into the wrong directory while the real deployment goes
        // untouched.
        anyhow::bail!(
            "no existing docket-core at {} — refusing to treat an unconfigured deploy_root as a fresh install (check DOCKET_UPDATER_DEPLOY_DIR)",
            paths.exe.display()
        );
    }

    let asset_name =
        docket_launcher_core::platform::current_asset_name("docket-core").ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported platform: {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;

    let checksums_txt = download::fetch_checksums(&client, &release, USER_AGENT).await?;
    let exe_bytes = download::fetch_asset_bytes(&client, &release, &asset_name, USER_AGENT).await?;
    download::verify(&checksums_txt, &asset_name, &exe_bytes)?;
    let zip_bytes =
        download::fetch_asset_bytes(&client, &release, "console-dist.zip", USER_AGENT).await?;
    download::verify(&checksums_txt, "console-dist.zip", &zip_bytes)?;

    task_control::stop(ctx.task_name)?;
    wait_until_stopped(&client, ctx.status_url).await?;

    match swap_and_verify(
        &client,
        ctx,
        &paths,
        &exe_bytes,
        &zip_bytes,
        &release.tag_name,
    )
    .await
    {
        Ok(()) => {
            // A successful update means whatever tag was previously marked
            // bad is no longer relevant to this deploy_root's history.
            let _ = std::fs::remove_file(known_bad_tag_path(ctx.deploy_root));
            Ok(Outcome::Updated {
                from: local.version,
                to: release.tag_name,
            })
        }
        Err(e) => {
            // Whatever state swap_and_verify left things in (new binary
            // alive-but-wrong-version, crashed, or never came back up),
            // always attempt both rollback and restart before surfacing the
            // failure — a partial failure here must never leave docket-core
            // stopped with no next-cycle recovery path: run() starts every
            // check by hitting /status, which would then never succeed
            // again on its own.
            let _ = task_control::stop(ctx.task_name);
            let rollback_result = deploy::rollback(&paths);
            let _ = task_control::start(ctx.task_name);
            let _ = std::fs::write(known_bad_tag_path(ctx.deploy_root), &release.tag_name);
            rollback_result?;
            Ok(Outcome::RolledBack {
                attempted: release.tag_name,
                reason: e.to_string(),
            })
        }
    }
}

/// Installs the downloaded exe + console zip and waits for the restarted
/// process to report the target version and pass a smoke check. Split out
/// of `run()` so every failure in this sequence — the install itself, or
/// the post-restart health wait — funnels through the same
/// rollback-then-restart handling in `run()`'s `Err` arm.
async fn swap_and_verify(
    client: &reqwest::Client,
    ctx: &UpdateContext<'_>,
    paths: &deploy::DeployPaths,
    exe_bytes: &[u8],
    zip_bytes: &[u8],
    target_version: &str,
) -> anyhow::Result<()> {
    deploy::install(paths, exe_bytes, zip_bytes)?;
    task_control::start(ctx.task_name)?;
    wait_for_healthy(client, ctx, target_version).await
}

/// Polls local `/status` until it stops responding (the old process has
/// actually exited, not just accepted a `schtasks /End` request) or the
/// bound is exhausted — bails *before* any file is touched if the old
/// process still seems to be up, since swapping files under a possibly
/// still-running process is exactly the file-lock failure mode this exists
/// to avoid.
async fn wait_until_stopped(client: &reqwest::Client, status_url: &str) -> anyhow::Result<()> {
    for _ in 0..STOP_POLL_ATTEMPTS {
        if status::fetch_status(client, status_url).await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(STOP_POLL_INTERVAL).await;
    }
    anyhow::bail!(
        "docket-core is still responding at {status_url} after stopping its scheduled task \
         — refusing to swap files under a possibly still-running process"
    )
}

/// Waits for the restarted `docket-core` to report `expected_version` via
/// `/status`, then smoke-tests it — bounded, so a cold start (SQLite WAL
/// recovery, antivirus scanning a freshly written unsigned exe) has time to
/// finish, but two failure modes a flat sleep-then-check-once can't tell
/// apart from success are caught instead: a stale process still serving the
/// *old* version after a restart race, and a version stream that never
/// converges (`docket-core`'s own `Cargo.toml` version vs. the repo-wide
/// release tag `/status` is compared against).
async fn wait_for_healthy(
    client: &reqwest::Client,
    ctx: &UpdateContext<'_>,
    expected_version: &str,
) -> anyhow::Result<()> {
    let mut last_err = None;
    for _ in 0..HEALTH_POLL_ATTEMPTS {
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        match status::fetch_status(client, ctx.status_url).await {
            Ok(s) if s.version == expected_version => {
                return smoke::check(client, ctx.smoke_url).await;
            }
            Ok(s) => {
                last_err = Some(anyhow::anyhow!(
                    "docket-core is serving version {} (expected {expected_version})",
                    s.version
                ));
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("docket-core never came back up")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves a fixed two-request sequence — local `/status` first, then
    /// GitHub `releases/latest` — then stops. Matches `run`'s early-return
    /// branches, which never issue a third request.
    async fn serve_status_then_release(
        status_body: String,
        release_body: String,
    ) -> (String, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in [status_body, release_body] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let base = format!("http://{addr}");
        (format!("{base}/status"), base)
    }

    fn ctx<'a>(status_url: &'a str, api_base: &'a str, deploy_root: &'a Path) -> UpdateContext<'a> {
        UpdateContext {
            status_url,
            smoke_url: "unused-in-these-tests",
            api_base,
            idle_threshold_secs: 300,
            deploy_root,
            task_name: "docket-core",
        }
    }

    #[tokio::test]
    async fn already_up_to_date_skips_without_touching_deploy() {
        let (status_url, api_base) = serve_status_then_release(
            r#"{"version":"v0.2.0","idle_seconds":10000}"#.to_string(),
            r#"{"tag_name":"v0.2.0","assets":[]}"#.to_string(),
        )
        .await;
        let deploy_root = std::env::temp_dir().join("docket-core-updater-run-test-up-to-date");
        let outcome = run(&ctx(&status_url, &api_base, &deploy_root))
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::UpToDate);
        assert!(
            !deploy_root.exists(),
            "run() must not touch the filesystem when already up to date"
        );
    }

    #[tokio::test]
    async fn different_version_but_not_idle_enough_skips_without_touching_deploy() {
        let (status_url, api_base) = serve_status_then_release(
            r#"{"version":"v0.1.0","idle_seconds":5}"#.to_string(),
            r#"{"tag_name":"v0.2.0","assets":[]}"#.to_string(),
        )
        .await;
        let deploy_root = std::env::temp_dir().join("docket-core-updater-run-test-not-idle");
        let outcome = run(&ctx(&status_url, &api_base, &deploy_root))
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::NotIdleEnough { idle_seconds: 5 });
        assert!(
            !deploy_root.exists(),
            "run() must not touch the filesystem when not idle enough"
        );
    }

    #[tokio::test]
    async fn known_bad_tag_matching_the_release_skips_without_downloading() {
        let (status_url, api_base) = serve_status_then_release(
            r#"{"version":"v0.1.0","idle_seconds":10000}"#.to_string(),
            // Empty `assets` — if run() got as far as resolving an asset
            // name and downloading, it would fail loudly (no asset found)
            // rather than silently succeed, so this also proves the skip
            // happens before any download attempt.
            r#"{"tag_name":"v0.2.0","assets":[]}"#.to_string(),
        )
        .await;
        let deploy_root = std::env::temp_dir().join("docket-core-updater-run-test-known-bad-tag");
        std::fs::create_dir_all(&deploy_root).unwrap();
        std::fs::write(known_bad_tag_path(&deploy_root), "v0.2.0\n").unwrap();

        let outcome = run(&ctx(&status_url, &api_base, &deploy_root))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            Outcome::SkippedKnownBad {
                tag: "v0.2.0".to_string()
            }
        );

        std::fs::remove_dir_all(&deploy_root).unwrap();
    }

    #[tokio::test]
    async fn missing_exe_at_deploy_root_is_an_error_not_a_fresh_install() {
        let (status_url, api_base) = serve_status_then_release(
            r#"{"version":"v0.1.0","idle_seconds":10000}"#.to_string(),
            r#"{"tag_name":"v0.2.0","assets":[]}"#.to_string(),
        )
        .await;
        // Exists but deliberately has no docket-core(.exe) in it — the
        // scenario an unset DOCKET_UPDATER_DEPLOY_DIR (defaulting to ".")
        // produces on a real Scheduled Task.
        let deploy_root = std::env::temp_dir().join("docket-core-updater-run-test-missing-exe");
        std::fs::create_dir_all(&deploy_root).unwrap();

        let result = run(&ctx(&status_url, &api_base, &deploy_root)).await;
        assert!(result.is_err());
        assert!(
            !deploy_root.join(EXE_NAME).exists(),
            "run() must not write into an unconfigured deploy_root"
        );

        std::fs::remove_dir_all(&deploy_root).unwrap();
    }
}
