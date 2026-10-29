//! Verifies the freshly restarted `docket-core` is actually serving before
//! declaring an update successful — the safety net that decides whether
//! `run()` keeps the new version or rolls back to `.prev`.

/// `token` is `DOCKET_TOKEN` (ADR-0026): the probe reads `/items`, which a
/// docket-core with auth enabled refuses without it — and a refused probe
/// would be read as a failed update and roll back a good release. It is sent
/// on this request only: the shared `client` also talks to GitHub, which must
/// never see it.
pub async fn check(
    client: &reqwest::Client,
    items_url: &str,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let mut request = client.get(items_url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let resp = request.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("smoke check GET {items_url} returned {}", resp.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(status_line: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!("{status_line}\r\nContent-Length: 2\r\n\r\n[]");
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{addr}/items")
    }

    #[tokio::test]
    async fn ok_status_passes() {
        let url = serve_once("HTTP/1.1 200 OK").await;
        let client = reqwest::Client::new();
        assert!(check(&client, &url, None).await.is_ok());
    }

    #[tokio::test]
    async fn error_status_fails() {
        let url = serve_once("HTTP/1.1 503 Service Unavailable").await;
        let client = reqwest::Client::new();
        assert!(check(&client, &url, None).await.is_err());
    }

    /// Serves `200` only to a request carrying `Authorization: Bearer secret`.
    async fn serve_gated() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 2048];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let status = if request.contains("authorization: bearer secret") {
                    "HTTP/1.1 200 OK"
                } else {
                    "HTTP/1.1 401 Unauthorized"
                };
                let response = format!("{status}\r\nContent-Length: 2\r\n\r\n[]");
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        format!("http://{addr}/items")
    }

    #[tokio::test]
    async fn token_is_sent_when_given_and_required_by_a_gated_server() {
        let url = serve_gated().await;
        let client = reqwest::Client::new();
        assert!(check(&client, &url, Some("secret")).await.is_ok());
        assert!(check(&client, &url, None).await.is_err());
        assert!(check(&client, &url, Some("wrong")).await.is_err());
    }

    #[tokio::test]
    async fn unreachable_host_fails() {
        let client = reqwest::Client::new();
        assert!(
            check(&client, "http://127.0.0.1:1/items", None)
                .await
                .is_err()
        );
    }
}
