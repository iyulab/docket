//! Fetches the locally running `docket-core`'s `/status` (version +
//! idle_seconds) — the signal this updater polls to decide whether to
//! replace the running binary.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Status {
    pub version: String,
    pub idle_seconds: u64,
}

pub async fn fetch_status(client: &reqwest::Client, status_url: &str) -> anyhow::Result<Status> {
    let resp = client.get(status_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("GET {status_url} returned {}", resp.status());
    }
    Ok(resp.json::<Status>().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Binds a one-shot local HTTP/1.1 server: accepts exactly one
    /// connection, ignores the request, replies with `status_line` +
    /// `body`. Same pattern `docket-launcher-core::release_client`'s tests
    /// use.
    async fn serve_once(status_line: &'static str, body: &'static str) -> String {
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
        format!("http://{addr}/status")
    }

    #[tokio::test]
    async fn parses_version_and_idle_seconds() {
        let url = serve_once(
            "HTTP/1.1 200 OK",
            r#"{"version":"v0.2.0","idle_seconds":842}"#,
        )
        .await;
        let client = reqwest::Client::new();
        let status = fetch_status(&client, &url).await.unwrap();
        assert_eq!(
            status,
            Status {
                version: "v0.2.0".to_string(),
                idle_seconds: 842
            }
        );
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let url = serve_once("HTTP/1.1 500 Internal Server Error", "{}").await;
        let client = reqwest::Client::new();
        assert!(fetch_status(&client, &url).await.is_err());
    }
}
