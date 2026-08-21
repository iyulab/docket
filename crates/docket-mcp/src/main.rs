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

// Every tool-parameter struct below denies unknown fields — a caller
// guessing a stale or misremembered field name (e.g. `owned_by`, the
// pre-ADR-0010 name for what's now `assignee`/`requester`/`topic_scope`)
// otherwise deserializes successfully with that field silently dropped,
// which for a filter parameter reads as "no matching items" rather than
// "you misspelled the filter". A rejected-field error is far more
// actionable than a quietly-empty result.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RegisterWorkerParams {
    /// Unique id for this worker.
    id: String,
    /// Topic prefixes this worker owns (see docs/glossary.md "topic").
    #[serde(default)]
    topics: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
    /// back as `requester` on the item. Optional; omit if there's no natural
    /// caller identity for this item.
    #[serde(default)]
    requester: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListItemsParams {
    /// Exact-match topic filter.
    #[serde(default)]
    topic: Option<String>,
    /// One of open/claimed/resolved/closed.
    #[serde(default)]
    state: Option<String>,
    /// A worker id — narrows the list to items whose `assignee` (current
    /// holder) is exactly this worker.
    #[serde(default)]
    assignee: Option<String>,
    /// Exact-match on `requester` — symmetric to `assignee`, above.
    #[serde(default)]
    requester: Option<String>,
    /// A registered worker id — narrows the list to items under any topic
    /// that worker is registered for (prefix match). Unlike `assignee`,
    /// this doesn't check who actually holds any given item — it's a
    /// topic-jurisdiction filter, not an ownership filter.
    #[serde(default)]
    topic_scope: Option<String>,
    /// Excludes archived items by default; `true` returns only archived
    /// items (explicit archive browse). See ADR-0013.
    #[serde(default)]
    archived: Option<bool>,
    /// Max rows returned, applied after every other filter. Server default
    /// 50, hard-capped at 200 — see ADR-0014. The tool result's `total`
    /// field reports how many rows matched before this cap, so you know
    /// whether to page with `offset`.
    #[serde(default)]
    limit: Option<usize>,
    /// Rows to skip before applying `limit`. Defaults to 0.
    #[serde(default)]
    offset: Option<usize>,
    /// When `true`, every returned item's `body` is omitted — set this once
    /// you only need enough of each row to decide which item (if any) to
    /// fetch in full next. See ADR-0014.
    #[serde(default)]
    summary: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
    /// Excludes archived items by default; `true` returns only archived
    /// items (explicit archive browse). See ADR-0013.
    #[serde(default)]
    archived: Option<bool>,
    /// Max rows returned, applied after every other filter. Server default
    /// 50, hard-capped at 200 — see ADR-0014. The tool result's `total`
    /// field reports how many rows matched before this cap, so you know
    /// whether to page with `offset`.
    #[serde(default)]
    limit: Option<usize>,
    /// Rows to skip before applying `limit`. Defaults to 0.
    #[serde(default)]
    offset: Option<usize>,
    /// When `true`, every returned item's `body` is omitted — set this once
    /// you only need enough of each row to decide which item (if any) to
    /// fetch in full next. See ADR-0014.
    #[serde(default)]
    summary: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClaimOrSubmitParams {
    item_id: String,
    worker_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApproveParams {
    item_id: String,
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReasonedParams {
    item_id: String,
    #[serde(default)]
    author: Option<String>,
    reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TagsParams {
    item_id: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetRequesterParams {
    item_id: String,
    /// The corrected requester identity. Must not be blank.
    requester: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTagsParams {
    /// Scope the vocabulary to items under this exact-match topic.
    #[serde(default)]
    topic: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddCommentParams {
    item_id: String,
    #[serde(default)]
    author: Option<String>,
    body: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
    requester: Option<String>,
    /// Was `owner` before ADR-0010; defaulted for the same reason as
    /// `requester`.
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    turn: Option<String>,
    /// Absent from servers older than ADR-0012. Defaulted for the same
    /// reason as `tags`/`turn` above. `Option<bool>` rather than a bare
    /// `bool` with a `false` default: an older server's response genuinely
    /// doesn't carry this fact at all, and defaulting it to `false` would
    /// misrepresent every non-closed item from an old server as closed.
    /// `None` means "server didn't say," distinct from `Some(false)`
    /// meaning "server said closed."
    #[serde(default)]
    open: Option<bool>,
    /// `None` means either "not archived" or "server predates ADR-0013" —
    /// indistinguishable from this DTO alone, matching `turn`/`open`'s
    /// existing older-server-defaulting convention.
    #[serde(default)]
    archived_at: Option<i64>,
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
struct TopicCountDto {
    topic: String,
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

/// A transport-level failure (`docket-core` unreachable, connection reset
/// mid-response, DNS failure, timeout, ...) previously surfaced as
/// reqwest's raw wire-level error text verbatim — an opaque string that
/// doesn't distinguish "the server rejected this" from "the server was
/// never reached", which matters to a caller (often an LLM) deciding
/// whether retrying makes sense. Named and phrased as retry-worthy here
/// instead.
fn unreachable_error(e: reqwest::Error) -> McpError {
    McpError::internal_error(
        format!("could not reach docket-core (transient — retrying may help): {e}"),
        None,
    )
}

/// Turns a `docket-core` HTTP response into a tool result: a non-2xx status
/// becomes a tool-level error (the model sees it and can react — e.g. retry
/// `list_items` after losing a claim race) rather than a protocol error.
async fn respond<T: Serialize + for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<CallToolResult, McpError> {
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(unreachable_error)?;
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

#[derive(Serialize)]
struct PaginatedItems {
    items: Vec<ItemDto>,
    /// Rows matching the filters before `limit`/`offset` were applied — an
    /// MCP tool result has no header channel (unlike the HTTP response this
    /// is read from), so this is how a caller learns whether to page with
    /// `offset` instead of assuming `items` is everything. See ADR-0014.
    total: usize,
}

/// Same shape as `respond`, but for `list_items`/`search_items`: reads the
/// `X-Total-Count` header docket-core's HTTP layer sets (ADR-0014) before
/// consuming the body, and re-wraps the bare `ItemDto[]` as `{items,
/// total}` — the one output shape an MCP caller can actually see.
async fn respond_paginated(resp: reqwest::Response) -> Result<CallToolResult, McpError> {
    let status = resp.status();
    let total = resp
        .headers()
        .get("X-Total-Count")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let bytes = resp.bytes().await.map_err(unreachable_error)?;
    if status.is_success() {
        let items: Vec<ItemDto> = serde_json::from_slice(&bytes)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        // Falls back to the page length rather than failing the whole call
        // if the header is ever missing/malformed — a caller still gets a
        // correct, if unconfirmed-total, result instead of an opaque error.
        let total = total.unwrap_or(items.len());
        let block = ContentBlock::json(&PaginatedItems { items, total })?;
        Ok(CallToolResult::success(vec![block]))
    } else {
        let message = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|b| b.error)
            .unwrap_or_else(|_| format!("docket-core returned {status}"));
        Ok(CallToolResult::error(vec![ContentBlock::text(message)]))
    }
}

/// Adds an `author` field to a request body when the caller supplied one —
/// every mutation that records authorship (approve/reject/reopen/
/// add_comment) follows the same optional-author convention, docket-core
/// defaulting to `"unknown"` when it's omitted.
fn with_optional_author(mut body: serde_json::Value, author: Option<String>) -> serde_json::Value {
    if let Some(author) = author {
        body["author"] = serde_json::Value::String(author);
    }
    body
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
            .map_err(unreachable_error)?;
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
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "List items, optionally filtered by topic, state, the worker currently assigned (assignee), the requester, a worker's topic jurisdiction (topic_scope), and/or archived status. Paginated via limit/offset — check the result's total field. Pass summary=true to omit each item's body when you only need enough to pick which one to fetch in full next"
    )]
    async fn list_items(
        &self,
        Parameters(p): Parameters<ListItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        let archived = p.archived.map(|a| a.to_string());
        let limit = p.limit.map(|l| l.to_string());
        let offset = p.offset.map(|o| o.to_string());
        let summary = p.summary.map(|s| s.to_string());
        let resp = self
            .http
            .get(format!("{}/items", self.base_url))
            .query(&[
                ("topic", p.topic.as_deref()),
                ("state", p.state.as_deref()),
                ("assignee", p.assignee.as_deref()),
                ("requester", p.requester.as_deref()),
                ("topic_scope", p.topic_scope.as_deref()),
                ("archived", archived.as_deref()),
                ("limit", limit.as_deref()),
                ("offset", offset.as_deref()),
                ("summary", summary.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        respond_paginated(resp).await
    }

    #[tool(
        description = "Search items by full-text query and/or tags — call this before create_item to check whether a matching issue already exists. Pass summary=true to omit each item's body when you only need enough to pick which one to fetch in full next"
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
        let archived = p.archived.map(|a| a.to_string());
        if let Some(a) = archived.as_deref() {
            query_pairs.push(("archived", a));
        }
        let limit = p.limit.map(|l| l.to_string());
        if let Some(l) = limit.as_deref() {
            query_pairs.push(("limit", l));
        }
        let offset = p.offset.map(|o| o.to_string());
        if let Some(o) = offset.as_deref() {
            query_pairs.push(("offset", o));
        }
        let summary = p.summary.map(|s| s.to_string());
        if let Some(s) = summary.as_deref() {
            query_pairs.push(("summary", s));
        }
        let resp = self
            .http
            .get(format!("{}/items", self.base_url))
            .query(&query_pairs)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond_paginated(resp).await
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
            .map_err(unreachable_error)?;
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
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Approve a resolved item as the requester, closing it with resolution=done"
    )]
    async fn approve_item(
        &self,
        Parameters(p): Parameters<ApproveParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = with_optional_author(serde_json::json!({}), p.author);
        let resp = self
            .http
            .post(format!("{}/items/{}/approve", self.base_url, p.item_id))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Reject a resolved item, sending it back to the assignee for rework. \
            Requires a reason, recorded as a comment atomically with the state change."
    )]
    async fn reject_item(
        &self,
        Parameters(p): Parameters<ReasonedParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = with_optional_author(serde_json::json!({ "reason": p.reason }), p.author);
        let resp = self
            .http
            .post(format!("{}/items/{}/reject", self.base_url, p.item_id))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Reopen a closed item that was closed prematurely or turns out not to be \
            finished. Puts it back in front of the assignee side and clears resolution — back to \
            claimed if it still has an assignee, back to open if it never had one. Requires a \
            reason, recorded as a comment atomically with the state change."
    )]
    async fn reopen_item(
        &self,
        Parameters(p): Parameters<ReasonedParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = with_optional_author(serde_json::json!({ "reason": p.reason }), p.author);
        let resp = self
            .http
            .post(format!("{}/items/{}/reopen", self.base_url, p.item_id))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Archive an item — hides it from default list_items/search_items results \
            (still fully queryable with archived=true). Idempotent. Does not lose any data; \
            there is currently no unarchive operation."
    )]
    async fn archive_item(
        &self,
        Parameters(p): Parameters<ListCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(format!("{}/items/{}/archive", self.base_url, p.item_id))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Set requester on an item that doesn't have one yet — the one way to \
            correct an item filed before a requester identity was available, or one a \
            migration left blank. State-independent (works on a closed item too — this \
            corrects metadata, it isn't a workflow transition). Does not cover assignee/turn \
            or title/body/topic; those have no edit path yet."
    )]
    async fn set_item_requester(
        &self,
        Parameters(p): Parameters<SetRequesterParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .patch(format!("{}/items/{}", self.base_url, p.item_id))
            .json(&serde_json::json!({ "requester": p.requester }))
            .send()
            .await
            .map_err(unreachable_error)?;
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
            .map_err(unreachable_error)?;
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
            .map_err(unreachable_error)?;
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
            .map_err(unreachable_error)?;
        respond::<Vec<TagCountDto>>(resp).await
    }

    #[tool(
        description = "List existing topics and how many non-archived items sit under each, most-populated first — call this before list_items/search_items to discover topic names instead of guessing"
    )]
    async fn list_topics(&self) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .get(format!("{}/topics", self.base_url))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<Vec<TopicCountDto>>(resp).await
    }

    #[tool(
        description = "Add a follow-up note to an item — upstream replies, extra repro info, release notices"
    )]
    async fn add_comment(
        &self,
        Parameters(p): Parameters<AddCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = with_optional_author(serde_json::json!({ "body": p.body }), p.author);
        let resp = self
            .http
            .post(format!("{}/items/{}/comments", self.base_url, p.item_id))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
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
            .map_err(unreachable_error)?;
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

    /// `field()` panics on non-string values (`as_str()` returns `None` for
    /// bools and `null`), so non-string fields (`open`, and `resolution`
    /// once it goes back to `null` after a reopen) go through this instead.
    fn json_value(result: &CallToolResult) -> serde_json::Value {
        serde_json::from_str(text_of(result)).expect("tool result is JSON")
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
                requester: None,
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
                assignee: None,
                requester: None,
                topic_scope: Some("w1".to_string()),
                archived: None,
                limit: None,
                offset: None,
                summary: None,
            }))
            .await
            .unwrap();
        assert_ne!(listed.is_error, Some(true));
        let listed_value: serde_json::Value = serde_json::from_str(text_of(&listed)).unwrap();
        assert_eq!(listed_value["total"], 1);
        assert_eq!(listed_value["items"].as_array().unwrap().len(), 1);

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
                author: None,
            }))
            .await
            .unwrap();
        assert_eq!(field(&approved, "state"), "closed");
        assert_eq!(field(&approved, "resolution"), "done");
    }

    /// Extends the M1 lifecycle one round further (ADR-0012): a resolved
    /// item can be rejected back to its assignee (state reverts to claimed,
    /// item stays open) and a closed item can be reopened (state reverts to
    /// claimed, resolution clears back to null) — round-tripping through
    /// both new transitions exactly as an MCP client would call them.
    #[tokio::test]
    async fn reject_then_resubmit_then_approve_round_trips() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-reject-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("reject.db");
        let core = spawn_core(18425, &db_path).await;
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
                requester: Some("requester-1".to_string()),
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w1".to_string(),
            }))
            .await
            .unwrap();
        server
            .submit_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w1".to_string(),
            }))
            .await
            .unwrap();

        let rejected = server
            .reject_item(Parameters(ReasonedParams {
                item_id: item_id.clone(),
                author: Some("requester-1".to_string()),
                reason: "missing tests".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&rejected, "state"), "claimed");
        assert_eq!(json_value(&rejected)["open"], serde_json::json!(true));

        server
            .submit_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: "w1".to_string(),
            }))
            .await
            .unwrap();

        let approved = server
            .approve_item(Parameters(ApproveParams {
                item_id: item_id.clone(),
                author: Some("requester-1".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(field(&approved, "state"), "closed");
        assert_eq!(json_value(&approved)["open"], serde_json::json!(false));

        let reopened = server
            .reopen_item(Parameters(ReasonedParams {
                item_id: item_id.clone(),
                author: Some("requester-1".to_string()),
                reason: "regression found".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&reopened, "state"), "claimed");
        assert!(json_value(&reopened)["resolution"].is_null());
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
                requester: None,
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
        let err = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "t".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap_err();
        // A clear, retry-worthy message instead of a bare reqwest error
        // string — see `unreachable_error`.
        assert!(err.message.contains("could not reach docket-core"));
    }

    /// A stale/misremembered field name (e.g. `owned_by`, the pre-ADR-0010
    /// name for what's now `assignee`/`requester`/`topic_scope`) used to
    /// deserialize successfully with the field silently dropped — a filter
    /// caller couldn't tell "no matches" from "you misspelled the filter".
    /// `deny_unknown_fields` on every params struct turns that into a
    /// visible error.
    #[test]
    fn list_items_params_rejects_an_unknown_field() {
        let err =
            serde_json::from_value::<ListItemsParams>(serde_json::json!({ "owned_by": null }))
                .unwrap_err();
        assert!(err.to_string().contains("owned_by"));
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
                requester: None,
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
                requester: None,
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/node-packages".to_string(),
                title: "unrelated".to_string(),
                body: None,
                tags: vec![],
                requester: None,
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
                archived: None,
                limit: None,
                offset: None,
                summary: None,
            }))
            .await
            .unwrap();
        let found_value: serde_json::Value = serde_json::from_str(text_of(&found)).unwrap();
        assert_eq!(found_value["total"], 1);
        assert_eq!(found_value["items"].as_array().unwrap().len(), 1);
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
                requester: None,
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

    #[tokio::test]
    async fn archive_item_hides_from_default_list_items_tool() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("archive.db");
        let core = spawn_core(18426, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "archive-me".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        server
            .archive_item(Parameters(ListCommentsParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();

        let default_list = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
            }))
            .await
            .unwrap();
        assert!(
            !json_value(&default_list)["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == item_id)
        );

        let archive_view = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                archived: Some(true),
                limit: None,
                offset: None,
                summary: None,
            }))
            .await
            .unwrap();
        assert!(
            json_value(&archive_view)["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == item_id)
        );
    }

    #[tokio::test]
    async fn list_topics_returns_counts_by_topic() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-topics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("topics.db");
        let core = spawn_core(18427, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        for _ in 0..2 {
            server
                .create_item(Parameters(CreateItemParams {
                    topic: "iyulab/docket".to_string(),
                    title: "t".to_string(),
                    body: None,
                    tags: vec![],
                    requester: None,
                }))
                .await
                .unwrap();
        }

        let topics = server.list_topics().await.unwrap();
        assert_ne!(topics.is_error, Some(true));
        let topics_value = json_value(&topics);
        let list = topics_value.as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["topic"], "iyulab/docket");
        assert_eq!(list[0]["count"], 2);
    }

    /// A caller can page through a filtered result and trust `total`
    /// against the unpaged count — the regression this guards is
    /// `limit`/`offset` being dropped somewhere between the MCP params and
    /// the HTTP query string.
    #[tokio::test]
    async fn list_items_limit_and_offset_are_forwarded_and_total_reflects_the_unpaged_count() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-paging-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("paging.db");
        let core = spawn_core(18428, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        for i in 0..5 {
            server
                .create_item(Parameters(CreateItemParams {
                    topic: "iyulab/docket".to_string(),
                    title: format!("item-{i}"),
                    body: None,
                    tags: vec![],
                    requester: None,
                }))
                .await
                .unwrap();
        }

        let page = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                archived: None,
                limit: Some(2),
                offset: Some(1),
                summary: None,
            }))
            .await
            .unwrap();
        let page_value = json_value(&page);
        assert_eq!(page_value["total"], 5);
        assert_eq!(page_value["items"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_items_summary_true_omits_body() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-summary-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("summary.db");
        let core = spawn_core(18429, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "t".to_string(),
                body: Some("long body text".to_string()),
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let result = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                archived: None,
                limit: None,
                offset: None,
                summary: Some(true),
            }))
            .await
            .unwrap();
        let value = json_value(&result);
        assert_eq!(value["items"][0]["body"], serde_json::Value::Null);
        assert_eq!(value["items"][0]["title"], "t");
    }

    #[tokio::test]
    async fn set_item_requester_backfills_a_blank_requester() {
        let dir = std::env::temp_dir().join(format!(
            "docket-mcp-test-set-requester-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set-requester.db");
        let core = spawn_core(18430, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "filed without a requester".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");
        assert_eq!(json_value(&created)["requester"], serde_json::Value::Null);

        let updated = server
            .set_item_requester(Parameters(SetRequesterParams {
                item_id: item_id.clone(),
                requester: "backfilled-reporter".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(json_value(&updated)["requester"], "backfilled-reporter");

        let refetched = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: None,
                requester: Some("backfilled-reporter".to_string()),
                topic_scope: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
            }))
            .await
            .unwrap();
        assert!(
            json_value(&refetched)["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == item_id)
        );

        let rejected = server
            .set_item_requester(Parameters(SetRequesterParams {
                item_id,
                requester: "   ".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(rejected.is_error, Some(true));
    }
}
