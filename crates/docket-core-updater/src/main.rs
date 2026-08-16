//! Single-shot "check GitHub Releases → maybe update docket-core" pass,
//! run once per invocation. The 15-minute repeat comes from a Windows
//! Scheduled Task registered outside this crate (docket-works-private
//! install script, design §4) — this binary does not loop or sleep
//! between checks itself.

const IDLE_THRESHOLD_SECS: u64 = 300;
const TASK_NAME: &str = "docket-core";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base =
        std::env::var("DOCKET_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:6500".to_string());
    let status_url = format!("{base}/status");
    let smoke_url = format!("{base}/items");
    let api_base = std::env::var("DOCKET_UPDATER_GITHUB_API_BASE")
        .unwrap_or_else(|_| "https://api.github.com".to_string());
    let deploy_root =
        std::env::var("DOCKET_UPDATER_DEPLOY_DIR").unwrap_or_else(|_| ".".to_string());

    let ctx = docket_core_updater::UpdateContext {
        status_url: &status_url,
        smoke_url: &smoke_url,
        api_base: &api_base,
        idle_threshold_secs: IDLE_THRESHOLD_SECS,
        deploy_root: std::path::Path::new(&deploy_root),
        task_name: TASK_NAME,
    };

    match docket_core_updater::run(&ctx).await? {
        docket_core_updater::Outcome::UpToDate => {
            println!("docket-core-updater: already up to date");
        }
        docket_core_updater::Outcome::NotIdleEnough { idle_seconds } => {
            println!(
                "docket-core-updater: update available but only idle {idle_seconds}s (need {IDLE_THRESHOLD_SECS}s), skipping"
            );
        }
        docket_core_updater::Outcome::Updated { from, to } => {
            println!("docket-core-updater: updated {from} -> {to}");
        }
        docket_core_updater::Outcome::RolledBack { attempted, reason } => {
            eprintln!(
                "docket-core-updater: update to {attempted} failed smoke check ({reason}), rolled back"
            );
        }
    }
    Ok(())
}
