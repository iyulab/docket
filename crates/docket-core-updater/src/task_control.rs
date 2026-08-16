//! Controls the Windows Scheduled Task that runs `docket-core` on the
//! deployment host (registered once by a docket-works-private install
//! script, design §4) — stopping/restarting the task, not the process
//! directly, matches how the existing manual publish path already
//! manages `docket-core`'s lifecycle.
//!
//! Windows-only: `docket-core-updater` only ever runs on the Windows
//! deployment host this scheduled task exists on. Not covered by
//! automated tests — there's no scheduled task to control in CI, and the
//! design (§7) already scopes the stop→swap→restart→rollback sequence to
//! manual E2E verification on the deployment host instead.

use std::process::Command;

#[cfg(windows)]
pub fn stop(task_name: &str) -> anyhow::Result<()> {
    let status = Command::new("schtasks")
        .args(["/End", "/TN", task_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("schtasks /End /TN {task_name} exited with {status}");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn stop(_task_name: &str) -> anyhow::Result<()> {
    anyhow::bail!("docket-core-updater's scheduled-task control is Windows-only")
}

#[cfg(windows)]
pub fn start(task_name: &str) -> anyhow::Result<()> {
    let status = Command::new("schtasks")
        .args(["/Run", "/TN", task_name])
        .status()?;
    if !status.success() {
        anyhow::bail!("schtasks /Run /TN {task_name} exited with {status}");
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn start(_task_name: &str) -> anyhow::Result<()> {
    anyhow::bail!("docket-core-updater's scheduled-task control is Windows-only")
}
