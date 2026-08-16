//! Orchestrates a single check-and-maybe-update pass for `docket-core`:
//! fetch its local `/status`, compare against the latest GitHub release,
//! and (Task 6) download+verify+swap+restart+smoke, rolling back on smoke
//! failure. The 15-minute repeat comes from a Windows Scheduled Task
//! registered outside this crate (docket-works-private install script,
//! design §4) — this binary itself does not loop or sleep between checks.

pub mod decision;
pub mod status;
