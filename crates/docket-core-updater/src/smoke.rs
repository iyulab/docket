//! Verifies the freshly restarted `docket-core` is actually serving before
//! declaring an update successful — the safety net that decides whether
//! `run()` keeps the new version or rolls back to `.prev`.

pub async fn check(client: &reqwest::Client, items_url: &str) -> anyhow::Result<()> {
    let resp = client.get(items_url).send().await?;
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
        assert!(check(&client, &url).await.is_ok());
    }

    #[tokio::test]
    async fn error_status_fails() {
        let url = serve_once("HTTP/1.1 503 Service Unavailable").await;
        let client = reqwest::Client::new();
        assert!(check(&client, &url).await.is_err());
    }

    #[tokio::test]
    async fn unreachable_host_fails() {
        let client = reqwest::Client::new();
        assert!(check(&client, "http://127.0.0.1:1/items").await.is_err());
    }
}
