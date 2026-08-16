//! `docket-mcp` launcher: MCP client configs point at this binary under the
//! name `docket-mcp`. It resolves the latest `docket-mcp` worker build via
//! `docket_launcher_core::resolve_and_run` (GitHub Releases check, cache,
//! checksum verify) and execs it with this process's stdio inherited.
//!
//! Never writes to its own stdout — stdout becomes the MCP JSON-RPC stream
//! once the worker takes over. All diagnostics (from `docket-launcher-core`)
//! go to stderr.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let code = docket_launcher_core::resolve_and_run("docket-mcp", &[]).await?;
    std::process::exit(code);
}
