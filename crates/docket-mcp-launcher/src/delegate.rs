//! Runs the cached `docket-mcp` worker as a child process with this
//! process's stdin/stdout/stderr inherited — the MCP protocol is
//! stdio-based, so the worker must read/write the exact same streams the
//! MCP client connected to the launcher, with no buffering or proxying in
//! between (`Command::status()` inherits stdio by default when it isn't
//! overridden with `.stdin()`/`.stdout()`).

use std::path::Path;
use std::process::Command;

/// Spawns `binary` with `args`, inheriting stdio, waits for it to exit, and
/// returns its exit code (or 1 if it was killed by a signal on Unix, which
/// has no exit code — matches conventional shell behavior).
pub fn run(binary: &Path, args: &[&str]) -> anyhow::Result<i32> {
    let status = Command::new(binary).args(args).status()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagates_exit_code() {
        #[cfg(windows)]
        let (bin, args): (&str, &[&str]) = ("cmd.exe", &["/c", "exit 3"]);
        #[cfg(not(windows))]
        let (bin, args): (&str, &[&str]) = ("sh", &["-c", "exit 3"]);

        let code = run(Path::new(bin), args).unwrap();
        assert_eq!(code, 3);
    }

    #[test]
    fn propagates_success() {
        #[cfg(windows)]
        let (bin, args): (&str, &[&str]) = ("cmd.exe", &["/c", "exit 0"]);
        #[cfg(not(windows))]
        let (bin, args): (&str, &[&str]) = ("sh", &["-c", "exit 0"]);

        let code = run(Path::new(bin), args).unwrap();
        assert_eq!(code, 0);
    }
}
