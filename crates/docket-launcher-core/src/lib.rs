//! Shared "check GitHub Releases → cache if needed → verify checksum → exec"
//! launcher logic, reused by both `docket-mcp-launcher` and
//! `docket-cc-launcher`.

mod cache;
mod checksum;
mod delegate;
mod platform;
mod release_client;
