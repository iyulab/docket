//! Fetches release metadata from GitHub's REST API
//! (`GET /repos/{owner}/{repo}/releases/latest`). `api_base` is a parameter
//! (not hardcoded to `https://api.github.com`) so tests can point it at a
//! local mock server instead.

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

pub async fn latest_release(
    client: &reqwest::Client,
    api_base: &str,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Release> {
    let url = format!("{api_base}/repos/{owner}/{repo}/releases/latest");
    // GitHub's API rejects requests with no User-Agent header.
    let resp = client
        .get(&url)
        .header("User-Agent", "docket-mcp-launcher")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("GitHub releases API returned {}", resp.status());
    }
    Ok(resp.json::<Release>().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Binds a one-shot local HTTP/1.1 server: accepts exactly one
    /// connection, ignores the request, replies with `status_line` +
    /// `body`. Enough to drive `latest_release` without a real network call.
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
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn parses_tag_and_assets() {
        let body = r#"{"tag_name":"v0.2.0","assets":[{"name":"docket-mcp-x86_64-pc-windows-msvc.exe","browser_download_url":"http://example/dl"}]}"#;
        let base = serve_once("HTTP/1.1 200 OK", body).await;
        let client = reqwest::Client::new();
        let release = latest_release(&client, &base, "iyulab", "docket").await.unwrap();
        assert_eq!(release.tag_name, "v0.2.0");
        assert!(release.asset("docket-mcp-x86_64-pc-windows-msvc.exe").is_some());
        assert!(release.asset("nonexistent").is_none());
    }

    #[tokio::test]
    async fn non_success_status_is_an_error() {
        let base = serve_once("HTTP/1.1 404 Not Found", "{}").await;
        let client = reqwest::Client::new();
        let result = latest_release(&client, &base, "iyulab", "docket").await;
        assert!(result.is_err());
    }
}
