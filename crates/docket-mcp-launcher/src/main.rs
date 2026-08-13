//! `docket-mcp` launcher: MCP client configs point at this binary under the
//! name `docket-mcp`. It checks GitHub Releases for the latest `docket-mcp`
//! worker build, downloads it into a local cache if needed (verifying its
//! checksum first), and execs the cached worker with this process's stdio
//! inherited. Full design: `claudedocs/plans/PLAN-docket-20260812-mcp-launcher-design.md`
//! (private, `docket-works`).
//!
//! Never writes to its own stdout — stdout becomes the MCP JSON-RPC stream
//! once the worker takes over. All diagnostics here go to stderr.

mod cache;
mod checksum;
mod delegate;
mod platform;
mod release_client;

use std::path::{Path, PathBuf};

const OWNER: &str = "iyulab";
const REPO: &str = "docket";
const GITHUB_API_BASE: &str = "https://api.github.com";

#[cfg(windows)]
const BINARY_EXT: &str = ".exe";
#[cfg(not(windows))]
const BINARY_EXT: &str = "";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cache_root = cache::cache_root()?;
    let asset_name = platform::current_asset_name("docket-mcp").ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported platform: {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let binary_path =
        resolve_worker_binary(&cache_root, &asset_name, BINARY_EXT, GITHUB_API_BASE).await?;
    let code = delegate::run(&binary_path, &[])?;
    std::process::exit(code);
}

/// Same data flow as the design spec §4: check the latest release, use the
/// cache if it's already current, otherwise download+verify+store; on any
/// failure reaching GitHub *or* completing that download+verify (asset
/// fetch, checksums.txt fetch, or a checksum mismatch), fall back to the
/// newest cached version instead of failing outright — only a hard error
/// when there's nothing cached at all.
async fn resolve_worker_binary(
    cache_root: &Path,
    asset_name: &str,
    binary_ext: &str,
    api_base: &str,
) -> anyhow::Result<PathBuf> {
    // `pool_max_idle_per_host(0)`: this function makes 1-3 requests total,
    // at most once per launch — connection reuse buys nothing here, and
    // disabling it keeps behavior deterministic for tests using a one-shot
    // mock server that expects one fresh connection per request.
    //
    // `connect_timeout` (rather than a whole-request `timeout`) only bounds
    // how long establishing the TCP connection may take; it doesn't cap the
    // body transfer, so it won't abort a large binary download on a slow
    // link the way a total-request timeout would.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_max_idle_per_host(0)
        .build()?;

    match release_client::latest_release(&client, api_base, OWNER, REPO).await {
        Ok(release) => {
            let version = &release.tag_name;
            let cached = cache::cached_binary_path(cache_root, version, binary_ext);
            if cached.exists() {
                return Ok(cached);
            }

            match download_and_store(&client, &release, cache_root, asset_name, binary_ext).await {
                Ok(path) => Ok(path),
                Err(e) => fall_back_to_cache(cache_root, binary_ext, "download failed", e),
            }
        }
        Err(e) => fall_back_to_cache(cache_root, binary_ext, "update check failed", e),
    }
}

/// Downloads the release's asset and checksums.txt, verifies the asset's
/// hash, and stores it in the cache. Split out of `resolve_worker_binary` so
/// that any failure in this sequence — not just a failure to reach the
/// releases API itself — can share the same cache-fallback path via
/// `fall_back_to_cache`.
async fn download_and_store(
    client: &reqwest::Client,
    release: &release_client::Release,
    cache_root: &Path,
    asset_name: &str,
    binary_ext: &str,
) -> anyhow::Result<PathBuf> {
    let version = &release.tag_name;
    let asset = release
        .asset(asset_name)
        .ok_or_else(|| anyhow::anyhow!("release {version} has no asset named {asset_name}"))?;
    let bytes = client
        .get(&asset.browser_download_url)
        .header("User-Agent", "docket-mcp-launcher")
        .send()
        .await?
        .bytes()
        .await?;

    let checksums_asset = release
        .asset("checksums.txt")
        .ok_or_else(|| anyhow::anyhow!("release {version} has no checksums.txt"))?;
    let checksums_txt = client
        .get(&checksums_asset.browser_download_url)
        .header("User-Agent", "docket-mcp-launcher")
        .send()
        .await?
        .text()
        .await?;
    let expected = checksum::expected_hash(&checksums_txt, asset_name)
        .ok_or_else(|| anyhow::anyhow!("checksums.txt has no entry for {asset_name}"))?;
    let actual = checksum::sha256_hex(&bytes);
    if actual != expected {
        anyhow::bail!("checksum mismatch for {asset_name}: expected {expected}, got {actual}");
    }

    Ok(cache::store(cache_root, version, binary_ext, &bytes)?)
}

/// Shared fallback for both failure sources in `resolve_worker_binary`:
/// warns on stderr and returns the newest cached binary if one exists, or a
/// hard error (still stderr-only — nothing here ever touches stdout) if
/// there's nothing cached to fall back to.
fn fall_back_to_cache(
    cache_root: &Path,
    binary_ext: &str,
    context: &str,
    e: anyhow::Error,
) -> anyhow::Result<PathBuf> {
    eprintln!("docket-mcp launcher: {context} ({e}), trying cache");
    cache::latest_cached(cache_root, binary_ext)
        .ok_or_else(|| anyhow::anyhow!("{context} and no cached docket-mcp is available: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn bind_mock() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("http://{addr}"))
    }

    /// Serves `routes` (path -> (content-type, body)), one request per
    /// `routes.len()` accepted connection, then stops. Matches
    /// `resolve_worker_binary`'s fixed request sequence for a cache-miss run:
    /// releases/latest, then the asset, then checksums.txt.
    fn spawn_serving(listener: TcpListener, routes: HashMap<String, (&'static str, Vec<u8>)>) {
        tokio::spawn(async move {
            for _ in 0..routes.len() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let (content_type, body) = routes
                    .get(path)
                    .unwrap_or_else(|| panic!("unexpected request path: {path}"));
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            }
        });
    }

    fn temp_cache_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "docket-launcher-main-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[tokio::test]
    async fn downloads_verifies_and_caches_a_new_version() {
        let asset_name = "docket-mcp-test-asset";
        let worker_bytes = b"#!/bin/sh\necho fake-worker\n".to_vec();
        let sha = checksum::sha256_hex(&worker_bytes);

        let (listener, base) = bind_mock().await;
        let release_json = format!(
            r#"{{"tag_name":"v0.9.9","assets":[{{"name":"{asset_name}","browser_download_url":"{base}/asset"}},{{"name":"checksums.txt","browser_download_url":"{base}/checksums.txt"}}]}}"#
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/repos/iyulab/docket/releases/latest".to_string(),
            ("application/json", release_json.into_bytes()),
        );
        routes.insert(
            "/asset".to_string(),
            ("application/octet-stream", worker_bytes.clone()),
        );
        routes.insert(
            "/checksums.txt".to_string(),
            ("text/plain", format!("{sha}  {asset_name}\n").into_bytes()),
        );
        spawn_serving(listener, routes);

        let cache_root = temp_cache_root("new-version");
        let path = resolve_worker_binary(&cache_root, asset_name, "", &base)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), worker_bytes);
        assert_eq!(path, cache::cached_binary_path(&cache_root, "v0.9.9", ""));

        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn checksum_mismatch_is_an_error_and_nothing_is_cached() {
        let asset_name = "docket-mcp-test-asset";
        let worker_bytes = b"real bytes".to_vec();

        let (listener, base) = bind_mock().await;
        let release_json = format!(
            r#"{{"tag_name":"v0.9.9","assets":[{{"name":"{asset_name}","browser_download_url":"{base}/asset"}},{{"name":"checksums.txt","browser_download_url":"{base}/checksums.txt"}}]}}"#
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/repos/iyulab/docket/releases/latest".to_string(),
            ("application/json", release_json.into_bytes()),
        );
        routes.insert(
            "/asset".to_string(),
            ("application/octet-stream", worker_bytes),
        );
        // Deliberately wrong hash.
        routes.insert(
            "/checksums.txt".to_string(),
            (
                "text/plain",
                format!("0000000000000000000000000000000000000000000000000000000000000000  {asset_name}\n")
                    .into_bytes(),
            ),
        );
        spawn_serving(listener, routes);

        let cache_root = temp_cache_root("bad-checksum");
        let result = resolve_worker_binary(&cache_root, asset_name, "", &base).await;
        assert!(result.is_err());
        assert!(!cache::cached_binary_path(&cache_root, "v0.9.9", "").exists());

        let _ = std::fs::remove_dir_all(&cache_root);
    }

    #[tokio::test]
    async fn checksum_mismatch_falls_back_to_cache_when_present() {
        let asset_name = "docket-mcp-test-asset";
        let worker_bytes = b"real bytes".to_vec();

        let (listener, base) = bind_mock().await;
        let release_json = format!(
            r#"{{"tag_name":"v0.9.9","assets":[{{"name":"{asset_name}","browser_download_url":"{base}/asset"}},{{"name":"checksums.txt","browser_download_url":"{base}/checksums.txt"}}]}}"#
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/repos/iyulab/docket/releases/latest".to_string(),
            ("application/json", release_json.into_bytes()),
        );
        routes.insert(
            "/asset".to_string(),
            ("application/octet-stream", worker_bytes),
        );
        // Deliberately wrong hash.
        routes.insert(
            "/checksums.txt".to_string(),
            (
                "text/plain",
                format!("0000000000000000000000000000000000000000000000000000000000000000  {asset_name}\n")
                    .into_bytes(),
            ),
        );
        spawn_serving(listener, routes);

        let cache_root = temp_cache_root("bad-checksum-with-cache");
        // Pre-existing cache is a different version than the release
        // reports, so the cache-hit early-return at the top of
        // `resolve_worker_binary` doesn't fire and the download path runs
        // (and fails on the checksum check) before the fallback kicks in.
        cache::store(&cache_root, "v0.1.0", "", b"stale but present").unwrap();

        let result = resolve_worker_binary(&cache_root, asset_name, "", &base)
            .await
            .unwrap();
        assert_eq!(result, cache::cached_binary_path(&cache_root, "v0.1.0", ""));
        // The bad download must never have been cached under the new version.
        assert!(!cache::cached_binary_path(&cache_root, "v0.9.9", "").exists());

        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn asset_download_failure_falls_back_to_cache_when_present() {
        let asset_name = "docket-mcp-test-asset";
        let cache_root = temp_cache_root("asset-download-fails-with-cache");
        cache::store(&cache_root, "v0.1.0", "", b"stale but present").unwrap();

        let (listener, base) = bind_mock().await;
        // The asset's download URL points at a port that refuses
        // connections, so the asset GET itself fails — a different failure
        // point than a checksum mismatch — while the release-check response
        // is otherwise valid.
        let release_json = format!(
            r#"{{"tag_name":"v0.9.9","assets":[{{"name":"{asset_name}","browser_download_url":"http://127.0.0.1:1/asset"}},{{"name":"checksums.txt","browser_download_url":"{base}/checksums.txt"}}]}}"#
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/repos/iyulab/docket/releases/latest".to_string(),
            ("application/json", release_json.into_bytes()),
        );
        spawn_serving(listener, routes);

        let result = resolve_worker_binary(&cache_root, asset_name, "", &base)
            .await
            .unwrap();
        assert_eq!(result, cache::cached_binary_path(&cache_root, "v0.1.0", ""));

        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn already_cached_version_skips_download() {
        let asset_name = "docket-mcp-test-asset";
        let cache_root = temp_cache_root("already-cached");
        cache::store(&cache_root, "v0.9.9", "", b"already here").unwrap();

        // Only route the mock serves is releases/latest — no /asset or
        // /checksums.txt route exists. `spawn_serving` stops accepting once
        // it has served its one route, so a stray download attempt would
        // hit a closed listener and fail fast with "connection refused"
        // rather than hang — caught by the final .unwrap() below, not a
        // silent false pass.
        let (listener, base) = bind_mock().await;
        let release_json = format!(
            r#"{{"tag_name":"v0.9.9","assets":[{{"name":"{asset_name}","browser_download_url":"{base}/asset"}}]}}"#
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/repos/iyulab/docket/releases/latest".to_string(),
            ("application/json", release_json.into_bytes()),
        );
        spawn_serving(listener, routes);

        let path = resolve_worker_binary(&cache_root, asset_name, "", &base)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"already here");

        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn unreachable_api_falls_back_to_cache() {
        let asset_name = "docket-mcp-test-asset";
        let cache_root = temp_cache_root("fallback");
        cache::store(&cache_root, "v0.1.0", "", b"stale but present").unwrap();

        // Port 1 refuses connections immediately (same trick docket-mcp's
        // own tests use for "unreachable").
        let result = resolve_worker_binary(&cache_root, asset_name, "", "http://127.0.0.1:1").await;
        assert_eq!(
            result.unwrap(),
            cache::cached_binary_path(&cache_root, "v0.1.0", "")
        );

        std::fs::remove_dir_all(&cache_root).unwrap();
    }

    #[tokio::test]
    async fn unreachable_api_and_no_cache_is_an_error() {
        let cache_root = temp_cache_root("no-fallback");
        let result = resolve_worker_binary(
            &cache_root,
            "docket-mcp-test-asset",
            "",
            "http://127.0.0.1:1",
        )
        .await;
        assert!(result.is_err());
    }
}
