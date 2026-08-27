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

/// Builds `{base_url}/items/{item_id}[/{suffix...}]`, percent-encoding
/// `item_id` (and any suffix segments) via `Url::path_segments_mut` instead
/// of raw `format!` string interpolation. `item_id` may be a `#`-prefixed
/// seq alias (ADR-0016), and `#` is the URL fragment delimiter — a naive
/// `format!("{base}/items/{item_id}")` string, once handed to reqwest and
/// parsed as a URL, silently drops everything from `#` onward *before the
/// request is ever sent*, turning `#142` into a request for an empty id
/// (docket-core then 404s) instead of the intended lookup. Building through
/// `Url` keeps every reserved character correctly encoded, not just `#`.
fn items_url(base_url: &str, item_id: &str, suffix: &[&str]) -> reqwest::Url {
    let mut url = reqwest::Url::parse(base_url).expect("base_url is a valid absolute URL");
    {
        let mut segments = url
            .path_segments_mut()
            .expect("http(s) base_url has path segments");
        segments.push("items").push(item_id);
        for s in suffix {
            segments.push(s);
        }
    }
    url
}

// Every tool-parameter struct below denies unknown fields — a caller
// guessing a stale or misremembered field name (e.g. `owned_by`, the
// pre-ADR-0010 name for what's now `assignee`/`requester`/`topic_scope`)
// otherwise deserializes successfully with that field silently dropped,
// which for a filter parameter reads as "no matching items" rather than
// "you misspelled the filter". A rejected-field error is far more
// actionable than a quietly-empty result.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RegisterWorkerParams {
    /// Unique id for this worker. Omit to use this session's
    /// `DOCKET_WORKER_ID` (see `resolve_identity`) — required, explicitly or
    /// via that fallback (cycle-58, same pattern as claim_item/add_comment/
    /// etc., HD-16/HD-17).
    #[serde(default)]
    id: Option<String>,
    /// Topic prefixes this worker owns (see docs/glossary.md "topic").
    #[serde(default)]
    topics: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetWorkerParams {
    /// The worker id to look up. Omit to look up this session's own
    /// registration via `DOCKET_WORKER_ID` (see `resolve_identity`) — HD-18
    /// (cycle-59), same fallback pattern as register_worker/claim_item/etc.
    #[serde(default)]
    id: Option<String>,
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
    /// A worker id — the one-shot "what should I be looking at" filter:
    /// matches items this worker is the `assignee` of, OR items this worker
    /// filed (`requester`) that are now `resolved` and waiting on its
    /// approval, OR `open` (unclaimed) items under a topic this worker is
    /// registered for (see docket-works#35 — the topic-jurisdiction test is
    /// the same one `topic_scope` uses). ANDs with every other filter here,
    /// same as `assignee`/`requester` individually. Prefer this over
    /// manually combining `assignee`, `requester`+`state=resolved`, and
    /// `topic_scope`+`state=open` yourself.
    #[serde(default)]
    mine: Option<String>,
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
    /// "asc" or "desc", sorting by `updated_at`. Defaults to "desc"
    /// (most-recently-touched first, today's fixed behavior); pass "asc"
    /// to find the longest-untouched items directly instead of paging to
    /// the tail via `offset`. See ADR-0020.
    #[serde(default)]
    order: Option<String>,
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
    /// A worker id — narrows results to items whose `assignee` (current
    /// holder) is exactly this worker. Same semantics as `list_items`'s
    /// field of the same name.
    #[serde(default)]
    assignee: Option<String>,
    /// Exact-match on `requester` — symmetric to `assignee`, above.
    #[serde(default)]
    requester: Option<String>,
    /// A registered worker id — narrows results to items under any topic
    /// that worker is registered for (prefix match). Same semantics as
    /// `list_items`'s field of the same name.
    #[serde(default)]
    topic_scope: Option<String>,
    /// A worker id — the one-shot "what do I currently hold" filter. Same
    /// semantics as `list_items`'s field of the same name.
    #[serde(default)]
    mine: Option<String>,
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
    /// "asc" or "desc", sorting by `updated_at`. Same semantics as
    /// `list_items`'s field of the same name. See ADR-0020.
    #[serde(default)]
    order: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClaimOrSubmitParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Omit to use this session's `DOCKET_WORKER_ID` (see `resolve_identity`)
    /// — required, explicitly or via that fallback, so a claim can always be
    /// traced to a worker.
    #[serde(default)]
    worker_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApproveParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Omit to use this session's `DOCKET_WORKER_ID` (see `resolve_identity`)
    /// — required, explicitly or via that fallback (HD-16/HD-17, cycle-57).
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReasonedParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Omit to use this session's `DOCKET_WORKER_ID` (see `resolve_identity`)
    /// — required, explicitly or via that fallback (HD-16/HD-17, cycle-57).
    #[serde(default)]
    author: Option<String>,
    reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TagsParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetRequesterParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
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
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Who's writing this comment. Omit to use this session's
    /// `DOCKET_WORKER_ID` (see `resolve_identity`) — required, explicitly or
    /// via that fallback, so the thread never lands docket-core's
    /// `"unknown"` default (Issue #30). Schema-optional (unlike the original
    /// cycle-55 shape) precisely so an omission can still resolve through
    /// the env var instead of failing at deserialization.
    #[serde(default)]
    author: Option<String>,
    body: String,
}

/// Shared by every tool whose only input is an item id (`list_comments`,
/// `archive_item`). `get_item` has its own (`GetItemParams`, below) since it
/// alone also takes `expand_related`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ItemIdParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetItemParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item.
    item_id: String,
    /// When `true`, also resolves the item's `related:<id>` tags (both
    /// directions) into the response's `related` field. Defaults to
    /// `false` (no `related` field at all). See
    /// [docket-works#33](https://github.com/iyulab/docket-works/issues/33).
    #[serde(default)]
    expand_related: Option<bool>,
}

/// Mirrors `docket-core`'s item JSON shape. A separate type from
/// `docket_core::Item` on purpose — see the module doc.
#[derive(Debug, Serialize, Deserialize)]
struct ItemDto {
    id: String,
    /// Absent from servers older than ADR-0016. Defaulted (to 0, never a
    /// real item's value — seq starts at 1) for the same older-server
    /// reason as `tags`/`turn`/`open` above.
    #[serde(default)]
    seq: i64,
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
    /// Only present when `get_item` was called with `expand_related=true`
    /// against a server that supports it — omitted (not `null`/`[]`)
    /// otherwise, same older-server-defaulting convention as `tags` above.
    /// See [docket-works#33](https://github.com/iyulab/docket-works/issues/33).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    related: Option<Vec<RelatedItemRefDto>>,
}

/// Mirrors `docket-core`'s `RelatedItemRef` JSON shape (docket-works#33).
#[derive(Debug, Serialize, Deserialize)]
struct RelatedItemRefDto {
    id: String,
    seq: i64,
    title: String,
    /// `"references"` or `"referenced_by"`.
    relation: String,
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

/// Resolves a caller identity (an `author` or `worker_id` tool argument)
/// against an explicit value or, when the caller omitted it, this process's
/// `DOCKET_WORKER_ID` (`docket-cc-launcher` injects it every session) —
/// HD-16/HD-17 (cycle-57, extended cycle-58): identity is required at this
/// layer for `register_worker`/`claim_item`/`submit_item`/`approve_item`/
/// `reject_item`/`reopen_item`/`add_comment`, but "required" means
/// *resolvable*, not "the caller must type it every time". A blank/
/// whitespace-only value counts as absent, the
/// same treatment `SetRequesterParams.requester` and `ReasonedParams.reason`
/// already get. `env_fallback` is threaded in by the caller (rather than
/// read here via `std::env::var`) purely so this stays a pure function —
/// see `resolve_identity_*` unit tests, which exercise it without touching
/// real process env state.
///
/// Returns a tool-level error (`CallToolResult::error`, visible to the
/// calling model) rather than `Err(McpError)` when neither resolves — a
/// protocol-level error would not be, per `claim_conflict_is_tool_level_error`.
fn resolve_identity(
    explicit: Option<String>,
    env_fallback: Option<String>,
    field: &str,
) -> Result<String, CallToolResult> {
    explicit
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env_fallback.filter(|s| !s.trim().is_empty()))
        .ok_or_else(|| {
            CallToolResult::error(vec![ContentBlock::text(format!(
                "{field} is required — pass it explicitly or set DOCKET_WORKER_ID"
            ))])
        })
}

fn docket_worker_id() -> Option<String> {
    std::env::var("DOCKET_WORKER_ID").ok()
}

fn with_author(mut body: serde_json::Value, author: String) -> serde_json::Value {
    body["author"] = serde_json::Value::String(author);
    body
}

#[tool_router(server_handler)]
impl DocketMcp {
    #[tool(
        description = "Register as a worker, reporting which topic prefixes you own. id may be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn register_worker(
        &self,
        Parameters(p): Parameters<RegisterWorkerParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match resolve_identity(p.id, docket_worker_id(), "id") {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .post(format!("{}/workers", self.base_url))
            .json(&serde_json::json!({ "id": id, "topics": p.topics }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<WorkerDto>(resp).await
    }

    #[tool(
        description = "Fetch a worker's registration — its topics and online status. id may be omitted to look up this session's own registration via DOCKET_WORKER_ID. The only way to positively confirm what you're currently registered as (topic_scope/mine treat an unknown worker id the same as one with no matching topics: an empty result, not an error)"
    )]
    async fn get_worker(
        &self,
        Parameters(p): Parameters<GetWorkerParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = match resolve_identity(p.id, docket_worker_id(), "id") {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .get(format!("{}/workers/{}", self.base_url, id))
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
        description = "List items, optionally filtered by topic, state, the worker currently assigned (assignee), the requester, a worker's topic jurisdiction (topic_scope), what a worker should currently be paying attention to (mine — assignee OR resolved-and-awaiting-my-approval OR open-and-unclaimed within a topic this worker is registered for), and/or archived status. `mine` alone covers the full \"what do I need to look at\" set — prefer it over combining assignee/requester/topic_scope yourself, since an unclaimed item in your own topic is otherwise easy to miss (see docket-works#35). Paginated via limit/offset — check the result's total field. Pass summary=true to omit each item's body when you only need enough to pick which one to fetch in full next. Ordered by updated_at descending (most-recently-touched first) by default — pass order=\"asc\" to find the longest-untouched items directly instead of paging to the tail via offset"
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
                ("mine", p.mine.as_deref()),
                ("archived", archived.as_deref()),
                ("limit", limit.as_deref()),
                ("offset", offset.as_deref()),
                ("summary", summary.as_deref()),
                ("order", p.order.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        respond_paginated(resp).await
    }

    #[tool(
        description = "Search items by full-text query and/or tags — call this before create_item to check whether a matching issue already exists. Combinable with the same ownership filters list_items offers (assignee/requester/topic_scope/mine). Pass summary=true to omit each item's body when you only need enough to pick which one to fetch in full next. Same order semantics as list_items (default updated_at descending, order=\"asc\" to reverse)"
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
        if let Some(a) = p.assignee.as_deref() {
            query_pairs.push(("assignee", a));
        }
        if let Some(r) = p.requester.as_deref() {
            query_pairs.push(("requester", r));
        }
        if let Some(t) = p.topic_scope.as_deref() {
            query_pairs.push(("topic_scope", t));
        }
        if let Some(m) = p.mine.as_deref() {
            query_pairs.push(("mine", m));
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
        if let Some(o) = p.order.as_deref() {
            query_pairs.push(("order", o));
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
        description = "Claim an open item — exclusive, only one worker can win a race for the same item. Call this before starting any work: it's the only thing that moves turn off its default; add_comment never does. worker_id may be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn claim_item(
        &self,
        Parameters(p): Parameters<ClaimOrSubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        let worker_id = match resolve_identity(p.worker_id, docket_worker_id(), "worker_id") {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["claim"]))
            .json(&serde_json::json!({ "worker_id": worker_id }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Submit a claimed item as done, moving it to resolved for the requester to approve. worker_id may be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn submit_item(
        &self,
        Parameters(p): Parameters<ClaimOrSubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        let worker_id = match resolve_identity(p.worker_id, docket_worker_id(), "worker_id") {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["submit"]))
            .json(&serde_json::json!({ "worker_id": worker_id }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Approve a resolved item as the requester, closing it with resolution=done. \
            If the item has a requester set, author must match it or the call fails — use \
            set_item_requester to correct a drifted identity. author may be omitted if this \
            session's DOCKET_WORKER_ID is set"
    )]
    async fn approve_item(
        &self,
        Parameters(p): Parameters<ApproveParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let body = with_author(serde_json::json!({}), author);
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["approve"]))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Reject a resolved item, sending it back to the assignee for rework. \
            Requires a reason, recorded as a comment atomically with the state change. If the \
            item has a requester set, author must match it or the call fails — use \
            set_item_requester to correct a drifted identity. author may be omitted if this \
            session's DOCKET_WORKER_ID is set"
    )]
    async fn reject_item(
        &self,
        Parameters(p): Parameters<ReasonedParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let body = with_author(serde_json::json!({ "reason": p.reason }), author);
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["reject"]))
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
            reason, recorded as a comment atomically with the state change. author may be \
            omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn reopen_item(
        &self,
        Parameters(p): Parameters<ReasonedParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let body = with_author(serde_json::json!({ "reason": p.reason }), author);
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["reopen"]))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Park an item that cannot progress right now because of a concrete \
            external dependency (e.g. no access to a paywalled standard, waiting on a third \
            party) — closes it with resolution=blocked. Unlike remove/merge/force-close/\
            force-approve this is a normal, fully reversible worker judgment call, not an admin \
            override: reopen_item is the way back once the dependency clears. Requires a reason, \
            recorded as a comment atomically with the state change — it is the only record of \
            why for whoever reopens it later. author may be omitted if this session's \
            DOCKET_WORKER_ID is set"
    )]
    async fn block_item(
        &self,
        Parameters(p): Parameters<ReasonedParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let body = with_author(serde_json::json!({ "reason": p.reason }), author);
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["block"]))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Park an item that is intentionally not being worked right now for a \
            reason short of a hard external block (e.g. cross-consumer demand not yet proven) — \
            closes it with resolution=deferred. Same shape as block_item: a normal, fully \
            reversible worker judgment call, reversed with reopen_item. Requires a reason, \
            recorded as a comment atomically with the state change. author may be omitted if \
            this session's DOCKET_WORKER_ID is set"
    )]
    async fn defer_item(
        &self,
        Parameters(p): Parameters<ReasonedParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let body = with_author(serde_json::json!({ "reason": p.reason }), author);
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["defer"]))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Fetch a single item by id — the way to resolve an id from a shared link \
            or a comment into its current state/resolution/requester/assignee/turn/tags/body. \
            item_id accepts either the canonical id or the item's short numeric alias (seq, e.g. \
            142 or #142) — both returned on every item. Unaffected by list_items/search_items' \
            summary mode (body always included) and returns archived items too (get_item is a \
            direct id lookup, not a list query). Pass expand_related=true to also resolve any \
            related:<id> tags (both directions — this item referencing another, or another \
            item referencing this one back) into a related field, instead of having to \
            dereference each related:<id> tag yourself with a separate get_item call."
    )]
    async fn get_item(
        &self,
        Parameters(p): Parameters<GetItemParams>,
    ) -> Result<CallToolResult, McpError> {
        let expand_related = p.expand_related.map(|b| b.to_string());
        let resp = self
            .http
            .get(items_url(&self.base_url, &p.item_id, &[]))
            .query(&[("expand_related", expand_related.as_deref())])
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
        Parameters(p): Parameters<ItemIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["archive"]))
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
            .patch(items_url(&self.base_url, &p.item_id, &[]))
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
            .post(items_url(&self.base_url, &p.item_id, &["tags"]))
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
            .delete(items_url(&self.base_url, &p.item_id, &["tags"]))
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
        description = "Add a follow-up note to an item — upstream replies, extra repro info, release notices. Never changes state or turn — narrating a whole workflow through comments alone leaves the item exactly where claim_item/submit_item last left it. `author` is required — identify yourself (your worker id, or another stable caller identity) so the thread stays readable instead of filling up with docket-core's \"unknown\" fallback. May be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn add_comment(
        &self,
        Parameters(p): Parameters<AddCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let body = serde_json::json!({ "body": p.body, "author": author });
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["comments"]))
            .json(&body)
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<CommentDto>(resp).await
    }

    #[tool(description = "List an item's comment thread in chronological order")]
    async fn list_comments(
        &self,
        Parameters(p): Parameters<ItemIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let resp = self
            .http
            .get(items_url(&self.base_url, &p.item_id, &["comments"]))
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
                id: Some("w1".to_string()),
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
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
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
                worker_id: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(claimed.is_error, Some(true));
        assert_eq!(field(&claimed, "state"), "claimed");

        let submitted = server
            .submit_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(field(&submitted, "state"), "resolved");

        let approved = server
            .approve_item(Parameters(ApproveParams {
                item_id: item_id.clone(),
                // Explicit — the omitted-with-no-fallback case is HD-16/
                // HD-17's own dedicated coverage
                // (`resolve_identity_errors_as_tool_level_when_both_absent`,
                // `add_comment_without_author_resolves_via_env_var_or_errors`),
                // not this happy-path lifecycle test's concern.
                author: Some("requester-1".to_string()),
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
                worker_id: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        server
            .submit_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("w1".to_string()),
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
                worker_id: Some("w1".to_string()),
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

    /// `block_item`/`defer_item` (ADR-0018) — a worker's own reversible
    /// judgment call, MCP-exposed unlike the admin closes — closes from any
    /// pre-closed state with `resolution=blocked`/`deferred`, and
    /// `reopen_item` is the way back, same as for any other closed item.
    #[tokio::test]
    async fn block_and_defer_close_and_reopen_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "docket-mcp-test-block-defer-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("block-defer.db");
        let core = spawn_core(18436, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let blocked_item = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "blocked one".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let blocked_id = field(&blocked_item, "id");

        let blocked = server
            .block_item(Parameters(ReasonedParams {
                item_id: blocked_id.clone(),
                author: Some("w1".to_string()),
                reason: "no access to the primary standard".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&blocked, "state"), "closed");
        assert_eq!(field(&blocked, "resolution"), "blocked");
        assert!(json_value(&blocked)["turn"].is_null());

        let reopened = server
            .reopen_item(Parameters(ReasonedParams {
                item_id: blocked_id.clone(),
                author: Some("w1".to_string()),
                reason: "standard obtained".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&reopened, "state"), "open");
        assert!(json_value(&reopened)["resolution"].is_null());

        let deferred_item = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "deferred one".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let deferred_id = field(&deferred_item, "id");

        let deferred = server
            .defer_item(Parameters(ReasonedParams {
                item_id: deferred_id.clone(),
                author: Some("w2".to_string()),
                reason: "cross-consumer demand not yet proven".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&deferred, "state"), "closed");
        assert_eq!(field(&deferred, "resolution"), "deferred");
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
                worker_id: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(first.is_error, Some(true));

        let second = server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("w2".to_string()),
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

    /// `AddCommentParams.author` is schema-optional (HD-16/HD-17, cycle-57)
    /// specifically so an omitted value can still resolve through
    /// `DOCKET_WORKER_ID` rather than failing to deserialize at all — the
    /// hard-failure guarantee Issue #30 wanted moved from the schema layer
    /// to `resolve_identity`, exercised below.
    #[test]
    fn add_comment_params_accepts_missing_author() {
        let parsed: AddCommentParams =
            serde_json::from_value(serde_json::json!({ "item_id": "x", "body": "hi" })).unwrap();
        assert_eq!(parsed.author, None);
    }

    /// `resolve_identity` — the shared fallback behind HD-16 (`author`
    /// required on `approve_item`/`reject_item`/`reopen_item`, matching
    /// `add_comment`'s cycle-55 treatment) and HD-17 (resolvable via
    /// `DOCKET_WORKER_ID` instead of a hard schema failure). Exercised as a
    /// pure function — `env_fallback` is passed in rather than read from
    /// real process env, so these cases can't race with any other test that
    /// happens to run concurrently in the same binary.
    #[test]
    fn resolve_identity_prefers_explicit_over_env_fallback() {
        let resolved = resolve_identity(
            Some("explicit".to_string()),
            Some("from-env".to_string()),
            "author",
        );
        assert_eq!(resolved.unwrap(), "explicit");
    }

    #[test]
    fn resolve_identity_falls_back_to_env_when_omitted() {
        let resolved = resolve_identity(None, Some("from-env".to_string()), "author");
        assert_eq!(resolved.unwrap(), "from-env");
    }

    #[test]
    fn resolve_identity_errors_as_tool_level_when_both_absent() {
        let error = resolve_identity(None, None, "author").unwrap_err();
        assert_eq!(error.is_error, Some(true));
        let message = text_of(&error);
        assert!(message.contains("author"));
        assert!(message.contains("DOCKET_WORKER_ID"));
    }

    /// Blank/whitespace-only counts as absent on both sides — a caller
    /// passing `author: ""` (or an env var left set to an empty string)
    /// must not silently resolve to an empty identity.
    #[test]
    fn resolve_identity_treats_blank_as_absent() {
        let result = resolve_identity(Some("   ".to_string()), Some("".to_string()), "author");
        assert!(result.unwrap_err().is_error == Some(true));
    }

    /// End-to-end through the real handlers (not just `resolve_identity`
    /// directly): `add_comment`/`register_worker`/`get_worker` read
    /// `DOCKET_WORKER_ID` from this process's actual environment when
    /// `author`/`id` are omitted. `set_var`/`remove_var` mutate global
    /// process state, which is
    /// normally unsafe to do in a parallel test binary — safe here only
    /// because this is the one test in the suite that touches this specific
    /// env var (checked: no other test or non-test code reads
    /// `DOCKET_WORKER_ID`). Every handler that resolves identity through it
    /// is exercised here, in one test, rather than one test per handler —
    /// splitting would reintroduce the exact global-state race this comment
    /// warns against.
    #[tokio::test]
    async fn identity_fallback_resolves_via_env_var_across_tools_or_errors() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-workerid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("workerid.db");
        let core = spawn_core(18435, &db_path).await;
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

        // SAFETY (env-mutation caveat above): the only test touching this var.
        unsafe {
            std::env::remove_var("DOCKET_WORKER_ID");
        }
        let without_fallback = server
            .add_comment(Parameters(AddCommentParams {
                item_id: item_id.clone(),
                author: None,
                body: "no identity available".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(without_fallback.is_error, Some(true));

        let register_without_fallback = server
            .register_worker(Parameters(RegisterWorkerParams {
                id: None,
                topics: vec!["iyulab".to_string()],
            }))
            .await
            .unwrap();
        assert_eq!(register_without_fallback.is_error, Some(true));

        let get_worker_without_fallback = server
            .get_worker(Parameters(GetWorkerParams { id: None }))
            .await
            .unwrap();
        assert_eq!(get_worker_without_fallback.is_error, Some(true));

        unsafe {
            std::env::set_var("DOCKET_WORKER_ID", "env-worker");
        }
        let with_fallback = server
            .add_comment(Parameters(AddCommentParams {
                item_id: item_id.clone(),
                author: None,
                body: "identity from env".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(with_fallback.is_error, Some(true));
        assert_eq!(field(&with_fallback, "author"), "env-worker");

        let registered = server
            .register_worker(Parameters(RegisterWorkerParams {
                id: None,
                topics: vec!["iyulab".to_string()],
            }))
            .await
            .unwrap();
        assert_ne!(registered.is_error, Some(true));
        assert_eq!(field(&registered, "id"), "env-worker");

        let self_looked_up = server
            .get_worker(Parameters(GetWorkerParams { id: None }))
            .await
            .unwrap();
        assert_ne!(self_looked_up.is_error, Some(true));
        assert_eq!(field(&self_looked_up, "id"), "env-worker");

        unsafe {
            std::env::remove_var("DOCKET_WORKER_ID");
        }
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
                assignee: None,
                requester: None,
                topic_scope: None,
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
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
            .list_comments(Parameters(ItemIdParams {
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
            .archive_item(Parameters(ItemIdParams {
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
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
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
                mine: None,
                archived: Some(true),
                limit: None,
                offset: None,
                summary: None,
                order: None,
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
                mine: None,
                archived: None,
                limit: Some(2),
                offset: Some(1),
                summary: None,
                order: None,
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
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: Some(true),
                order: None,
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
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
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

    /// `get_worker` is the only positive-confirmation path for registration
    /// — round-trips a real registration and 404s a never-registered id,
    /// per docs/usage.md's read/write not-found asymmetry.
    #[tokio::test]
    async fn get_worker_confirms_registration_and_404s_when_unknown() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-get-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("get-worker.db");
        let core = spawn_core(18431, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("w1".to_string()),
                topics: vec!["iyulab".to_string()],
            }))
            .await
            .unwrap();

        let found = server
            .get_worker(Parameters(GetWorkerParams {
                id: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(found.is_error, Some(true));
        assert_eq!(json_value(&found)["id"], "w1");
        assert_eq!(json_value(&found)["topics"][0], "iyulab");

        let missing = server
            .get_worker(Parameters(GetWorkerParams {
                id: Some("never-registered".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(missing.is_error, Some(true));
    }

    /// `mine` ORs assignee and pending-approval-requester — verifies the
    /// filter is actually forwarded end to end through both `list_items`
    /// and `search_items` (docket-core's own test already covers the OR
    /// logic itself; this is a thin-wrapper parity check, same as every
    /// other filter in this file).
    #[tokio::test]
    async fn mine_filter_is_forwarded_through_list_items_and_search_items() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-mine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("mine.db");
        let core = spawn_core(18432, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let held = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "held by w1".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let held_id = field(&held, "id");
        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: held_id.clone(),
                worker_id: Some("w1".to_string()),
            }))
            .await
            .unwrap();

        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "held by someone else".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let listed = server
            .list_items(Parameters(ListItemsParams {
                topic: None,
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                mine: Some("w1".to_string()),
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
            }))
            .await
            .unwrap();
        let listed_items = json_value(&listed)["items"].as_array().unwrap().clone();
        assert_eq!(listed_items.len(), 1);
        assert_eq!(listed_items[0]["id"], held_id);

        let searched = server
            .search_items(Parameters(SearchItemsParams {
                query: None,
                tags: vec![],
                tag_match: None,
                topic: None,
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                mine: Some("w1".to_string()),
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
            }))
            .await
            .unwrap();
        let searched_items = json_value(&searched)["items"].as_array().unwrap().clone();
        assert_eq!(searched_items.len(), 1);
        assert_eq!(searched_items[0]["id"], held_id);
    }

    /// `order` is forwarded end to end through both `list_items` and
    /// `search_items`, not just dropped between the MCP params and the HTTP
    /// call (`docket-core`'s own tests already cover the SQL-level
    /// behavior). See ADR-0020.
    #[tokio::test]
    async fn order_is_forwarded_through_list_items_and_search_items() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("order.db");
        let core = spawn_core(18437, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let first = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "order-probe first".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let first_id = field(&first, "id");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "order-probe second".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let second_id = field(&second, "id");

        let listed_asc = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: Some("asc".to_string()),
            }))
            .await
            .unwrap();
        let listed_asc_items = json_value(&listed_asc)["items"].as_array().unwrap().clone();
        assert_eq!(listed_asc_items[0]["id"], first_id);
        assert_eq!(listed_asc_items[1]["id"], second_id);

        let searched_asc = server
            .search_items(Parameters(SearchItemsParams {
                query: Some("order-probe".to_string()),
                tags: vec![],
                tag_match: None,
                topic: None,
                state: None,
                assignee: None,
                requester: None,
                topic_scope: None,
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: Some("asc".to_string()),
            }))
            .await
            .unwrap();
        let searched_asc_items = json_value(&searched_asc)["items"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(searched_asc_items[0]["id"], first_id);
        assert_eq!(searched_asc_items[1]["id"], second_id);
    }

    /// `get_item` is the one-call path from an id (e.g. resolved out of a
    /// shared link) to the item's current state — round-trips a created
    /// item, 404s an unknown id, and confirms it still returns an archived
    /// item (unlike list_items/search_items, which hide archived by
    /// default).
    #[tokio::test]
    async fn get_item_round_trips_and_covers_archived_and_unknown() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-get-item-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("get-item.db");
        let core = spawn_core(18433, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "resolve me by id".to_string(),
                body: Some("full body".to_string()),
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let fetched = server
            .get_item(Parameters(GetItemParams {
                item_id: item_id.clone(),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert_ne!(fetched.is_error, Some(true));
        assert_eq!(json_value(&fetched)["id"], item_id);
        assert_eq!(json_value(&fetched)["body"], "full body");

        server
            .archive_item(Parameters(ItemIdParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();

        let fetched_archived = server
            .get_item(Parameters(GetItemParams {
                item_id: item_id.clone(),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert_ne!(fetched_archived.is_error, Some(true));
        assert_eq!(json_value(&fetched_archived)["id"], item_id);

        let missing = server
            .get_item(Parameters(GetItemParams {
                item_id: "never-created".to_string(),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert_eq!(missing.is_error, Some(true));
    }

    /// docket-works#33: `expand_related` is forwarded end to end through the
    /// real HTTP request (not just exercised against docket-core directly),
    /// and defaults to omitting `related` when unset.
    #[tokio::test]
    async fn get_item_expand_related_is_forwarded_through_the_real_request() {
        let dir = std::env::temp_dir().join(format!(
            "docket-mcp-test-expand-related-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("expand-related.db");
        let core = spawn_core(18438, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let a = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let a_id = field(&a, "id");

        let b = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "b".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let b_id = field(&b, "id");

        server
            .add_tags(Parameters(TagsParams {
                item_id: a_id.clone(),
                tags: vec![format!("related:{b_id}")],
            }))
            .await
            .unwrap();

        let without_expand = server
            .get_item(Parameters(GetItemParams {
                item_id: a_id.clone(),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert!(json_value(&without_expand).get("related").is_none());

        let with_expand = server
            .get_item(Parameters(GetItemParams {
                item_id: a_id.clone(),
                expand_related: Some(true),
            }))
            .await
            .unwrap();
        let with_expand_body = json_value(&with_expand);
        let related = with_expand_body["related"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["id"], b_id);
        assert_eq!(related[0]["relation"], "references");
    }

    /// `item_id` accepts the bare numeric `seq` alias, or the same value
    /// prefixed with `#` — both must resolve end to end through the real
    /// HTTP request docket-mcp sends, not just docket-core's own
    /// in-process resolver. The `#`-prefixed form specifically exercises a
    /// URL-fragment pitfall: `#` is the fragment delimiter in a URL, so a
    /// naive `format!("{base}/items/{item_id}")` that reqwest then parses
    /// as a URL could silently drop everything from `#` onward before the
    /// request is ever sent, turning `#142` into a lookup for an empty id
    /// instead of a 404 or a resolved item — a failure this MCP-level test
    /// would catch that a docket-core-only unit test cannot, since that
    /// layer never constructs a URL string from the alias at all.
    #[tokio::test]
    async fn get_item_accepts_seq_alias_bare_and_hash_prefixed() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-seq-alias-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("seq-alias.db");
        let core = spawn_core(18434, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "resolve me by seq".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");
        let seq = json_value(&created)["seq"]
            .as_i64()
            .expect("seq is a number");

        let by_bare_seq = server
            .get_item(Parameters(GetItemParams {
                item_id: seq.to_string(),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert_ne!(by_bare_seq.is_error, Some(true));
        assert_eq!(json_value(&by_bare_seq)["id"], item_id);

        let by_hash_seq = server
            .get_item(Parameters(GetItemParams {
                item_id: format!("#{seq}"),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert_ne!(
            by_hash_seq.is_error,
            Some(true),
            "error: {}",
            text_of(&by_hash_seq)
        );
        assert_eq!(json_value(&by_hash_seq)["id"], item_id);
    }
}
