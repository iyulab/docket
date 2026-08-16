//! Backs up the currently-deployed `docket-core` binary and console
//! directory to `.prev`/`.prev` siblings, installs the new ones, and can
//! roll both back to the pre-update state on demand. Every operation here
//! is plain filesystem I/O — no network, no Windows Scheduled Task
//! control (that's `task_control`, Task 6) — so it's fully testable with
//! temp directories on any platform.

use std::path::{Path, PathBuf};

pub struct DeployPaths {
    pub exe: PathBuf,
    pub exe_prev: PathBuf,
    pub console_dir: PathBuf,
    pub console_dir_prev: PathBuf,
}

impl DeployPaths {
    pub fn under(root: &Path, exe_name: &str) -> Self {
        Self {
            exe: root.join(exe_name),
            exe_prev: root.join(format!("{exe_name}.prev")),
            console_dir: root.join("console").join("dist"),
            console_dir_prev: root.join("console").join("dist.prev"),
        }
    }
}

/// Backs up the current `docket-core` binary and console directory (if
/// present — a first-ever run of the updater might not have a pre-existing
/// `.prev` to keep, but in practice `docket-core` is always already
/// deployed before the updater's scheduled task is registered), then
/// installs `exe_bytes`/`console_zip_bytes` in their place. Only the most
/// recent backup is kept — an existing `.prev` is discarded before this
/// run's backup replaces it.
pub fn install(
    paths: &DeployPaths,
    exe_bytes: &[u8],
    console_zip_bytes: &[u8],
) -> anyhow::Result<()> {
    if paths.exe_prev.exists() {
        std::fs::remove_file(&paths.exe_prev)?;
    }
    if paths.exe.exists() {
        std::fs::rename(&paths.exe, &paths.exe_prev)?;
    }
    std::fs::write(&paths.exe, exe_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&paths.exe, std::fs::Permissions::from_mode(0o755))?;
    }

    if paths.console_dir_prev.exists() {
        std::fs::remove_dir_all(&paths.console_dir_prev)?;
    }
    if paths.console_dir.exists() {
        std::fs::rename(&paths.console_dir, &paths.console_dir_prev)?;
    }
    std::fs::create_dir_all(&paths.console_dir)?;
    extract_zip(console_zip_bytes, &paths.console_dir)?;

    Ok(())
}

/// Restores the `.prev` backup `install` made, undoing it. Errors if
/// there's no `exe_prev` backup to restore from — that means `install`
/// never ran (or already rolled back), and rolling back twice would
/// silently discard the newly-installed version instead of the backup.
pub fn rollback(paths: &DeployPaths) -> anyhow::Result<()> {
    if !paths.exe_prev.exists() {
        anyhow::bail!("no backup at {} to roll back to", paths.exe_prev.display());
    }
    if paths.exe.exists() {
        std::fs::remove_file(&paths.exe)?;
    }
    std::fs::rename(&paths.exe_prev, &paths.exe)?;

    if paths.console_dir_prev.exists() {
        if paths.console_dir.exists() {
            std::fs::remove_dir_all(&paths.console_dir)?;
        }
        std::fs::rename(&paths.console_dir_prev, &paths.console_dir)?;
    }
    Ok(())
}

/// Extracts `zip_bytes` into `dest`, rejecting any entry whose path would
/// escape `dest` (zip-slip) — `enclosed_name()` returns `None` for those.
fn extract_zip(zip_bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    let reader = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader)?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(relative) = file.enclosed_name() else {
            anyhow::bail!("zip entry {} has an unsafe path", file.name());
        };
        let out_path = dest.join(relative);
        if file.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut file, &mut out_file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "docket-core-updater-deploy-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// Builds a minimal valid zip (one file, `marker.txt`) in memory, using
    /// the same `zip` crate this module extracts with — round-tripping
    /// through the real format instead of hand-crafting bytes.
    fn make_zip(entry_name: &str, contents: &[u8]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            writer.start_file(entry_name, options).unwrap();
            writer.write_all(contents).unwrap();
            writer.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn install_writes_exe_and_extracts_console_zip_from_scratch() {
        let root = temp_root("fresh-install");
        std::fs::create_dir_all(&root).unwrap();
        let paths = DeployPaths::under(&root, "docket-core.exe");
        let zip_bytes = make_zip("index.html", b"<html>marker</html>");

        install(&paths, b"new exe bytes", &zip_bytes).unwrap();

        assert_eq!(std::fs::read(&paths.exe).unwrap(), b"new exe bytes");
        assert!(!paths.exe_prev.exists());
        assert_eq!(
            std::fs::read(paths.console_dir.join("index.html")).unwrap(),
            b"<html>marker</html>"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn install_backs_up_the_previous_exe_and_console_dir() {
        let root = temp_root("backup-then-install");
        std::fs::create_dir_all(&root).unwrap();
        let paths = DeployPaths::under(&root, "docket-core.exe");
        std::fs::write(&paths.exe, b"old exe bytes").unwrap();
        std::fs::create_dir_all(&paths.console_dir).unwrap();
        std::fs::write(paths.console_dir.join("index.html"), b"old console").unwrap();

        let zip_bytes = make_zip("index.html", b"new console");
        install(&paths, b"new exe bytes", &zip_bytes).unwrap();

        assert_eq!(std::fs::read(&paths.exe).unwrap(), b"new exe bytes");
        assert_eq!(std::fs::read(&paths.exe_prev).unwrap(), b"old exe bytes");
        assert_eq!(
            std::fs::read(paths.console_dir.join("index.html")).unwrap(),
            b"new console"
        );
        assert_eq!(
            std::fs::read(paths.console_dir_prev.join("index.html")).unwrap(),
            b"old console"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rollback_restores_the_backed_up_version() {
        let root = temp_root("rollback");
        std::fs::create_dir_all(&root).unwrap();
        let paths = DeployPaths::under(&root, "docket-core.exe");
        std::fs::write(&paths.exe, b"old exe bytes").unwrap();
        std::fs::create_dir_all(&paths.console_dir).unwrap();
        std::fs::write(paths.console_dir.join("index.html"), b"old console").unwrap();

        let zip_bytes = make_zip("index.html", b"bad console");
        install(&paths, b"bad exe bytes", &zip_bytes).unwrap();
        rollback(&paths).unwrap();

        assert_eq!(std::fs::read(&paths.exe).unwrap(), b"old exe bytes");
        assert!(!paths.exe_prev.exists());
        assert_eq!(
            std::fs::read(paths.console_dir.join("index.html")).unwrap(),
            b"old console"
        );
        assert!(!paths.console_dir_prev.exists());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rollback_with_no_backup_is_an_error() {
        let root = temp_root("rollback-no-backup");
        std::fs::create_dir_all(&root).unwrap();
        let paths = DeployPaths::under(&root, "docket-core.exe");
        assert!(rollback(&paths).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn extract_zip_rejects_unsafe_paths() {
        let root = temp_root("zip-slip");
        std::fs::create_dir_all(&root).unwrap();
        let zip_bytes = make_zip("../../escape.txt", b"malicious");
        let result = extract_zip(&zip_bytes, &root);
        assert!(result.is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
