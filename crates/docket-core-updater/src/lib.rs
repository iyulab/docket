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

use std::path::Path;

const USER_AGENT: &str = "docket-core-updater";
const OWNER: &str = "iyulab";
const REPO: &str = "docket";

#[cfg(windows)]
const EXE_NAME: &str = "docket-core.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "docket-core";

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
    NotIdleEnough { idle_seconds: u64 },
    Updated { from: String, to: String },
    RolledBack { attempted: String, reason: String },
}

/// Runs one check: fetch local `/status` + the latest GitHub release, and
/// either skip (already current, or not idle long enough) or download,
/// verify, swap, restart, and smoke-test — rolling back and restarting on
/// a failed smoke test rather than leaving `docket-core` down.
pub async fn run(ctx: &UpdateContext<'_>) -> anyhow::Result<Outcome> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
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

    let paths = deploy::DeployPaths::under(ctx.deploy_root, EXE_NAME);
    task_control::stop(ctx.task_name)?;
    deploy::install(&paths, &exe_bytes, &zip_bytes)?;
    task_control::start(ctx.task_name)?;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    match smoke::check(&client, ctx.smoke_url).await {
        Ok(()) => Ok(Outcome::Updated {
            from: local.version,
            to: release.tag_name,
        }),
        Err(e) => {
            // The new binary may still be alive (e.g. returning 500s rather
            // than having crashed) — Windows locks a running exe's file, so
            // deploy::rollback's remove_file/rename would fail if the
            // scheduled task isn't stopped first. Tolerate stop() failing
            // here (the process may have already died on its own, which is
            // also a valid reason smoke failed) but always attempt it
            // before touching the filesystem.
            let _ = task_control::stop(ctx.task_name);
            deploy::rollback(&paths)?;
            task_control::start(ctx.task_name)?;
            Ok(Outcome::RolledBack {
                attempted: release.tag_name,
                reason: e.to_string(),
            })
        }
    }
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
}
