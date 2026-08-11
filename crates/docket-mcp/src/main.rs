//! Exposes docket-core's work-queue operations as MCP tools. This is a thin
//! translation layer: every tool is an HTTP call to a running `docket-core`
//! server. It never links `docket_core` as a library — the layer boundary is
//! drawn at the protocol (see docs/architecture.md "Four layers"), not at
//! the language, so this crate stays coupled only to core's HTTP/JSON
//! contract, not its internal Rust types.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServiceExt, tool, tool_router};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct DocketMcp {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct RegisterWorkerParams {
    /// Unique id for this worker.
    id: String,
    /// Topic prefixes this worker owns (see docs/glossary.md "topic").
    #[serde(default)]
    topics: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct CreateItemParams {
    /// The topic this item is filed in front of.
    topic: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListItemsParams {
    /// Exact-match topic filter.
    #[serde(default)]
    topic: Option<String>,
    /// One of open/claimed/resolved/closed.
    #[serde(default)]
    state: Option<String>,
    /// A registered worker id — narrows the list to items under any topic
    /// that worker owns (prefix match).
    #[serde(default)]
    owned_by: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClaimOrSubmitParams {
    item_id: String,
    worker_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApproveParams {
    item_id: String,
}

/// Mirrors `docket-core`'s item JSON shape. A separate type from
/// `docket_core::Item` on purpose — see the module doc.
#[derive(Debug, Serialize, Deserialize)]
struct ItemDto {
    id: String,
    topic: String,
    title: String,
    body: Option<String>,
    state: String,
    resolution: Option<String>,
    owner: Option<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerDto {
    id: String,
    topics: Vec<String>,
    online: bool,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: String,
}

/// Turns a `docket-core` HTTP response into a tool result: a non-2xx status
/// becomes a tool-level error (the model sees it and can react — e.g. retry
/// `list_items` after losing a claim race) rather than a protocol error.
async fn respond<T: Serialize + for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<CallToolResult, McpError> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    if status.is_success() {
        let value: T = serde_json::from_slice(&bytes)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let block = ContentBlock::json(&value)?;
        Ok(CallToolResult::success(vec![block]))
    } else {
        let message = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|b| b.error)
            .unwrap_or_else(|_| format!("docket-core returned {status}"));
        Ok(CallToolResult::error(vec![ContentBlock::text(message)]))
    }
}

#[tool_router(server_handler)]
impl DocketMcp {
    #[tool(description = "Register as a worker, reporting which topic prefixes you own")]
    async fn register_worker(
        &self,
        Parameters(p): Parameters<RegisterWorkerParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/workers", self.base_url))
            .json(&p)
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<WorkerDto>(resp).await
    }

    #[tool(description = "File a new item in front of a topic")]
    async fn create_item(
        &self,
        Parameters(p): Parameters<CreateItemParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/items", self.base_url))
            .json(&p)
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "List items, optionally filtered by topic, state, and/or a worker's ownership"
    )]
    async fn list_items(
        &self,
        Parameters(p): Parameters<ListItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .get(format!("{}/items", self.base_url))
            .query(&[
                ("topic", p.topic.as_deref()),
                ("state", p.state.as_deref()),
                ("owned_by", p.owned_by.as_deref()),
            ])
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<Vec<ItemDto>>(resp).await
    }

    #[tool(
        description = "Claim an open item — exclusive, only one worker can win a race for the same item"
    )]
    async fn claim_item(
        &self,
        Parameters(p): Parameters<ClaimOrSubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/items/{}/claim", self.base_url, p.item_id))
            .json(&serde_json::json!({ "worker_id": p.worker_id }))
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Submit a claimed item as done, moving it to resolved for the requester to approve"
    )]
    async fn submit_item(
        &self,
        Parameters(p): Parameters<ClaimOrSubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/items/{}/submit", self.base_url, p.item_id))
            .json(&serde_json::json!({ "worker_id": p.worker_id }))
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Approve a resolved item as the requester, closing it with resolution=done"
    )]
    async fn approve_item(
        &self,
        Parameters(p): Parameters<ApproveParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/items/{}/approve", self.base_url, p.item_id))
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<ItemDto>(resp).await
    }
}

fn http_client() -> reqwest::Client {
    // Without a timeout, a hung or unreachable docket-core blocks a tool
    // call — and the calling AI session — forever instead of surfacing as
    // an error the model can react to.
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest client builds with a plain timeout")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base_url =
        std::env::var("DOCKET_CORE_URL").unwrap_or_else(|_| "http://127.0.0.1:8420".to_string());
    let server = DocketMcp {
        http: http_client(),
        base_url,
    };
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::time::Duration;

    struct CoreProcess {
        child: Child,
        base_url: String,
    }

    impl Drop for CoreProcess {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Sibling binary in the same workspace `target/` — Cargo only exposes
    /// `CARGO_BIN_EXE_*` for a package's own binaries, not other workspace
    /// members, so this walks from the test binary's own path instead.
    fn docket_core_binary() -> std::path::PathBuf {
        let mut path = std::env::current_exe().expect("current test executable path");
        path.pop(); // .../target/debug/deps
        path.pop(); // .../target/debug
        path.push(if cfg!(windows) {
            "docket-core.exe"
        } else {
            "docket-core"
        });
        path
    }

    async fn spawn_core(port: u16, db_path: &std::path::Path) -> CoreProcess {
        let binary = docket_core_binary();
        let child = Command::new(&binary)
            .env("DOCKET_PORT", port.to_string())
            .env("DOCKET_DB_PATH", db_path)
            .spawn()
            .unwrap_or_else(|e| {
                panic!("failed to spawn {binary:?} (run `cargo build -p docket-core` first): {e}")
            });
        let base_url = format!("http://127.0.0.1:{port}");
        // Wrap immediately: `Child::drop` doesn't reap the process, so if the
        // readiness loop below panics before returning, an un-wrapped
        // `child` would leak a zombie instead of being killed by `Drop`.
        let process = CoreProcess { child, base_url };
        let client = reqwest::Client::new();
        for _ in 0..50 {
            if client
                .get(format!("{}/items", process.base_url))
                .send()
                .await
                .is_ok()
            {
                return process;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("docket-core did not become ready within 5s");
    }

    fn text_of(result: &CallToolResult) -> &str {
        match result.content.first() {
            Some(ContentBlock::Text(t)) => &t.text,
            other => panic!("expected a text content block, got {other:?}"),
        }
    }

    fn field(result: &CallToolResult, name: &str) -> String {
        let value: serde_json::Value =
            serde_json::from_str(text_of(result)).expect("tool result is JSON");
        value[name]
            .as_str()
            .unwrap_or_else(|| panic!("field {name} missing or not a string in {value}"))
            .to_string()
    }

    /// The M1 lifecycle (open -> claimed -> resolved -> closed), exercised
    /// through the MCP tool functions exactly as an MCP client would call
    /// them (minus the stdio framing) — verifying it holds over the mcp ->
    /// core HTTP hop, not just raw HTTP to core directly.
    #[tokio::test]
    async fn full_lifecycle_through_mcp_tools() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("lifecycle.db");
        let core = spawn_core(18420, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "fix the thing".to_string(),
                body: None,
            }))
            .await
            .unwrap();
        assert_ne!(created.is_error, Some(true));
        let item_id = field(&created, "id");

        let registered = server
            .register_worker(Parameters(RegisterWorkerParams {
                id: "w1".to_string(),
                topics: vec!["iyulab".to_string()],
            }))
            .await
            .unwrap();
        assert_ne!(registered.is_error, Some(true));

        let listed = server
            .list_items(Parameters(ListItemsParams {
                topic: None,
                state: Some("open".to_string()),
                owned_by: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(listed.is_error, Some(true));
        let listed_value: serde_json::Value = serde_json::from_str(text_of(&listed)).unwrap();
        assert_eq!(listed_value.as_array().unwrap().len(), 1);

        let claimed = server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(claimed.is_error, Some(true));
        assert_eq!(field(&claimed, "state"), "claimed");

        let submitted = server
            .submit_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w1".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&submitted, "state"), "resolved");

        let approved = server
            .approve_item(Parameters(ApproveParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&approved, "state"), "closed");
        assert_eq!(field(&approved, "resolution"), "done");
    }

    /// Losing a claim race must come back as a tool-level error the model
    /// can see and react to, not a protocol error it can't.
    #[tokio::test]
    async fn claim_conflict_is_tool_level_error() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-conflict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("conflict.db");
        let core = spawn_core(18421, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "race".to_string(),
                body: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let first = server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w1".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(first.is_error, Some(true));

        let second = server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w2".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(second.is_error, Some(true));
    }

    /// An unreachable docket-core must surface as a protocol error the
    /// tool-call machinery propagates, not a hang or a panic. A closed port
    /// refuses the connection immediately, so this doesn't need to wait out
    /// the client's 10s timeout to prove the failure path works.
    #[tokio::test]
    async fn unreachable_core_is_an_error_not_a_hang() {
        let server = DocketMcp {
            http: http_client(),
            base_url: "http://127.0.0.1:1".to_string(),
        };
        let result = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "t".to_string(),
                body: None,
            }))
            .await;
        assert!(result.is_err());
    }
}
