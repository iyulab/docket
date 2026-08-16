//! Downloads a release asset and verifies it against the release's
//! `checksums.txt` — the same trust model `docket-launcher-core` uses for
//! worker binaries (GitHub TLS + self-published checksums, no code
//! signing), applied here to `docket-core` itself and `console-dist.zip`.

use docket_launcher_core::checksum;
use docket_launcher_core::release_client::Release;

/// Downloads and returns the release's `checksums.txt` body as text — one
/// fetch shared by every asset this release's `verify` calls check
/// against, rather than re-downloading it per asset.
pub async fn fetch_checksums(
    client: &reqwest::Client,
    release: &Release,
    user_agent: &str,
) -> anyhow::Result<String> {
    let asset = release
        .asset("checksums.txt")
        .ok_or_else(|| anyhow::anyhow!("release {} has no checksums.txt", release.tag_name))?;
    let text = client
        .get(&asset.browser_download_url)
        .header("User-Agent", user_agent)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(text)
}

/// Downloads `asset_name`'s raw bytes, unverified — pair with `verify`.
pub async fn fetch_asset_bytes(
    client: &reqwest::Client,
    release: &Release,
    asset_name: &str,
    user_agent: &str,
) -> anyhow::Result<Vec<u8>> {
    let asset = release.asset(asset_name).ok_or_else(|| {
        anyhow::anyhow!(
            "release {} has no asset named {asset_name}",
            release.tag_name
        )
    })?;
    let bytes = client
        .get(&asset.browser_download_url)
        .header("User-Agent", user_agent)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(bytes.to_vec())
}

/// Checks `bytes`' sha256 against `checksums_txt`'s entry for `asset_name`.
pub fn verify(checksums_txt: &str, asset_name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let expected = checksum::expected_hash(checksums_txt, asset_name)
        .ok_or_else(|| anyhow::anyhow!("checksums.txt has no entry for {asset_name}"))?;
    let actual = checksum::sha256_hex(bytes);
    if actual != expected {
        anyhow::bail!("checksum mismatch for {asset_name}: expected {expected}, got {actual}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn bind_mock() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("http://{addr}"))
    }

    /// Serves exactly one request (whatever path it is) with `body`, then
    /// stops — enough for the single-asset-fetch tests below.
    fn spawn_serving_one(listener: TcpListener, content_type: &'static str, body: Vec<u8>) {
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = socket.read(&mut buf).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });
    }

    fn release_with_asset(name: &str, url: String) -> Release {
        serde_json::from_value(serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [{"name": name, "browser_download_url": url}]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn fetch_checksums_returns_body_text() {
        let (listener, base) = bind_mock().await;
        spawn_serving_one(listener, "text/plain", b"aaa  console-dist.zip\n".to_vec());
        let release = release_with_asset("checksums.txt", format!("{base}/checksums.txt"));

        let client = reqwest::Client::new();
        let text = fetch_checksums(&client, &release, "docket-core-updater")
            .await
            .unwrap();
        assert_eq!(text, "aaa  console-dist.zip\n");
    }

    #[tokio::test]
    async fn fetch_checksums_missing_asset_is_an_error() {
        let release: Release = serde_json::from_value(serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": []
        }))
        .unwrap();
        let client = reqwest::Client::new();
        assert!(
            fetch_checksums(&client, &release, "docket-core-updater")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fetch_asset_bytes_returns_raw_content() {
        let (listener, base) = bind_mock().await;
        spawn_serving_one(
            listener,
            "application/octet-stream",
            b"fake exe bytes".to_vec(),
        );
        let release = release_with_asset(
            "docket-core-x86_64-pc-windows-msvc.exe",
            format!("{base}/asset"),
        );

        let client = reqwest::Client::new();
        let bytes = fetch_asset_bytes(
            &client,
            &release,
            "docket-core-x86_64-pc-windows-msvc.exe",
            "docket-core-updater",
        )
        .await
        .unwrap();
        assert_eq!(bytes, b"fake exe bytes");
    }

    #[test]
    fn verify_accepts_matching_checksum() {
        let bytes = b"console-dist.zip contents";
        let sha = checksum::sha256_hex(bytes);
        let checksums_txt = format!("{sha}  console-dist.zip\n");
        assert!(verify(&checksums_txt, "console-dist.zip", bytes).is_ok());
    }

    #[test]
    fn verify_rejects_mismatched_checksum() {
        let checksums_txt =
            "0000000000000000000000000000000000000000000000000000000000000000  console-dist.zip\n";
        let result = verify(
            checksums_txt,
            "console-dist.zip",
            b"console-dist.zip contents",
        );
        assert!(result.is_err());
    }

    #[test]
    fn verify_missing_entry_is_an_error() {
        let checksums_txt = "aaa  something-else\n";
        assert!(verify(checksums_txt, "console-dist.zip", b"bytes").is_err());
    }
}
