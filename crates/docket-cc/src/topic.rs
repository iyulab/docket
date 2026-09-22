//! Derives a `topic` string from a local directory, so a caller never has
//! to invent that string by hand — and, more importantly, so the same
//! directory produces the same topic across sessions and machines. Without
//! a deterministic rule, two sessions working on what is structurally the
//! same repository (a plain clone here, a submodule of some umbrella
//! there) could each invent a different topic string, which would split
//! `claim` ownership across two candidate pools that can never see each
//! other — `claim` exclusivity only means anything within a single topic.
//!
//! Algorithm: the nearest `.git` above the given directory identifies the
//! repository, and its topic is that repository's `org/repo` (read from its
//! `origin` remote) — the whole repository is one topic by default, however
//! many packages or crates live inside it, matching how this repository's
//! own topics are already used in practice. A `.git` *file* (submodule or
//! worktree gitlink) is followed to the real git directory — including a
//! worktree's `commondir` indirection — so both submodules and worktrees
//! resolve to the same `org/repo` a plain clone of that repository would; a
//! submodule's own `.git` stops the walk at the submodule, so it never
//! inherits the umbrella repository's identity. A `.docket/topic` file
//! anywhere above the directory always wins over this derivation — the
//! escape hatch for the cases the algorithm gets wrong (no remote yet), and
//! the intended way to opt a specific directory into a finer-grained topic
//! than the repo-level default.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Where a derived topic actually came from. The distinction exists for
/// one of the three: `FolderName` is a **guess**, and the other two are
/// answers. Returning the topic alone threw that away at the one place
/// that knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicSource {
    /// An explicit `.docket/topic`. Whatever it says is intended.
    Override,
    /// Parsed from the repository's `origin` remote — stable across clones,
    /// machines, and however the directory happens to be named locally.
    OriginRemote,
    /// **Fallback**: no `.git` above the directory, or a `.git` with no
    /// `origin` remote. The topic is the directory's own name, which is not
    /// an identity — another clone of the same repository under a different
    /// directory name derives a different one, and a bare name with no
    /// `org/` scope is a *different identity* from the scoped spelling, not
    /// a shorthand for it. This is where a short-form spelling drift is
    /// created rather than merely detected.
    FolderName,
}

/// A derived topic and where it came from. See [`TopicSource`].
#[derive(Debug, Clone)]
pub struct DerivedTopic {
    pub topic: String,
    pub source: TopicSource,
}

/// The public entry point. Never fails — a directory with no `.git`
/// anywhere above it, or a `.git` with no `origin` remote, still produces a
/// usable (if less specific) topic rather than an error, since the caller
/// (a Claude Code session about to create or search for an item) has no
/// good recovery path for "topic derivation failed".
///
/// **The fallback is a guess, and the caller is told so** via
/// [`TopicSource`] — `collect_submodule_topics` below already refuses to
/// report a bare folder name on the grounds that it misleads more than
/// silence does, and the same is true here; the difference is that this
/// path has no silence available, so it reports the guess *and* labels it.
pub fn derive_topic_detailed(start: &Path) -> DerivedTopic {
    if let Some(topic) = find_topic_override(start) {
        return DerivedTopic {
            topic,
            source: TopicSource::Override,
        };
    }
    let (fallback_root, from_remote) = match find_repo_root(start) {
        Some((root, git_entry)) => {
            let derived = resolve_git_common_dir(&git_entry)
                .and_then(|common| remote_origin_org_repo(&common));
            (root, derived)
        }
        None => (start.to_path_buf(), None),
    };
    match from_remote {
        Some(topic) => DerivedTopic {
            topic,
            source: TopicSource::OriginRemote,
        },
        None => DerivedTopic {
            topic: folder_name(&fallback_root),
            source: TopicSource::FolderName,
        },
    }
}

/// [`derive_topic_detailed`] when only the string is wanted — the shape
/// every caller used before the source became worth knowing.
pub fn derive_topic(start: &Path) -> String {
    derive_topic_detailed(start).topic
}

/// `derive_topic`'s upward walk stops at the nearest `.git`, deliberately —
/// a submodule never inherits its umbrella's identity (see the module doc).
/// This is the other direction: `start`'s own topic, plus the topic of
/// every submodule nested anywhere underneath its repository root, read
/// straight from `.gitmodules` (recursing into a submodule's own
/// `.gitmodules` for an umbrella-of-umbrellas). A caller working across an
/// umbrella and its submodules otherwise has to enumerate and register every
/// sibling topic by hand, and a missed one goes silently unnoticed (a
/// `topic_scope`/`mine` filter just returns fewer rows, no error). Order:
/// `start`'s own topic first, then each submodule in `.gitmodules`
/// declaration order, depth-first. Deduplicated, so an override that
/// happens to collide with a submodule's derived topic isn't listed twice.
///
/// Returns the submodules it had to leave out alongside the topics — see
/// [`AllTopics::skipped`] for why that half is not an implementation
/// detail.
pub fn derive_all_topics_detailed(start: &Path) -> AllTopics {
    let mut topics = vec![derive_topic(start)];
    let mut skipped = Vec::new();
    if let Some((root, _)) = find_repo_root(start) {
        collect_submodule_topics(&root, &root, &mut topics, &mut skipped);
    }
    let mut seen = HashSet::new();
    topics.retain(|t| seen.insert(t.clone()));
    AllTopics { topics, skipped }
}

/// The result of [`derive_all_topics_detailed`].
#[derive(Debug, Clone)]
pub struct AllTopics {
    /// One topic per repository in the tree, `start`'s own first.
    pub topics: Vec<String>,
    /// Declared `.gitmodules` paths (relative to `start`'s repository root)
    /// that were **not** included, because nothing is checked out there.
    ///
    /// Skipping them is correct — an uninitialized submodule has no
    /// `origin` remote to derive from, and guessing a folder name would be
    /// the very thing [`TopicSource::FolderName`] exists to warn about. But
    /// the whole point of listing every topic at once is that a caller
    /// stops missing one by hand, and a silent skip has this function
    /// committing that omission on the caller's behalf: the output is
    /// indistinguishable from a complete answer, and it feeds straight into
    /// a worker registration, where a missing topic shows up only as a
    /// query quietly returning fewer rows.
    pub skipped: Vec<String>,
}

/// Reads `repo_root/.gitmodules` (absent → no submodules, not an error) and
/// appends each listed submodule's topic, recursing into any submodule that
/// is itself an umbrella. A submodule directory that doesn't actually exist
/// or isn't checked out (`.gitmodules` lists it, but `git submodule update`
/// was never run) is skipped rather than falling back to a folder-name
/// topic — an uninitialized submodule has no `origin` remote to derive from
/// and reporting a bare directory name here would be more misleading than
/// silence. Each such skip is recorded in `skipped` so the caller can say
/// so; skipping quietly would make this function's output indistinguishable
/// from a complete one (see [`AllTopics::skipped`]).
fn collect_submodule_topics(
    outer_root: &Path,
    repo_root: &Path,
    topics: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    let Ok(content) = std::fs::read_to_string(repo_root.join(".gitmodules")) else {
        return;
    };
    for path in gitmodule_paths(&content) {
        let submodule_dir = repo_root.join(&path);
        if submodule_dir.join(".git").exists() {
            topics.push(derive_topic(&submodule_dir));
            collect_submodule_topics(outer_root, &submodule_dir, topics, skipped);
        } else {
            // Relative to the outermost root, not to the umbrella that
            // declared it: a bare leaf name leaves the caller hunting for
            // which nested umbrella it belongs to, and nested umbrellas are
            // exactly why this function recurses.
            skipped.push(
                submodule_dir
                    .strip_prefix(outer_root)
                    .unwrap_or(&submodule_dir)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

/// A deliberately minimal `.gitmodules` reader — same "one thing, no
/// general-purpose config library" rationale as `parse_remote_origin_url`
/// below. Extracts every `path = ...` value regardless of which
/// `[submodule "name"]` section it sits under, since only the checkout path
/// (not the section name) is needed to locate each submodule on disk.
fn gitmodule_paths(content: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("path") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                paths.push(value.trim().to_string());
            }
        }
    }
    paths
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Walks from `start` up to the filesystem root looking for a `.docket/topic`
/// file. The first non-empty first line found wins — this is an explicit
/// per-directory opt-out of the rest of the algorithm, so it is checked
/// before any `.git` walk, not folded into it.
fn find_topic_override(start: &Path) -> Option<String> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".docket").join("topic");
        if let Ok(content) = std::fs::read_to_string(&candidate) {
            let first_line = content.lines().next().unwrap_or("").trim();
            if !first_line.is_empty() {
                return Some(first_line.to_string());
            }
        }
    }
    None
}

/// The nearest `.git` (file or directory) above `start`, and the directory
/// it was found in (the repository root). A submodule's own `.git` file
/// stops the walk at the submodule's root, not the umbrella repo's — that
/// is what makes a submodule self-isolate to its own `org/repo` topic.
fn find_repo_root(start: &Path) -> Option<(PathBuf, PathBuf)> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".git");
        if candidate.exists() {
            return Some((ancestor.to_path_buf(), candidate));
        }
    }
    None
}

/// Resolves a `.git` entry (directory, or a submodule/worktree gitlink
/// file) to the directory that actually holds `config` — following a
/// worktree's `commondir` indirection when present, since a worktree's own
/// gitdir under `.git/worktrees/<name>/` has no `config` of its own.
fn resolve_git_common_dir(git_entry: &Path) -> Option<PathBuf> {
    let mut dir = if git_entry.is_dir() {
        git_entry.to_path_buf()
    } else {
        let content = std::fs::read_to_string(git_entry).ok()?;
        let target = content
            .lines()
            .find_map(|l| l.trim().strip_prefix("gitdir:"))?
            .trim();
        let target_path = PathBuf::from(target);
        if target_path.is_absolute() {
            target_path
        } else {
            git_entry.parent()?.join(target_path)
        }
    };
    if let Ok(commondir_content) = std::fs::read_to_string(dir.join("commondir")) {
        let common = PathBuf::from(commondir_content.trim());
        dir = if common.is_absolute() {
            common
        } else {
            dir.join(common)
        };
    }
    Some(dir)
}

fn remote_origin_org_repo(git_common_dir: &Path) -> Option<String> {
    let config = std::fs::read_to_string(git_common_dir.join("config")).ok()?;
    org_repo_from_url(&parse_remote_origin_url(&config)?)
}

/// A deliberately minimal `[remote "origin"] url = ...` reader rather than
/// a full git-config parser or a `git2`/libgit2 dependency — the only thing
/// ever extracted from this file is one URL, so a small, dependency-free
/// scan matches principles.md "simplicity > reliability > scalability"
/// better than a general-purpose config library would.
fn parse_remote_origin_url(config: &str) -> Option<String> {
    let mut in_origin_section = false;
    for raw_line in config.lines() {
        let line = raw_line.trim();
        if line.starts_with('[') {
            in_origin_section = line.eq_ignore_ascii_case(r#"[remote "origin"]"#);
            continue;
        }
        if in_origin_section && let Some(rest) = line.strip_prefix("url") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Reduces any remote URL form (`https://host/org/repo.git`,
/// `git@host:org/repo.git`, `ssh://git@host/org/repo.git`, with or without
/// a trailing `.git`) to `org/repo` by taking the last two non-empty
/// `/`-or-`:`-separated segments. Host-agnostic on purpose — `org/repo` is
/// derived from remote *structure*, not from any particular forge.
fn org_repo_from_url(url: &str) -> Option<String> {
    let normalized = url.replace(':', "/");
    let mut segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }
    let mut repo = segments.pop().unwrap().to_string();
    if let Some(stripped) = repo.strip_suffix(".git") {
        repo = stripped.to_string();
    }
    let org = segments.pop().unwrap();
    if repo.is_empty() || org.is_empty() {
        return None;
    }
    Some(format!("{org}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "docket-cc-topic-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_config_with_origin(git_dir: &Path, url: &str) {
        std::fs::create_dir_all(git_dir).unwrap();
        std::fs::write(
            git_dir.join("config"),
            format!("[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = {url}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n"),
        )
        .unwrap();
    }

    /// A folder name is not an identity. Two clones of one repository under
    /// different directory names derive different topics, and a topic with
    /// no `org/` scope is a different identity from the scoped one -- which
    /// is how a short-form spelling drift gets created rather than merely
    /// detected. The fallback stays (a caller has no recovery path for
    /// "derivation failed"), but it stops being silent.
    #[test]
    fn a_repo_without_an_origin_remote_reports_the_folder_name_fallback() {
        let root = temp_dir("no-origin");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".git").join("config"),
            "[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();

        let derived = derive_topic_detailed(&root);
        assert_eq!(derived.topic, folder_name(&root));
        assert_eq!(derived.source, TopicSource::FolderName);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_directory_with_no_git_at_all_reports_the_folder_name_fallback() {
        let dir = temp_dir("no-git");
        let derived = derive_topic_detailed(&dir);
        assert_eq!(derived.topic, folder_name(&dir));
        assert_eq!(derived.source, TopicSource::FolderName);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_derived_org_repo_is_not_reported_as_a_fallback() {
        let root = temp_dir("has-origin");
        write_config_with_origin(&root.join(".git"), "https://github.com/acme/widget.git");

        let derived = derive_topic_detailed(&root);
        assert_eq!(derived.topic, "acme/widget");
        assert_eq!(derived.source, TopicSource::OriginRemote);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An explicit `.docket/topic` is the documented escape hatch, so it is
    /// never a fallback however it is spelled -- warning there would be
    /// telling the caller off for doing exactly what the warning asks for.
    #[test]
    fn an_explicit_override_is_never_reported_as_a_fallback() {
        let root = temp_dir("override");
        std::fs::create_dir_all(root.join(".docket")).unwrap();
        std::fs::write(root.join(".docket").join("topic"), "widget\n").unwrap();

        let derived = derive_topic_detailed(&root);
        assert_eq!(derived.topic, "widget");
        assert_eq!(derived.source, TopicSource::Override);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn org_repo_parses_https_ssh_and_scp_style_urls() {
        assert_eq!(
            org_repo_from_url("https://github.com/iyulab/docket.git"),
            Some("iyulab/docket".to_string())
        );
        assert_eq!(
            org_repo_from_url("https://github.com/iyulab/docket"),
            Some("iyulab/docket".to_string())
        );
        assert_eq!(
            org_repo_from_url("git@github.com:iyulab/docket.git"),
            Some("iyulab/docket".to_string())
        );
        assert_eq!(
            org_repo_from_url("ssh://git@github.com/iyulab/docket.git"),
            Some("iyulab/docket".to_string())
        );
    }

    #[test]
    fn org_repo_is_none_for_a_url_with_no_org_segment() {
        assert_eq!(org_repo_from_url("docket.git"), None);
    }

    #[test]
    fn repo_root_with_remote_derives_org_repo() {
        let root = temp_dir("repo-root");
        write_config_with_origin(&root.join(".git"), "https://github.com/iyulab/docket.git");

        assert_eq!(derive_topic(&root), "iyulab/docket");

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The whole repository is one topic by default — a package inside it
    /// resolves to the same topic as the repository root, not a sub-topic
    /// of its own. Matches how this project's own topics are already used
    /// (e.g. every crate in this repository shares one topic).
    #[test]
    fn subdir_resolves_to_the_same_repo_level_topic_as_the_root() {
        let root = temp_dir("monorepo-root");
        write_config_with_origin(&root.join(".git"), "https://github.com/iyulab/docket.git");
        let subdir = root.join("crates").join("docket-console");

        assert_eq!(derive_topic(&subdir), "iyulab/docket");
        assert_eq!(derive_topic(&subdir), derive_topic(&root));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A submodule's own `.git` *file* (a gitlink, not a directory) must
    /// stop the upward walk at the submodule itself — resolving through
    /// `gitdir:` to the submodule's own remote, not the umbrella's — which
    /// is what lets `iyulab/docket` used as a submodule converge on the
    /// same topic as a plain clone of `iyulab/docket`.
    #[test]
    fn submodule_gitlink_resolves_to_its_own_remote_not_the_umbrella() {
        let umbrella = temp_dir("umbrella-root");
        write_config_with_origin(
            &umbrella.join(".git"),
            "https://github.com/someone/umbrella.git",
        );
        let submodule_dir = umbrella.join("docket");
        std::fs::create_dir_all(&submodule_dir).unwrap();
        std::fs::write(
            submodule_dir.join(".git"),
            "gitdir: ../.git/modules/docket\n",
        )
        .unwrap();
        write_config_with_origin(
            &umbrella.join(".git").join("modules").join("docket"),
            "https://github.com/iyulab/docket.git",
        );

        assert_eq!(derive_topic(&submodule_dir), "iyulab/docket");

        std::fs::remove_dir_all(&umbrella).unwrap();
    }

    /// A worktree's gitdir (`.git/worktrees/<name>/`) has no `config` of
    /// its own — only the main repo's common dir does — so resolution must
    /// follow `commondir` one hop further before it can find the remote.
    #[test]
    fn worktree_commondir_resolves_to_the_main_repos_remote() {
        let main_repo = temp_dir("worktree-main-repo");
        write_config_with_origin(
            &main_repo.join(".git"),
            "https://github.com/iyulab/docket.git",
        );
        let worktree_gitdir = main_repo.join(".git").join("worktrees").join("feature-x");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::write(worktree_gitdir.join("commondir"), "../..\n").unwrap();

        let worktree_dir = temp_dir("worktree-checkout");
        std::fs::write(
            worktree_dir.join(".git"),
            format!("gitdir: {}\n", worktree_gitdir.display()),
        )
        .unwrap();

        assert_eq!(derive_topic(&worktree_dir), "iyulab/docket");

        std::fs::remove_dir_all(&main_repo).unwrap();
        std::fs::remove_dir_all(&worktree_dir).unwrap();
    }

    #[test]
    fn docket_topic_override_wins_over_git_derivation() {
        let root = temp_dir("override-root");
        write_config_with_origin(&root.join(".git"), "https://github.com/iyulab/docket.git");
        std::fs::create_dir_all(root.join(".docket")).unwrap();
        std::fs::write(root.join(".docket").join("topic"), "custom/topic-name\n").unwrap();

        assert_eq!(derive_topic(&root), "custom/topic-name");
        // The override must win from a subdirectory too, not only the exact
        // directory the file lives in.
        let subdir = root.join("nested");
        assert_eq!(derive_topic(&subdir), "custom/topic-name");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn git_dir_with_no_origin_remote_falls_back_to_folder_name() {
        let root = temp_dir("no-remote-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".git").join("config"),
            "[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();

        assert_eq!(derive_topic(&root), folder_name(&root));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The umbrella+submodule shape a caller can be in — walking from the
    /// umbrella root must list both the umbrella's own topic and the
    /// submodule's, in `.gitmodules` order.
    #[test]
    fn derive_all_topics_lists_the_umbrella_and_its_submodule() {
        let umbrella = temp_dir("all-topics-umbrella");
        write_config_with_origin(
            &umbrella.join(".git"),
            "https://github.com/acme/umbrella.git",
        );
        std::fs::write(
            umbrella.join(".gitmodules"),
            "[submodule \"docket\"]\n\tpath = docket\n\turl = https://github.com/iyulab/docket.git\n",
        )
        .unwrap();
        let submodule_dir = umbrella.join("docket");
        std::fs::create_dir_all(&submodule_dir).unwrap();
        std::fs::write(
            submodule_dir.join(".git"),
            "gitdir: ../.git/modules/docket\n",
        )
        .unwrap();
        write_config_with_origin(
            &umbrella.join(".git").join("modules").join("docket"),
            "https://github.com/iyulab/docket.git",
        );

        assert_eq!(
            derive_all_topics_detailed(&umbrella).topics,
            vec!["acme/umbrella".to_string(), "iyulab/docket".to_string(),]
        );

        std::fs::remove_dir_all(&umbrella).unwrap();
    }

    /// A submodule listed in `.gitmodules` but never actually checked out
    /// (no `git submodule update`) has nothing to derive a topic from —
    /// must be skipped, not reported under a misleading folder-name
    /// fallback.
    #[test]
    fn derive_all_topics_skips_an_uninitialized_submodule() {
        let umbrella = temp_dir("all-topics-uninit");
        write_config_with_origin(
            &umbrella.join(".git"),
            "https://github.com/acme/umbrella.git",
        );
        std::fs::write(
            umbrella.join(".gitmodules"),
            "[submodule \"docket\"]\n\tpath = docket\n\turl = https://github.com/iyulab/docket.git\n",
        )
        .unwrap();
        // Note: no `docket/` directory created at all — an uninitialized
        // submodule reference with nothing checked out on disk.

        assert_eq!(
            derive_all_topics_detailed(&umbrella).topics,
            vec!["acme/umbrella".to_string()]
        );

        std::fs::remove_dir_all(&umbrella).unwrap();
    }

    /// Skipping is the right call -- an uninitialized submodule has no
    /// remote to derive from -- but `--all` exists precisely so a caller
    /// stops missing topics, so a silent skip has the command committing
    /// the very omission it was added to prevent. The skip stays; it stops
    /// being invisible.
    #[test]
    fn derive_all_topics_reports_which_submodules_it_skipped() {
        let umbrella = temp_dir("all-topics-skips-reported");
        write_config_with_origin(
            &umbrella.join(".git"),
            "https://github.com/acme/umbrella.git",
        );
        std::fs::write(
            umbrella.join(".gitmodules"),
            "[submodule \"widget\"]\n\tpath = widget\n\turl = https://github.com/acme/widget.git\n",
        )
        .unwrap();

        let all = derive_all_topics_detailed(&umbrella);
        assert_eq!(all.topics, vec!["acme/umbrella".to_string()]);
        assert_eq!(
            all.skipped,
            vec!["widget".to_string()],
            "the declared path of the submodule that was not checked out"
        );

        std::fs::remove_dir_all(&umbrella).unwrap();
    }

    #[test]
    fn derive_all_topics_reports_nothing_skipped_when_every_submodule_is_checked_out() {
        let umbrella = temp_dir("all-topics-none-skipped");
        write_config_with_origin(
            &umbrella.join(".git"),
            "https://github.com/acme/umbrella.git",
        );
        std::fs::write(
            umbrella.join(".gitmodules"),
            "[submodule \"widget\"]\n\tpath = widget\n\turl = https://github.com/acme/widget.git\n",
        )
        .unwrap();
        write_config_with_origin(
            &umbrella.join("widget").join(".git"),
            "https://github.com/acme/widget.git",
        );

        let all = derive_all_topics_detailed(&umbrella);
        assert_eq!(
            all.topics,
            vec!["acme/umbrella".to_string(), "acme/widget".to_string()]
        );
        assert!(all.skipped.is_empty());

        std::fs::remove_dir_all(&umbrella).unwrap();
    }

    /// A skip inside a nested umbrella is just as invisible as one at the
    /// top, and the path has to say *where* -- a bare leaf name would leave
    /// the caller hunting for which umbrella it belongs to.
    #[test]
    fn derive_all_topics_reports_a_skip_nested_inside_a_submodule_with_its_full_path() {
        let root = temp_dir("all-topics-nested-skip");
        write_config_with_origin(&root.join(".git"), "https://github.com/acme/outer.git");
        std::fs::write(
            root.join(".gitmodules"),
            "[submodule \"mid\"]\n\tpath = mid\n\turl = https://github.com/acme/mid.git\n",
        )
        .unwrap();
        write_config_with_origin(
            &root.join("mid").join(".git"),
            "https://github.com/acme/mid.git",
        );
        std::fs::write(
            root.join("mid").join(".gitmodules"),
            "[submodule \"leaf\"]\n\tpath = leaf\n\turl = https://github.com/acme/leaf.git\n",
        )
        .unwrap();

        let all = derive_all_topics_detailed(&root);
        assert_eq!(
            all.topics,
            vec!["acme/outer".to_string(), "acme/mid".to_string()]
        );
        assert_eq!(
            all.skipped,
            vec![format!("mid{}leaf", std::path::MAIN_SEPARATOR)]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Nested umbrellas (a submodule that is itself an umbrella) must
    /// recurse — the whole point of walking `.gitmodules` rather than
    /// listing only the immediate children.
    #[test]
    fn derive_all_topics_recurses_into_a_nested_umbrella() {
        let root = temp_dir("all-topics-nested");
        write_config_with_origin(&root.join(".git"), "https://github.com/org/root.git");
        std::fs::write(
            root.join(".gitmodules"),
            "[submodule \"mid\"]\n\tpath = mid\n\turl = https://github.com/org/mid.git\n",
        )
        .unwrap();
        let mid_dir = root.join("mid");
        std::fs::create_dir_all(&mid_dir).unwrap();
        std::fs::write(mid_dir.join(".git"), "gitdir: ../.git/modules/mid\n").unwrap();
        write_config_with_origin(
            &root.join(".git").join("modules").join("mid"),
            "https://github.com/org/mid.git",
        );
        std::fs::write(
            mid_dir.join(".gitmodules"),
            "[submodule \"leaf\"]\n\tpath = leaf\n\turl = https://github.com/org/leaf.git\n",
        )
        .unwrap();
        let leaf_dir = mid_dir.join("leaf");
        std::fs::create_dir_all(&leaf_dir).unwrap();
        std::fs::write(
            leaf_dir.join(".git"),
            "gitdir: ../../.git/modules/mid/modules/leaf\n",
        )
        .unwrap();
        write_config_with_origin(
            &root
                .join(".git")
                .join("modules")
                .join("mid")
                .join("modules")
                .join("leaf"),
            "https://github.com/org/leaf.git",
        );

        assert_eq!(
            derive_all_topics_detailed(&root).topics,
            vec![
                "org/root".to_string(),
                "org/mid".to_string(),
                "org/leaf".to_string(),
            ]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn no_git_anywhere_falls_back_to_the_directorys_own_name() {
        // std::env::temp_dir() is outside any git checkout on every platform
        // this project targets, so no ancestor of this directory has a
        // `.git` — the same assumption docket-core's own temp-dir test
        // helpers already rely on.
        let dir = temp_dir("no-git-anywhere");

        assert_eq!(derive_topic(&dir), folder_name(&dir));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
