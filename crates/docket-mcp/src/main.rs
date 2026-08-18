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
    /// Free-form labels. Call `list_tags` first to reuse existing
    /// vocabulary instead of inventing a new tag string.
    #[serde(default)]
    tags: Vec<String>,
    /// Who this item is being worked for — the requester's identity, shown
    /// back as `from` on the item. Optional; omit if there's no natural
    /// caller identity for this item.
    #[serde(default)]
    from: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListItemsParams {
    /// Exact-match topic filter.
    #[serde(default)]
    topic: Option<String>,
    /// One of open/claimed/resolved/closed.
    #[serde(default)]
    state: Option<String>,
    /// A worker id — narrows the list to items whose `to` (current
    /// assignee) is exactly this worker.
    #[serde(default)]
    to: Option<String>,
    /// Exact-match on `from` (the requester) — symmetric to `to`, above.
    #[serde(default)]
    from: Option<String>,
    /// A registered worker id — narrows the list to items under any topic
    /// that worker is registered for (prefix match). Unlike `to`, this
    /// doesn't check who actually holds any given item — it's a
    /// topic-jurisdiction filter, not an ownership filter.
    #[serde(default)]
    topic_scope: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchItemsParams {
    /// Full-text match against title+body.
    #[serde(default)]
    query: Option<String>,
    /// Filter to items carrying any/all of these tags (see `tag_match`).
    #[serde(default)]
    tags: Vec<String>,
    /// "any" (default) or "all". Ignored if `tags` is empty.
    #[serde(default)]
    tag_match: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    state: Option<String>,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TagsParams {
    item_id: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListTagsParams {
    /// Scope the vocabulary to items under this exact-match topic.
    #[serde(default)]
    topic: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AddCommentParams {
    item_id: String,
    #[serde(default)]
    author: Option<String>,
    body: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCommentsParams {
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
    /// Absent from servers older than ADR-0010. Defaulted for the same
    /// reason as `tags` below.
    #[serde(default)]
    from: Option<String>,
    /// Was `owner` before ADR-0010; defaulted for the same reason as `from`.
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    turn: Option<String>,
    /// Absent from servers older than the tag feature. Without a default,
    /// every tool response from such a server fails to deserialize, not just
    /// the tag-related ones.
    #[serde(default)]
    tags: Vec<String>,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerDto {
    id: String,
    topics: Vec<String>,
    online: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct TagCountDto {
    tag: String,
    count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct CommentDto {
    id: String,
    item_id: String,
    author: String,
    body: String,
    created_at: i64,
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
        description = "List items, optionally filtered by topic, state, the worker currently assigned (to), the requester (from), and/or a worker's topic jurisdiction (topic_scope)"
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
                ("to", p.to.as_deref()),
                ("from", p.from.as_deref()),
                ("topic_scope", p.topic_scope.as_deref()),
            ])
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<Vec<ItemDto>>(resp).await
    }

    #[tool(
        description = "Search items by full-text query and/or tags — call this before create_item to check whether a matching issue already exists"
    )]
    async fn search_items(
        &self,
        Parameters(p): Parameters<SearchItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut query_pairs: Vec<(&str, &str)> = Vec::new();
        if let Some(q) = p.query.as_deref() {
            query_pairs.push(("q", q));
        }
        for tag in &p.tags {
            query_pairs.push(("tag", tag.as_str()));
        }
        if let Some(m) = p.tag_match.as_deref() {
            query_pairs.push(("tag_match", m));
        }
        if let Some(t) = p.topic.as_deref() {
            query_pairs.push(("topic", t));
        }
        if let Some(s) = p.state.as_deref() {
            query_pairs.push(("state", s));
        }
        let resp = self
            .http
            .get(format!("{}/items", self.base_url))
            .query(&query_pairs)
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

    #[tool(
        description = "Add tags to an item (idempotent — adding an already-present tag is a no-op)"
    )]
    async fn add_tags(
        &self,
        Parameters(p): Parameters<TagsParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/items/{}/tags", self.base_url, p.item_id))
            .json(&serde_json::json!({ "tags": p.tags }))
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<Vec<String>>(resp).await
    }

    #[tool(
        description = "Remove tags from an item (idempotent — removing an absent tag is a no-op)"
    )]
    async fn remove_tags(
        &self,
        Parameters(p): Parameters<TagsParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .delete(format!("{}/items/{}/tags", self.base_url, p.item_id))
            .json(&serde_json::json!({ "tags": p.tags }))
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<Vec<String>>(resp).await
    }

    #[tool(
        description = "List existing tags and how many items carry each, most-used first — call this before drafting a new item to reuse existing vocabulary instead of a new synonym"
    )]
    async fn list_tags(
        &self,
        Parameters(p): Parameters<ListTagsParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .get(format!("{}/tags", self.base_url))
            .query(&[("topic", p.topic.as_deref())])
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<Vec<TagCountDto>>(resp).await
    }

    #[tool(
        description = "Add a follow-up note to an item — upstream replies, extra repro info, release notices"
    )]
    async fn add_comment(
        &self,
        Parameters(p): Parameters<AddCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut body = serde_json::json!({ "body": p.body });
        if let Some(author) = p.author {
            body["author"] = serde_json::Value::String(author);
        }
        let resp = self
            .http
            .post(format!("{}/items/{}/comments", self.base_url, p.item_id))
            .json(&body)
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<CommentDto>(resp).await
    }

    #[tool(description = "List an item's comment thread in chronological order")]
    async fn list_comments(
        &self,
        Parameters(p): Parameters<ListCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .get(format!("{}/items/{}/comments", self.base_url, p.item_id))
            .send()
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        respond::<Vec<CommentDto>>(resp).await
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
                tags: vec![],
                from: None,
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
                to: None,
                from: None,
                topic_scope: Some("w1".to_string()),
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
                tags: vec![],
                from: None,
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
                tags: vec![],
                from: None,
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn create_item_with_tags_then_add_remove_and_list_tags() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("tags.db");
        let core = spawn_core(18422, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/node-packages".to_string(),
                title: "t".to_string(),
                body: None,
                tags: vec!["severity:medium".to_string()],
                from: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let added = server
            .add_tags(Parameters(TagsParams {
                item_id: item_id.clone(),
                tags: vec!["awaiting-release".to_string()],
            }))
            .await
            .unwrap();
        assert_ne!(added.is_error, Some(true));

        let tags = server
            .list_tags(Parameters(ListTagsParams { topic: None }))
            .await
            .unwrap();
        let tags_value: serde_json::Value = serde_json::from_str(text_of(&tags)).unwrap();
        assert!(tags_value.as_array().unwrap().len() >= 2);

        let removed = server
            .remove_tags(Parameters(TagsParams {
                item_id: item_id.clone(),
                tags: vec!["awaiting-release".to_string()],
            }))
            .await
            .unwrap();
        let removed_value: serde_json::Value = serde_json::from_str(text_of(&removed)).unwrap();
        assert_eq!(removed_value, serde_json::json!(["severity:medium"]));
    }

    #[tokio::test]
    async fn search_items_finds_by_query_and_tag() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-search-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("search.db");
        let core = spawn_core(18423, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/node-packages".to_string(),
                title: "form Enter bypasses preventDefault".to_string(),
                body: None,
                tags: vec!["severity:medium".to_string()],
                from: None,
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/node-packages".to_string(),
                title: "unrelated".to_string(),
                body: None,
                tags: vec![],
                from: None,
            }))
            .await
            .unwrap();

        let found = server
            .search_items(Parameters(SearchItemsParams {
                query: Some("preventDefault".to_string()),
                tags: vec![],
                tag_match: None,
                topic: None,
                state: None,
            }))
            .await
            .unwrap();
        let found_value: serde_json::Value = serde_json::from_str(text_of(&found)).unwrap();
        assert_eq!(found_value.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn add_comment_then_list_comments() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-comments-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("comments.db");
        let core = spawn_core(18424, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "t".to_string(),
                body: None,
                tags: vec![],
                from: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let added = server
            .add_comment(Parameters(AddCommentParams {
                item_id: item_id.clone(),
                author: Some("maintainer".to_string()),
                body: "root cause found".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(added.is_error, Some(true));

        let listed = server
            .list_comments(Parameters(ListCommentsParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();
        let listed_value: serde_json::Value = serde_json::from_str(text_of(&listed)).unwrap();
        assert_eq!(listed_value.as_array().unwrap().len(), 1);
        assert_eq!(listed_value[0]["author"], "maintainer");
    }
}
