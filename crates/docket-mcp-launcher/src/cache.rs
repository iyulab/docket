//! Manages the local cache of downloaded `docket-mcp` worker binaries under
//! `~/.docket/cache/mcp/<version>/`. Each release version gets its own
//! directory, so "is version X cached" is just "does that path exist" — no
//! separate version-tracking file needed. `~/.docket/` is the same
//! user-data root ADR-0008 established for `docket-cc`'s file projections;
//! this uses a disjoint `cache/mcp/` subtree under it.

use std::path::{Path, PathBuf};

pub fn cache_root() -> anyhow::Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("no user data directory available on this platform"))?;
    Ok(base.join("docket").join("cache").join("mcp"))
}

/// Path a cached worker binary for `version` would live at (may not exist).
pub fn cached_binary_path(cache_root: &Path, version: &str, binary_ext: &str) -> PathBuf {
    cache_root
        .join(version)
        .join(format!("docket-mcp{binary_ext}"))
}

/// Writes `bytes` (already checksum-verified by the caller) into the cache
/// for `version`, atomically — a sibling temp file is renamed into place
/// only after the full write succeeds, so a partially-written file is never
/// visible at `cached_binary_path`'s location (same pattern as docket-cc's
/// `write_item_file`).
pub fn store(
    cache_root: &Path,
    version: &str,
    binary_ext: &str,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    let dir = cache_root.join(version);
    std::fs::create_dir_all(&dir)?;
    let final_path = cached_binary_path(cache_root, version, binary_ext);
    let tmp_path = dir.join(format!(
        ".docket-mcp{binary_ext}.tmp-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

/// The most recently cached worker binary, if any (by directory mtime) —
/// used when a fresh update check fails and there's nothing better to do
/// than fall back to whatever is already on disk.
pub fn latest_cached(cache_root: &Path, binary_ext: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(cache_root).ok()?;
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            let binary = e.path().join(format!("docket-mcp{binary_ext}"));
            binary.exists().then_some((mtime, binary))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "docket-launcher-cache-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn uncached_version_path_does_not_exist() {
        let root = temp_root();
        assert!(!cached_binary_path(&root, "v0.2.0", "").exists());
    }

    #[test]
    fn store_then_cached_binary_path_has_the_right_content_and_no_tmp_residue() {
        let root = temp_root();
        let path = store(&root, "v0.2.0", "", b"fake binary contents").unwrap();
        assert_eq!(path, cached_binary_path(&root, "v0.2.0", ""));
        assert_eq!(std::fs::read(&path).unwrap(), b"fake binary contents");

        let leftovers: Vec<_> = std::fs::read_dir(root.join("v0.2.0"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn different_versions_do_not_collide() {
        let root = temp_root();
        store(&root, "v0.1.0", "", b"old").unwrap();
        store(&root, "v0.2.0", "", b"new").unwrap();
        assert_eq!(
            std::fs::read(cached_binary_path(&root, "v0.1.0", "")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(cached_binary_path(&root, "v0.2.0", "")).unwrap(),
            b"new"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn latest_cached_is_none_when_nothing_cached() {
        let root = temp_root();
        assert_eq!(latest_cached(&root, ""), None);
    }

    #[test]
    fn latest_cached_picks_the_most_recently_written_version() {
        let root = temp_root();
        store(&root, "v0.1.0", "", b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        store(&root, "v0.2.0", "", b"new").unwrap();
        assert_eq!(
            latest_cached(&root, ""),
            Some(cached_binary_path(&root, "v0.2.0", ""))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
