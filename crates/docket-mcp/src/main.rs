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
/// Builds a `docket-core` URL by pushing each path segment through
/// `Url::path_segments_mut`, which percent-encodes it.
///
/// **Never build a request path with `format!`.** A value carrying a `/` or a
/// `#` would otherwise change the request's *shape* instead of travelling as
/// one segment — and the failure is silent rather than a 404: a two-segment
/// path misses its single-segment route, falls through to docket-core's static
/// console service, and comes back as a `200 text/html` that this tool then
/// fails to parse as JSON. `get_worker` hit exactly that, so every `org/repo`
/// worker id was unlookupable regardless of whether it was registered.
/// Both values are routine here: a worker id is conventionally `org/repo`, and an
/// item accepts its `seq` alias in `#142` form.
fn api_url(base_url: &str, segments: &[&str]) -> reqwest::Url {
    let mut url = reqwest::Url::parse(base_url).expect("base_url is a valid absolute URL");
    {
        let mut path = url
            .path_segments_mut()
            .expect("http(s) base_url has path segments");
        for segment in segments {
            path.push(segment);
        }
    }
    url
}

fn items_url(base_url: &str, item_id: &str, suffix: &[&str]) -> reqwest::Url {
    let mut segments = Vec::with_capacity(2 + suffix.len());
    segments.push("items");
    segments.push(item_id);
    segments.extend_from_slice(suffix);
    api_url(base_url, &segments)
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
struct ListEventsParams {
    /// The worker whose activity feed to read. Omit to use this session's
    /// `DOCKET_WORKER_ID` (see `resolve_identity`), same fallback every
    /// other worker-identity argument gets.
    #[serde(default)]
    for_worker: Option<String>,
    /// The cursor from a previous call's result, or omit/0 to start from
    /// the beginning. Always advance to the returned `cursor`, even when
    /// `events` came back empty -- that still means "caught up to here",
    /// not "nothing happened yet".
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListItemsParams {
    /// Topic filter — matches one topic, case-insensitively like every
    /// identifier comparison (ADR-0021).
    #[serde(default)]
    topic: Option<String>,
    /// One of open/claimed/resolved/closed.
    #[serde(default)]
    state: Option<String>,
    /// A worker id — narrows the list to items whose `assignee` (current
    /// holder) is this worker. Compared case-insensitively, like every
    /// identifier (ADR-0021).
    #[serde(default)]
    assignee: Option<String>,
    /// Matches the item's `requester` — symmetric to `assignee` above, and
    /// case-insensitive like it (ADR-0021).
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
    /// filed (`requester`) that are now `resolved` and waiting on *its*
    /// decision — approve it, or answer what the assignee asked and hand it
    /// back with `reject_item` (ADR-0010's 2026-09-08 update) — OR `open`
    /// (unclaimed) items under a topic this worker is registered for (the
    /// topic-jurisdiction test is the same one
    /// `topic_scope` uses). ANDs with every other filter here,
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
    /// When `true`, each returned item also resolves its own `related:<id>`
    /// tags (both directions) into a `related` field — the same expansion
    /// `get_item` offers for a single item, applied per row here after
    /// `limit`/`offset`, so the cost is bounded by the returned page, not
    /// the unpaged total. Defaults to `false` (no `related` field on any
    /// item).
    #[serde(default)]
    expand_related: Option<bool>,
    /// Requires `mine` — when `true`, adds an `unregistered_open` field to
    /// the result: open, unclaimed item counts by topic, for topics whose
    /// `owned_by` (see `list_topics`) is **empty** — no worker at all is
    /// registered for them. Surfaces the exact blind spot `mine` alone
    /// cannot: a topic everyone forgot to register for still returns 0
    /// rows here, silently indistinguishable from "nothing to do". Not "a
    /// topic some other worker owns" — that's normal and not this caller's
    /// business, and would swamp the real signal on a server with many
    /// topics. Costs one extra request against `docket-core` — opt-in so a
    /// plain `mine` query pays nothing for it. No effect without `mine`
    /// (the flag is only meaningful alongside a query about your own
    /// registration).
    #[serde(default)]
    report_gaps: Option<bool>,
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
    /// Matches the item's `requester` — symmetric to `assignee` above, and
    /// case-insensitive like it (ADR-0021).
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
    /// Same semantics as `list_items`'s field of the same name — resolves
    /// each returned item's `related:<id>` tags into a `related` field,
    /// bounded by the returned page.
    #[serde(default)]
    expand_related: Option<bool>,
    /// Same semantics as `list_items`'s field of the same name — requires
    /// `mine`, adds an `unregistered_open` registration-gap hint.
    #[serde(default)]
    report_gaps: Option<bool>,
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
struct SubmitParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The submitting worker. Omit to use this session's `DOCKET_WORKER_ID`
    /// (see `resolve_identity`).
    #[serde(default)]
    worker_id: Option<String>,
    /// What is being handed back, when it isn't simply "done" — most usefully
    /// the question the requester has to answer. Recorded as a comment with
    /// the transition itself, so the thread says why the turn moved rather
    /// than leaving that to a separate `add_comment` the reader has to
    /// correlate. Optional; omit it for an ordinary completed submission.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetRequesterParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The corrected requester identity. Must not be blank.
    requester: String,
    /// Who is making the correction — recorded on the lifecycle comment the
    /// change writes. Omit to use this session's `DOCKET_WORKER_ID` (see
    /// `resolve_identity`), same treatment as every other authored operation.
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetAssigneeParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The corrected assignee identity. Must not be blank — this tool only
    /// reassigns, it never clears assignee (see `reopen_item` for that).
    assignee: String,
    /// Who is making the correction — recorded on the lifecycle comment the
    /// change writes. Omit to use this session's `DOCKET_WORKER_ID` (see
    /// `resolve_identity`), same treatment as every other authored operation.
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetTopicParams {
    /// The item's canonical id, or its short numeric alias (`seq`) — e.g.
    /// `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The corrected topic. Must not be blank. For a spelling people
    /// actually use across many items, declare an alias instead
    /// (`docs/usage.md`) — this is the one-off, single-item fix.
    topic: String,
    /// Who is making the correction — recorded on the lifecycle comment the
    /// change writes. Omit to use this session's `DOCKET_WORKER_ID` (see
    /// `resolve_identity`), same treatment as every other authored operation.
    #[serde(default)]
    author: Option<String>,
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
    /// `false` (no `related` field at all).
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    related: Option<Vec<RelatedItemRefDto>>,
}

/// Mirrors `docket-core`'s `RelatedItemRef` JSON shape.
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
    /// Which declared spellings folded into this row (ADR-0022). Defaulted
    /// so an older `docket-core` that doesn't send this field still
    /// deserializes, rather than failing the whole `list_topics` call.
    #[serde(default)]
    aliases: Vec<String>,
    /// Registered worker ids covering this topic — empty means orphan.
    /// Same defaulting reason as `aliases`.
    #[serde(default)]
    owned_by: Vec<String>,
    /// Open, unclaimed items in this topic — distinct from `count`, which
    /// is every non-archived item regardless of state. Same defaulting
    /// reason as `aliases`.
    #[serde(default)]
    open_unclaimed: i64,
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
fn content_type(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none")
        .to_string()
}

/// Message for a 2xx whose body isn't the JSON this tool expected.
///
/// The bare serde text ("expected value at line 1 column 1") leaves the caller
/// unable to tell a real not-found from a broken transport — the complaint
/// where a mis-shaped path quietly returned docket-core's console HTML with a 200. The
/// status and content-type are what separate the two, so they travel with the
/// parse error.
fn unparseable_body(
    status: reqwest::StatusCode,
    content_type: &str,
    error: &serde_json::Error,
) -> String {
    format!(
        "docket-core returned {status} with a body this tool could not parse          (content-type: {content_type}) — check DOCKET_CORE_URL and the request path: {error}"
    )
}

async fn respond<T: Serialize + for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<CallToolResult, McpError> {
    let status = resp.status();
    let content_type = content_type(&resp);
    let bytes = resp.bytes().await.map_err(unreachable_error)?;
    if status.is_success() {
        let value: T = serde_json::from_slice(&bytes).map_err(|e| {
            McpError::internal_error(unparseable_body(status, &content_type, &e), None)
        })?;
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
    let content_type = content_type(&resp);
    let bytes = resp.bytes().await.map_err(unreachable_error)?;
    if status.is_success() {
        let items: Vec<ItemDto> = serde_json::from_slice(&bytes).map_err(|e| {
            McpError::internal_error(unparseable_body(status, &content_type, &e), None)
        })?;
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

/// Inserts a `topic_advisory` key into a successful tool result's JSON
/// object — never appended as trailing prose, which would break every
/// caller (including this crate's own `field()`/`json_value()` test
/// helpers) that parses a tool result as one JSON document. Falls back to
/// the unmodified `result` if the content block isn't the JSON object this
/// is meant to annotate, rather than panicking or losing the created item.
fn with_topic_advisory(result: CallToolResult, note: String) -> CallToolResult {
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text.text) else {
        return result;
    };
    let Some(obj) = value.as_object_mut() else {
        return result;
    };
    obj.insert(
        "topic_advisory".to_string(),
        serde_json::Value::String(note),
    );
    match ContentBlock::json(&value) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
}

/// Inserts an `unregistered_open` key into a successful paginated result —
/// `with_topic_advisory`'s sibling for `fetch_gap_hint`. Same
/// fallback-to-unmodified behavior for a result shape this doesn't
/// recognize.
fn with_gap_hint(result: CallToolResult, hint: serde_json::Value) -> CallToolResult {
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text.text) else {
        return result;
    };
    let Some(obj) = value.as_object_mut() else {
        return result;
    };
    obj.insert("unregistered_open".to_string(), hint);
    match ContentBlock::json(&value) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
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
            .post(api_url(&self.base_url, &["workers"]))
            .json(&serde_json::json!({ "id": id, "topics": p.topics }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<WorkerDto>(resp).await
    }

    #[tool(
        description = "What's new since I last looked -- an activity feed independent of turn/state. Unlike mine (a snapshot of what you hold right now), this surfaces comments and transitions on items you're a stakeholder on (assignee/requester, folding declared aliases) or that fall under a topic you're registered for, even while turn stays with the other party -- e.g. an assignee answering your question in a comment without submitting (see ADR-0010's 2026-09-09 update). Always advance since to the returned cursor on your next call, even when events came back empty -- that still means \"caught up to here\", not \"nothing happened yet\". for_worker may be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn list_events(
        &self,
        Parameters(p): Parameters<ListEventsParams>,
    ) -> Result<CallToolResult, McpError> {
        let for_worker = match resolve_identity(p.for_worker, docket_worker_id(), "for_worker") {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let since = p.since.unwrap_or(0).to_string();
        let limit = p.limit.map(|l| l.to_string());
        let resp = self
            .http
            .get(api_url(&self.base_url, &["events"]))
            .query(&[
                ("for", Some(for_worker.as_str())),
                ("since", Some(since.as_str())),
                ("limit", limit.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<serde_json::Value>(resp).await
    }

    #[tool(
        description = "Fetch a worker's registration — its topics and online flag. online is set once at register_worker and never updated afterward — it is NOT a liveness signal, it means \"has registered\", not \"is currently active\". id may be omitted to look up this session's own registration via DOCKET_WORKER_ID. The only way to positively confirm what you're currently registered as (topic_scope/mine treat an unknown worker id the same as one with no matching topics: an empty result, not an error)"
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
            .get(api_url(&self.base_url, &["workers", &id]))
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
            .post(api_url(&self.base_url, &["items"]))
            .json(&p)
            .send()
            .await
            .map_err(unreachable_error)?;
        let result = respond::<ItemDto>(resp).await?;
        if result.is_error == Some(true) {
            return Ok(result);
        }
        // Advisory, never fatal: a failed advisory lookup must not turn a
        // successful create into a tool error. The item exists either way,
        // so the worst case is the caller simply isn't told.
        let Ok(Some(note)) = self.fetch_topic_advisory(&p.topic).await else {
            return Ok(result);
        };
        Ok(with_topic_advisory(result, note))
    }

    /// `GET /topics/candidates` — one sentence when the topic looks
    /// mistargeted, `None` when it does not. Returns `Err` only for a
    /// transport failure the caller is expected to ignore.
    ///
    /// An unserved topic is reported **with or without** a candidate
    /// suggestion — dropping the warning entirely whenever `candidates`
    /// came back empty would silently miss exactly the case a mismatch
    /// with a word inserted in the middle produces (e.g. `org/widgets` vs.
    /// the registered `org/vendor-widgets`: last path segments differ, so
    /// no candidate is found, but the topic is still unserved and the
    /// filer still deserves to know).
    async fn fetch_topic_advisory(&self, topic: &str) -> anyhow::Result<Option<String>> {
        #[derive(serde::Deserialize)]
        struct Advisory {
            unserved: bool,
            candidates: Vec<String>,
        }
        let resp: Advisory = self
            .http
            .get(api_url(&self.base_url, &["topics", "candidates"]))
            .query(&[("topic", topic)])
            .send()
            .await?
            .json()
            .await?;
        if !resp.unserved {
            return Ok(None);
        }
        if resp.candidates.is_empty() {
            return Ok(Some(format!(
                "No registered worker serves topic `{topic}`. If this is a genuinely new \
                 topic, an admin can register a worker for it; if it was meant to reach an \
                 existing topic under a different spelling, an admin can declare an alias \
                 (POST /aliases) or correct this item's topic (PATCH /items/{{id}})."
            )));
        }
        Ok(Some(format!(
            "No registered worker serves topic `{topic}`. Served topics sharing its last \
             path segment: {}. If one of those was the intended target, an admin can declare \
             an alias (POST /aliases) or correct this item's topic (PATCH /items/{{id}}).",
            resp.candidates
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
    }

    /// `GET /topics` — registration-gap hint accompanying `mine=<worker>`.
    /// Topics with open, unclaimed items whose `owned_by` is **empty** —
    /// no worker at all is registered for them, the same blind spot that
    /// lets `mine` silently return 0 while items sit under a topic
    /// everyone forgot to register for. `None` when there is nothing to
    /// report, or the call fails (advisory only, like
    /// `fetch_topic_advisory`).
    ///
    /// Deliberately **not** "topics `owned_by` doesn't name this caller" —
    /// that would also match every topic a *different* worker legitimately
    /// owns, which is normal and not this caller's business; on a server
    /// with many topics that would bury the real signal (a topic nobody at
    /// all is registered for) under everyone else's ordinary backlog. The
    /// result is the same for every caller, since it names no worker —
    /// `mine` still gates whether the flag applies, because it's only
    /// meaningful alongside a query about *your own* registration.
    async fn fetch_gap_hint(&self) -> anyhow::Result<Option<serde_json::Value>> {
        #[derive(serde::Deserialize)]
        struct TopicRow {
            topic: String,
            #[serde(default)]
            owned_by: Vec<String>,
            #[serde(default)]
            open_unclaimed: i64,
        }
        let resp = self
            .http
            .get(api_url(&self.base_url, &["topics"]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let topics: Vec<TopicRow> = resp.json().await?;

        let mut gaps = serde_json::Map::new();
        for t in topics {
            if t.open_unclaimed == 0 || !t.owned_by.is_empty() {
                continue;
            }
            gaps.insert(t.topic, serde_json::json!(t.open_unclaimed));
        }
        Ok(if gaps.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(gaps))
        })
    }

    #[tool(
        description = "List items, optionally filtered by topic, state, the worker currently assigned (assignee), the requester, a worker's topic jurisdiction (topic_scope), what a worker should currently be paying attention to (mine — assignee OR resolved-and-waiting-on-my-decision OR open-and-unclaimed within a topic this worker is registered for), and/or archived status. `mine` alone covers the full \"what do I need to look at\" set — prefer it over combining assignee/requester/topic_scope yourself, since an unclaimed item in your own topic is otherwise easy to miss. Paginated via limit/offset — check the result's total field. Pass summary=true to omit each item's body when you only need enough to pick which one to fetch in full next. Ordered by updated_at descending (most-recently-touched first) by default — pass order=\"asc\" to find the longest-untouched items directly instead of paging to the tail via offset. Pass expand_related=true to also resolve each returned item's related:<id> tags (both directions) into a related field, applied only to the returned page — same expansion get_item offers for a single item. Pass report_gaps=true (with mine) to add an unregistered_open hint: open/unclaimed item counts by topic for topics no worker at all is registered for (not topics someone else owns) — the signal that a mine=... 0-result may be an orphaned topic, not really nothing to do"
    )]
    async fn list_items(
        &self,
        Parameters(p): Parameters<ListItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        let archived = p.archived.map(|a| a.to_string());
        let limit = p.limit.map(|l| l.to_string());
        let offset = p.offset.map(|o| o.to_string());
        let summary = p.summary.map(|s| s.to_string());
        let expand_related = p.expand_related.map(|b| b.to_string());
        let resp = self
            .http
            .get(api_url(&self.base_url, &["items"]))
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
                ("expand_related", expand_related.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        let result = respond_paginated(resp).await?;
        if result.is_error == Some(true) {
            return Ok(result);
        }
        if p.report_gaps != Some(true) || p.mine.is_none() {
            return Ok(result);
        }
        // Advisory, never fatal — same treatment create_item gives
        // fetch_topic_advisory's failure mode.
        let Ok(Some(hint)) = self.fetch_gap_hint().await else {
            return Ok(result);
        };
        Ok(with_gap_hint(result, hint))
    }

    #[tool(
        description = "Search items by full-text query and/or tags — call this before create_item to check whether a matching issue already exists. Combinable with the same ownership filters list_items offers (assignee/requester/topic_scope/mine). Pass summary=true to omit each item's body when you only need enough to pick which one to fetch in full next. Same order semantics as list_items (default updated_at descending, order=\"asc\" to reverse). Same expand_related semantics as list_items too — resolves each returned item's related:<id> tags into a related field, bounded by the returned page. Same report_gaps semantics as list_items too — with mine, adds an unregistered_open registration-gap hint"
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
        let expand_related = p.expand_related.map(|b| b.to_string());
        if let Some(e) = expand_related.as_deref() {
            query_pairs.push(("expand_related", e));
        }
        let resp = self
            .http
            .get(api_url(&self.base_url, &["items"]))
            .query(&query_pairs)
            .send()
            .await
            .map_err(unreachable_error)?;
        let result = respond_paginated(resp).await?;
        if result.is_error == Some(true) {
            return Ok(result);
        }
        if p.report_gaps != Some(true) || p.mine.is_none() {
            return Ok(result);
        }
        let Ok(Some(hint)) = self.fetch_gap_hint().await else {
            return Ok(result);
        };
        Ok(with_gap_hint(result, hint))
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
        description = "Hand a claimed item back to the requester — claimed -> resolved. This is \
            the ONLY transition that moves turn to the requester, and it means \"the assignee \
            can't take this further, the requester decides what happens next\" — which covers \
            finished work AND work waiting on an answer only the requester has. Submit in both \
            cases: an item you leave in claimed while you wait for an answer is invisible to \
            every query the requester runs (mine surfaces items assigned to them, resolved items \
            they filed, and unclaimed items in their topics — never one you are holding), so the \
            question sits unread in the thread. Put the question in reason; the requester answers \
            with reject_item, which hands the turn back to you with their answer attached. \
            worker_id may be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn submit_item(
        &self,
        Parameters(p): Parameters<SubmitParams>,
    ) -> Result<CallToolResult, McpError> {
        let worker_id = match resolve_identity(p.worker_id, docket_worker_id(), "worker_id") {
            Ok(id) => id,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .post(items_url(&self.base_url, &p.item_id, &["submit"]))
            .json(&serde_json::json!({ "worker_id": worker_id, "reason": p.reason }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Approve a resolved item as the requester, closing it with resolution=done. \
            If the item has a requester set, author must match it or the call fails — use \
            set_item_requester to correct a drifted identity — correct it, never retry under \
            the wrong spelling. author may be omitted if this session's DOCKET_WORKER_ID is set"
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
        description = "Hand a resolved item back to the assignee — resolved -> claimed. \
            Rework is one use; ANSWERING a question the assignee submitted is an equally normal \
            one, since reason is what carries the answer. Requires a reason, recorded as a \
            comment atomically with the state change. If the \
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
        description = "Correct an item's requester — whether it has one already or not. Covers \
            both an item filed before a requester identity was available (or left blank by a \
            migration) and an identity that drifted afterwards: a typo, a renamed repo, or two \
            consumers spelling the same identity differently. That second case is what \
            approve_item/reject_item point here for when their requester match fails — correct \
            the item, do not impersonate the wrong spelling. State-independent (works on a \
            closed item too — this corrects metadata, it isn't a workflow transition) and \
            idempotent (setting the value it already has changes nothing). A real change is \
            recorded as a comment naming the old and new value. Does not cover turn/title/body — \
            use set_item_assignee to correct assignee, set_item_topic to correct topic. author may \
            be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn set_item_requester(
        &self,
        Parameters(p): Parameters<SetRequesterParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .patch(items_url(&self.base_url, &p.item_id, &[]))
            .json(&serde_json::json!({ "requester": p.requester, "author": author }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Correct an item's assignee — the mirror of set_item_requester on the \
            other side of the handshake. Covers both an item nobody has claimed yet and one \
            whose assignee identity drifted or vanished (a claimed workspace renamed or torn \
            down, with nothing else able to move it off). Reassignment only: this never clears \
            assignee — unassigning is reopen_item's job, not this one's. State-independent \
            (works on a closed item too — this corrects metadata, it isn't a workflow \
            transition) and idempotent (setting the value it already has changes nothing). A \
            real change is recorded as a comment naming the old and new value. author may be \
            omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn set_item_assignee(
        &self,
        Parameters(p): Parameters<SetAssigneeParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .patch(items_url(&self.base_url, &p.item_id, &[]))
            .json(&serde_json::json!({ "assignee": p.assignee, "author": author }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Correct an item's topic — the third correction in the same family as \
            set_item_requester/set_item_assignee. For an item filed under the wrong topic: a \
            typo, or a mismatch caught by create_item's topic_advisory / GET /topics/candidates \
            after the fact. For a spelling people actually use across many items, declare an \
            alias instead (put_alias) — this is the one-off, single-item fix, not a standing \
            declaration. State-independent (works on a closed item too — this corrects metadata, \
            it isn't a workflow transition) and idempotent (setting the value it already has \
            changes nothing). A real change is recorded as a comment naming the old and new \
            value. author may be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn set_item_topic(
        &self,
        Parameters(p): Parameters<SetTopicParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let resp = self
            .http
            .patch(items_url(&self.base_url, &p.item_id, &[]))
            .json(&serde_json::json!({ "topic": p.topic, "author": author }))
            .send()
            .await
            .map_err(unreachable_error)?;
        respond::<ItemDto>(resp).await
    }

    #[tool(
        description = "Add tags to an item (idempotent — adding an already-present tag is a no-op). If you use the related:<id> convention (see get_item/list_items' expand_related), tag with the target's canonical id, not its seq alias (e.g. #142) — reverse lookup (\"referenced_by\") only matches the literal canonical-id string, so a seq-alias-tagged reference is found in the forward direction but never shows up on the other item's referenced_by list"
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
            .get(api_url(&self.base_url, &["tags"]))
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
            .get(api_url(&self.base_url, &["topics"]))
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
                .get(api_url(&process.base_url, &["items"]))
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

    /// The ADR-0010 09-09 scenario this feature exists for: an assignee
    /// answers a requester's question in a comment without submitting --
    /// turn correctly stays with the assignee (real, unfinished work), and
    /// list_events is what lets the requester see the answer anyway.
    #[tokio::test]
    async fn list_events_surfaces_a_comment_without_turn_moving() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("events.db");
        let core = spawn_core(18442, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        // list_events requires the caller to be a registered worker (its
        // topic-jurisdiction leg has no meaning otherwise) -- unlike mine/
        // topic_scope on list_items, which tolerate an unknown id as an
        // empty result. See Store::list_events' doc comment.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("acme/req".to_string()),
                topics: vec![],
            }))
            .await
            .unwrap();

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "question".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/req".to_string()),
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("acme/assignee".to_string()),
            }))
            .await
            .unwrap();

        server
            .add_comment(Parameters(AddCommentParams {
                item_id: item_id.clone(),
                body: "answered your question".to_string(),
                author: Some("acme/assignee".to_string()),
            }))
            .await
            .unwrap();

        let first = server
            .list_events(Parameters(ListEventsParams {
                for_worker: Some("acme/req".to_string()),
                since: Some(0),
                limit: None,
            }))
            .await
            .unwrap();
        assert_ne!(first.is_error, Some(true));
        let body = json_value(&first);
        let events = body["events"].as_array().unwrap();
        assert!(
            events
                .iter()
                .any(|e| e["item_id"] == item_id && e["kind"] == "comment"),
            "requester must see the comment event even though turn stayed with assignee"
        );
        let cursor = body["cursor"].as_i64().unwrap();
        assert!(cursor > 0);

        let second = server
            .list_events(Parameters(ListEventsParams {
                for_worker: Some("acme/req".to_string()),
                since: Some(cursor),
                limit: None,
            }))
            .await
            .unwrap();
        let body2 = json_value(&second);
        assert!(body2["events"].as_array().unwrap().is_empty());
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
                expand_related: None,
                report_gaps: None,
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
            .submit_item(Parameters(SubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("w1".to_string()),
                reason: None,
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
            .submit_item(Parameters(SubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("w1".to_string()),
                reason: None,
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
            .submit_item(Parameters(SubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("w1".to_string()),
                reason: None,
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
                expand_related: None,
                report_gaps: None,
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
                expand_related: None,
                report_gaps: None,
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
                expand_related: None,
                report_gaps: None,
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

    /// A worker that files against a plausible-but-wrong org gets told so in
    /// the tool result it already reads, rather than having to know to ask.
    #[tokio::test]
    async fn create_item_tool_appends_a_mistargeting_advisory() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("mistarget.db");
        let core = spawn_core(18441, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        // A served topic with items, so there is something to be suggested.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("iyulab/docket".to_string()),
                topics: vec!["iyulab/docket".to_string()],
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "served".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let mistargeted = server
            .create_item(Parameters(CreateItemParams {
                topic: "other-org/docket".to_string(),
                title: "mistargeted".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let text = text_of(&mistargeted);
        assert!(
            text.contains("mistargeted") || text.contains("\"title\""),
            "the item is still created"
        );
        assert!(
            text.contains("iyulab/docket"),
            "the served topic sharing the last segment is named: {text}"
        );
    }

    /// A mismatch that inserts an extra word in the middle of an otherwise
    /// similar name (e.g. `org/data-widgets` vs. a registered
    /// `org/vendor-data-widgets`) shares no last path segment with anything
    /// served, so `topic_candidates` finds nothing to suggest. The advisory
    /// must still fire in that case — dropping it entirely whenever there
    /// is no candidate would leave exactly this shape of mismatch
    /// unflagged.
    #[tokio::test]
    async fn create_item_tool_warns_of_an_orphan_topic_even_without_a_candidate_suggestion() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("orphan-no-candidate.db");
        let core = spawn_core(18444, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        // A registered worker exists, but for an unrelated topic that shares
        // no last path segment with the one below — so `candidates` comes
        // back empty even though the topic is unserved.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("iyulab/other".to_string()),
                topics: vec!["iyulab/other".to_string()],
            }))
            .await
            .unwrap();

        let orphaned = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/lonely".to_string(),
                title: "orphaned".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let text = text_of(&orphaned);
        assert!(
            text.contains("No registered worker serves topic `acme/lonely`"),
            "orphan topic must still be flagged with no candidate available: {text}"
        );
        assert!(
            !text.contains("Served topics sharing"),
            "must not claim a candidate exists when none was found: {text}"
        );
    }

    /// `list_topics`' `owned_by` is the bulk-visibility counterpart to the
    /// single-topic advisory above — an admin scanning the whole vocabulary
    /// sees every orphan at once instead of tripping over them one
    /// `create_item` at a time.
    #[tokio::test]
    async fn list_topics_tool_reports_owned_by_registered_workers() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("owned-by.db");
        let core = spawn_core(18445, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("iyulab/bot".to_string()),
                topics: vec!["iyulab/docket".to_string()],
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "covered".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/lonely".to_string(),
                title: "orphaned".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let topics = server.list_topics().await.unwrap();
        let value = json_value(&topics);
        let list = value.as_array().unwrap();
        let covered = list.iter().find(|t| t["topic"] == "iyulab/docket").unwrap();
        assert_eq!(covered["owned_by"], serde_json::json!(["iyulab/bot"]));
        let orphan = list.iter().find(|t| t["topic"] == "acme/lonely").unwrap();
        assert_eq!(
            orphan["owned_by"],
            serde_json::json!([]),
            "no worker is registered for this topic"
        );
    }

    /// `mine=w1` alone can't tell "nothing to do" apart from "a topic
    /// nobody registered for has open work". With `report_gaps=true`, the
    /// tool result must name that orphan topic and its open-unclaimed
    /// count instead of staying silent about it — but a topic a *different*
    /// worker legitimately owns is not w1's business and must not appear,
    /// even though it's equally outside w1's own registration.
    #[tokio::test]
    async fn list_items_report_gaps_surfaces_orphan_topics_but_not_another_workers() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("report-gaps.db");
        let core = spawn_core(18446, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("w1".to_string()),
                topics: vec!["iyulab/one".to_string()],
            }))
            .await
            .unwrap();
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("w2".to_string()),
                topics: vec!["iyulab/three".to_string()],
            }))
            .await
            .unwrap();
        // Nobody is registered for this topic — the gap `mine=w1` alone
        // cannot see.
        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/two".to_string(),
                title: "unregistered gap".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        // w2's own topic, also outside w1's registration — but this is w2's
        // ordinary backlog, not a gap, and must not show up in w1's hint.
        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/three".to_string(),
                title: "w2's own open work".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let mine_alone = server
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
                expand_related: None,
                report_gaps: None,
            }))
            .await
            .unwrap();
        let mine_value = json_value(&mine_alone);
        assert_eq!(
            mine_value["items"].as_array().unwrap().len(),
            0,
            "mine=w1 alone sees nothing — exactly the silent gap this issue is about"
        );
        assert!(
            mine_value.get("unregistered_open").is_none(),
            "no hint without report_gaps=true"
        );

        let with_hint = server
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
                expand_related: None,
                report_gaps: Some(true),
            }))
            .await
            .unwrap();
        let hint_value = json_value(&with_hint);
        assert_eq!(
            hint_value["unregistered_open"],
            serde_json::json!({"iyulab/two": 1}),
            "the gap topic and its open-unclaimed count must be named: {hint_value}"
        );
    }

    /// `report_gaps=true` without `mine` has nothing to compare a
    /// registration against, so it must be a no-op rather than guessing —
    /// same "requires `mine`" contract `search_items` documents.
    #[tokio::test]
    async fn list_items_report_gaps_without_mine_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("report-gaps-no-mine.db");
        let core = spawn_core(18447, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/two".to_string(),
                title: "unregistered".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();

        let result = server
            .list_items(Parameters(ListItemsParams {
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
                expand_related: None,
                report_gaps: Some(true),
            }))
            .await
            .unwrap();
        assert!(json_value(&result).get("unregistered_open").is_none());
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
                expand_related: None,
                report_gaps: None,
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
                expand_related: None,
                report_gaps: None,
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
                author: Some("acme/fixer".to_string()),
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
                expand_related: None,
                report_gaps: None,
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
                item_id: item_id.clone(),
                requester: "   ".to_string(),
                author: Some("acme/fixer".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(rejected.is_error, Some(true));

        // The other half of what this tool is for: repairing a requester that
        // is already set. The tool used to describe itself as covering only
        // the blank case, which is what sent a reader looking for a primitive
        // that already existed.
        let repaired = server
            .set_item_requester(Parameters(SetRequesterParams {
                item_id: item_id.clone(),
                requester: "Backfilled-Reporter".to_string(),
                author: Some("acme/fixer".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(repaired.is_error, Some(true));
        assert_eq!(json_value(&repaired)["requester"], "Backfilled-Reporter");

        let comments = server
            .list_comments(Parameters(ItemIdParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();
        let rendered = text_of(&comments);
        assert!(
            rendered.contains("requester: backfilled-reporter -> Backfilled-Reporter"),
            "correction must be recorded in the thread, got: {rendered}"
        );
    }

    /// The assignee side of the same handshake as
    /// `set_item_requester_backfills_a_blank_requester`: a workspace claimed
    /// under one spelling that later renamed or vanished, with nothing else
    /// able to move `assignee` off of it.
    #[tokio::test]
    async fn set_item_assignee_reassigns_a_claimed_item() {
        let dir = std::env::temp_dir().join(format!(
            "docket-mcp-test-set-assignee-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set-assignee.db");
        let core = spawn_core(18443, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "claimed under a spelling that later drifted".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("acme/widget".to_string()),
            }))
            .await
            .unwrap();

        let rejected = server
            .set_item_assignee(Parameters(SetAssigneeParams {
                item_id: item_id.clone(),
                assignee: "   ".to_string(),
                author: Some("acme/fixer".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(rejected.is_error, Some(true));

        let reassigned = server
            .set_item_assignee(Parameters(SetAssigneeParams {
                item_id: item_id.clone(),
                assignee: "acme/Widget".to_string(),
                author: Some("acme/fixer".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(reassigned.is_error, Some(true));
        assert_eq!(json_value(&reassigned)["assignee"], "acme/Widget");

        let refetched = server
            .list_items(Parameters(ListItemsParams {
                topic: Some("iyulab/docket".to_string()),
                state: None,
                assignee: Some("acme/Widget".to_string()),
                requester: None,
                topic_scope: None,
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
                expand_related: None,
                report_gaps: None,
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

        let comments = server
            .list_comments(Parameters(ItemIdParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();
        let rendered = text_of(&comments);
        assert!(
            rendered.contains("assignee: acme/widget -> acme/Widget"),
            "correction must be recorded in the thread, got: {rendered}"
        );
    }

    /// The third correction in the same family — an item filed under a
    /// topic that turned out to be a typo, without declaring a standing
    /// alias for what was really a one-off mistake.
    #[tokio::test]
    async fn set_item_topic_corrects_a_mistargeted_item() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-set-topic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set-topic.db");
        let core = spawn_core(18448, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/dcoket".to_string(),
                title: "filed under a typo'd topic".to_string(),
                body: None,
                tags: vec![],
                requester: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let rejected = server
            .set_item_topic(Parameters(SetTopicParams {
                item_id: item_id.clone(),
                topic: "   ".to_string(),
                author: Some("acme/fixer".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(rejected.is_error, Some(true));

        let corrected = server
            .set_item_topic(Parameters(SetTopicParams {
                item_id: item_id.clone(),
                topic: "iyulab/docket".to_string(),
                author: Some("acme/fixer".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(corrected.is_error, Some(true));
        assert_eq!(json_value(&corrected)["topic"], "iyulab/docket");

        let refetched = server
            .get_item(Parameters(GetItemParams {
                item_id: item_id.clone(),
                expand_related: None,
            }))
            .await
            .unwrap();
        assert_eq!(json_value(&refetched)["topic"], "iyulab/docket");

        let comments = server
            .list_comments(Parameters(ItemIdParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();
        let rendered = text_of(&comments);
        assert!(
            rendered.contains("topic: iyulab/dcoket -> iyulab/docket"),
            "correction must be recorded in the thread, got: {rendered}"
        );
    }

    /// Pins the encoding contract itself, independently of docket-core.
    ///
    /// The end-to-end tests here run against a core built from this same
    /// workspace, so they would also pass on core's fix alone — but this crate
    /// talks to whatever core a machine is pointed at, which can be an older
    /// deployment that still answers a mis-shaped path with `200 text/html`.
    /// Encoding the segment on the way out is what makes the request correct
    /// regardless.
    #[test]
    fn api_url_encodes_a_path_segment_rather_than_splitting_it() {
        assert_eq!(
            api_url("http://127.0.0.1:8420", &["workers", "acme/widget"]).as_str(),
            "http://127.0.0.1:8420/workers/acme%2Fwidget"
        );
        // An item's `seq` alias arrives as `#142`; unencoded, `#` would make
        // the rest a URL fragment and the path just `/items/`.
        assert_eq!(
            items_url("http://127.0.0.1:8420", "#142", &["claim"]).as_str(),
            "http://127.0.0.1:8420/items/%23142/claim"
        );
    }

    /// The question-and-answer round trip ADR-0010's 2026-09-08 update makes
    /// explicit — and specifically the visibility claim behind it.
    ///
    /// An assignee that needs a requester decision must submit rather than sit
    /// on `claimed`, because `claimed` is the one state no query of the
    /// requester's reaches: `mine` matches items assigned to *them*, `resolved`
    /// items they filed, and unclaimed items in their topics. This asserts the
    /// gap directly — the item is absent from the requester's `mine` while the
    /// assignee holds it, and present the moment it is handed back — so the
    /// reasoning stays checkable instead of living only in prose.
    #[tokio::test]
    async fn a_question_reaches_the_requester_only_after_submit() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-question-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("question.db");
        let core = spawn_core(18440, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/thing".to_string(),
                title: "t".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/filer".to_string()),
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("acme/worker".to_string()),
            }))
            .await
            .unwrap();

        let mine = |who: &str| {
            let server = &server;
            let who = who.to_string();
            async move {
                server
                    .list_items(Parameters(ListItemsParams {
                        topic: None,
                        state: None,
                        assignee: None,
                        requester: None,
                        topic_scope: None,
                        mine: Some(who),
                        archived: None,
                        limit: None,
                        offset: None,
                        summary: None,
                        order: None,
                        expand_related: None,
                        report_gaps: None,
                    }))
                    .await
                    .unwrap()
            }
        };

        // Held by the assignee: invisible to the requester. This is why
        // parking a question in `claimed` loses it.
        let held = mine("acme/filer").await;
        assert_eq!(json_value(&held)["total"], 0);

        let submitted = server
            .submit_item(Parameters(SubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("acme/worker".to_string()),
                reason: Some("which of the two schemas should this follow?".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(field(&submitted, "state"), "resolved");
        assert_eq!(json_value(&submitted)["turn"], "requester");

        // Handed back: now it is in front of the requester, question and all.
        let waiting = mine("acme/filer").await;
        assert_eq!(json_value(&waiting)["total"], 1);
        assert_eq!(json_value(&waiting)["items"][0]["id"], item_id);

        let comments = server
            .list_comments(Parameters(ItemIdParams {
                item_id: item_id.clone(),
            }))
            .await
            .unwrap();
        assert!(
            text_of(&comments).contains("which of the two schemas should this follow?"),
            "the question travels with the transition, got: {}",
            text_of(&comments)
        );

        // The answer comes back through `reject_item` — a turn handoff here,
        // not a verdict on the work.
        let answered = server
            .reject_item(Parameters(ReasonedParams {
                item_id: item_id.clone(),
                author: Some("acme/filer".to_string()),
                reason: "the first one".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(field(&answered, "state"), "claimed");
        assert_eq!(json_value(&answered)["turn"], "assignee");
        assert_eq!(json_value(&answered)["resolution"], serde_json::Value::Null);

        let back_to_worker = mine("acme/worker").await;
        assert_eq!(json_value(&back_to_worker)["total"], 1);
    }

    /// `get_worker` is the only positive-confirmation path for registration
    /// — round-trips a real registration and 404s a never-registered id,
    /// per docs/usage.md's read/write not-found asymmetry.
    ///
    /// The fixture ids are deliberately `org/repo` shaped, which is the only
    /// form real callers use and the one that broke: a worker id is the single
    /// value this crate puts in a *path segment*, so an unencoded `/` made the
    /// request miss its route and come back as docket-core's console HTML with
    /// a `200`, for registered and unregistered ids alike.
    /// A slash-free id can't reproduce that, which is how the earlier version
    /// of this very test passed against a tool that never worked. Registering
    /// then reading back is what proves the segment round-trips: `api_url`
    /// encodes the `/` and axum's `Path` extractor decodes it.
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
                id: Some("acme/widget".to_string()),
                topics: vec!["acme".to_string()],
            }))
            .await
            .unwrap();

        let found = server
            .get_worker(Parameters(GetWorkerParams {
                id: Some("acme/widget".to_string()),
            }))
            .await
            .unwrap();
        assert_ne!(found.is_error, Some(true));
        assert_eq!(json_value(&found)["id"], "acme/widget");
        assert_eq!(json_value(&found)["topics"][0], "acme");

        // A never-registered id must be a readable tool error, not a parse
        // failure the caller can't tell apart from a broken transport.
        let missing = server
            .get_worker(Parameters(GetWorkerParams {
                id: Some("acme/never-registered".to_string()),
            }))
            .await
            .unwrap();
        assert_eq!(missing.is_error, Some(true));
        assert_eq!(text_of(&missing), "not found");
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
                expand_related: None,
                report_gaps: None,
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
                expand_related: None,
                report_gaps: None,
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
                expand_related: None,
                report_gaps: None,
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
                expand_related: None,
                report_gaps: None,
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

    /// `expand_related` is forwarded end to end through the
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

    /// The same `expand_related` forwarding `get_item` gets (above), proven
    /// on `list_items` too — a batch-expand follow-on.
    #[tokio::test]
    async fn list_items_expand_related_is_forwarded_through_the_real_request() {
        let dir = std::env::temp_dir().join(format!(
            "docket-mcp-test-list-expand-related-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("list-expand-related.db");
        let core = spawn_core(18439, &db_path).await;
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
                expand_related: None,
                report_gaps: None,
            }))
            .await
            .unwrap();
        let without_expand_body = json_value(&without_expand);
        for item in without_expand_body["items"].as_array().unwrap() {
            assert!(item.get("related").is_none());
        }

        let with_expand = server
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
                expand_related: Some(true),
                report_gaps: None,
            }))
            .await
            .unwrap();
        let with_expand_body = json_value(&with_expand);
        let items = with_expand_body["items"].as_array().unwrap();
        let a_row = items.iter().find(|i| i["id"] == a_id).unwrap();
        let related = a_row["related"].as_array().unwrap();
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
