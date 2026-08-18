//! Background check for a newer release than the one this process resolved
//! at startup. `resolve_and_run` already checks GitHub Releases on every
//! *invocation* — but the worker it execs (`docket-mcp`'s MCP server, in
//! particular) is a long-lived process once a client connects to it over
//! stdio, so "every invocation" only helps a *new* session. A session that
//! is already running never re-resolves its own version, and a release
//! landing mid-session goes unnoticed with no error — the tool calls just
//! keep working against whatever schema this process already has.
//!
//! This module mitigates that blind spot from the launcher side: it stays
//! alive for the life of the process (the launcher blocks on the worker's
//! exit via `delegate::run`, so a background task here runs for exactly as
//! long as the session does) and periodically re-checks whether a newer
//! release has appeared, printing one stderr warning the first time it has.
//! Diagnostics only — never affects the worker's behavior or exit code, and
//! a failed check is silent (matches `resolve_worker_binary`'s "a broken
//! update check is not a reason to fail the run" philosophy).

use std::time::Duration;

use crate::release_client;

/// Checks once whether GitHub's latest release differs from
/// `current_version`. `None` means either "still current" or "the check
/// itself failed" — both are silent by design, since a periodic background
/// check has no legitimate way to surface a hard error.
async fn check_once(
    client: &reqwest::Client,
    api_base: &str,
    owner: &str,
    repo: &str,
    worker_name: &str,
    current_version: &str,
) -> Option<String> {
    let release = release_client::latest_release(client, api_base, owner, repo)
        .await
        .ok()?;
    (release.tag_name != current_version).then(|| {
        format!(
            "{worker_name} launcher: a newer release ({}) is available — this session is \
             running {current_version} and won't pick it up until restarted",
            release.tag_name
        )
    })
}

/// Spawns a background task that calls `check_once` every `interval` for
/// the life of the process. Prints the first warning it gets to stderr,
/// then stops checking — a session that is already stale doesn't need to
/// be told again every hour.
pub fn spawn_watcher(
    worker_name: String,
    current_version: String,
    api_base: String,
    owner: String,
    repo: String,
    interval: Duration,
) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        loop {
            tokio::time::sleep(interval).await;
            if let Some(warning) = check_once(
                &client,
                &api_base,
                &owner,
                &repo,
                &worker_name,
                &current_version,
            )
            .await
            {
                eprintln!("{warning}");
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Same one-shot mock pattern `release_client`'s own tests use — kept
    /// local rather than shared since it's a handful of lines and this
    /// module's tests want a different body per case.
    async fn serve_once(status_line: &'static str, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn warns_when_a_newer_version_is_available() {
        let body = r#"{"tag_name":"v0.4.0","assets":[]}"#.to_string();
        let base = serve_once("HTTP/1.1 200 OK", body).await;
        let client = reqwest::Client::new();

        let warning = check_once(&client, &base, "iyulab", "docket", "docket-mcp", "v0.3.0-1")
            .await
            .unwrap();
        assert!(warning.contains("v0.4.0"));
        assert!(warning.contains("v0.3.0-1"));
        assert!(warning.contains("docket-mcp"));
    }

    #[tokio::test]
    async fn silent_when_already_current() {
        let body = r#"{"tag_name":"v0.3.0-1","assets":[]}"#.to_string();
        let base = serve_once("HTTP/1.1 200 OK", body).await;
        let client = reqwest::Client::new();

        let warning =
            check_once(&client, &base, "iyulab", "docket", "docket-mcp", "v0.3.0-1").await;
        assert!(warning.is_none());
    }

    #[tokio::test]
    async fn silent_when_the_check_itself_fails() {
        let base = serve_once("HTTP/1.1 500 Internal Server Error", "{}".to_string()).await;
        let client = reqwest::Client::new();

        let warning =
            check_once(&client, &base, "iyulab", "docket", "docket-mcp", "v0.3.0-1").await;
        assert!(warning.is_none());
    }
}
