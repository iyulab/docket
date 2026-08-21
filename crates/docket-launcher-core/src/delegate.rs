//! Runs the resolved worker binary as a child process with this process's
//! stdin/stdout/stderr inherited, with no buffering or proxying in between
//! (`Command::status()` inherits stdio by default when it isn't overridden
//! with `.stdin()`/`.stdout()`). Two different launchers depend on this:
//! `docket-mcp`'s stdio-based MCP protocol needs the exact same streams the
//! MCP client connected to the launcher, and `docket-cc`'s `hook` subcommand
//! needs its plain-text summary to reach the `SessionStart` hook caller's
//! stdout unbuffered.

use std::path::Path;
use std::process::Command;

/// Spawns `binary` with `args`, inheriting stdio, waits for it to exit, and
/// returns its exit code (or 1 if it was killed by a signal on Unix, which
/// has no exit code — matches conventional shell behavior).
pub fn run(binary: &Path, args: &[&str]) -> anyhow::Result<i32> {
    run_with_env(binary, args, &[])
}

/// Same as `run`, but with `extra_env` set on the child in addition to this
/// process's own inherited environment — used to hand a launcher-only signal
/// (e.g. "a newer release was just cached") to the worker it is about to
/// exec without touching `run`'s existing callers.
pub fn run_with_env(
    binary: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> anyhow::Result<i32> {
    let mut command = Command::new(binary);
    command.args(args);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let status = command.status()?;
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

    #[test]
    fn extra_env_reaches_the_child() {
        #[cfg(windows)]
        let (bin, args): (&str, &[&str]) = (
            "cmd.exe",
            &[
                "/c",
                "if \"%DOCKET_TEST_VAR%\"==\"present\" (exit 5) else (exit 6)",
            ],
        );
        #[cfg(not(windows))]
        let (bin, args): (&str, &[&str]) = (
            "sh",
            &[
                "-c",
                "[ \"$DOCKET_TEST_VAR\" = present ] && exit 5 || exit 6",
            ],
        );

        let code = run_with_env(Path::new(bin), args, &[("DOCKET_TEST_VAR", "present")]).unwrap();
        assert_eq!(code, 5);
    }
}
