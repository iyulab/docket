//! Exposes docket-core's work-queue operations as MCP tools. This is a thin
//! translation layer: every tool is an HTTP call to a running `docket-core`
//! server. It never links `docket_core` as a library — the layer boundary is
//! drawn at the protocol (see docs/architecture.md "Four layers"), not at
//! the language, so this crate stays coupled only to core's HTTP/JSON
//! contract, not its internal Rust types.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
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
    /// A channel topic to tell: one line naming this item is also written there as a topic comment.
    /// Nothing is sent unless this is given.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The adapter's half of ADR-0028's item-to-channel link. The core never links an item to a
    // channel by itself; this writes one ordinary topic comment after the item exists. Not sent to
    // the core -- it is not an item field.
    #[serde(default, skip_serializing)]
    cc: Option<String>,
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
    /// Exact topic (case-insensitive).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Topic filter — matches one topic, case-insensitively like every
    // identifier comparison (ADR-0021).
    #[serde(default)]
    topic: Option<String>,
    /// open, claimed, resolved or closed. Not together with `mine`.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // One of open/claimed/resolved/closed.
    #[serde(default)]
    state: Option<String>,
    /// Items held by this worker id.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A worker id — narrows the list to items whose `assignee` (current
    // holder) is this worker. Compared case-insensitively, like every
    // identifier (ADR-0021).
    #[serde(default)]
    assignee: Option<String>,
    /// Items filed by this identity.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Matches the item's `requester` — symmetric to `assignee` above, and
    // case-insensitive like it (ADR-0021).
    #[serde(default)]
    requester: Option<String>,
    /// Items under the topics this worker id is registered for (prefix match).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A registered worker id — narrows the list to items under any topic
    // that worker is registered for (prefix match). Unlike `assignee`,
    // this doesn't check who actually holds any given item — it's a
    // topic-jurisdiction filter, not an ownership filter.
    #[serde(default)]
    topic_scope: Option<String>,
    /// Worker id: your inbox -- items you hold (not closed; add state=closed to include them), resolved items you filed that await your decision, unclaimed open items in your topics. Use it alone.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A worker id — the one-shot "what should I be looking at" filter:
    // matches items this worker is the `assignee` of (not yet closed -- pass `state=closed` to include
    // closed ones), OR items this worker
    // filed (`requester`) that are now `resolved` and waiting on *its*
    // decision — approve it, or answer what the assignee asked and hand it
    // back with `reject_item` (ADR-0010's 2026-09-08 update) — OR `open`
    // (unclaimed) items under a topic this worker is registered for (the
    // topic-jurisdiction test is the same one
    // `topic_scope` uses). ANDs with every other filter here,
    // same as `assignee`/`requester` individually. Prefer this over
    // manually combining `assignee`, `requester`+`state=resolved`, and
    // `topic_scope`+`state=open` yourself.
    #[serde(default)]
    mine: Option<String>,
    /// true = only archived items; default excludes them.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Excludes archived items by default; `true` returns only archived
    // items (explicit archive browse). See ADR-0013.
    #[serde(default)]
    archived: Option<bool>,
    /// Max rows (default 50, cap 200); `total` counts every match.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Max rows returned, applied after every other filter. Server default
    // 50, hard-capped at 200 — see ADR-0014. The tool result's `total`
    // field reports how many rows matched before this cap, so you know
    // whether to page with `offset`.
    #[serde(default)]
    limit: Option<usize>,
    /// Rows to skip.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Rows to skip before applying `limit`. Defaults to 0.
    #[serde(default)]
    offset: Option<usize>,
    /// true omits each row's body.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // When `true`, every returned item's `body` is omitted — set this once
    // you only need enough of each row to decide which item (if any) to
    // fetch in full next. See ADR-0014.
    #[serde(default)]
    summary: Option<bool>,
    /// "asc" = oldest-touched first (default: newest-touched first).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // "asc" or "desc", sorting by `updated_at`. Defaults to "desc"
    // (most-recently-touched first, today's fixed behavior); pass "asc"
    // to find the longest-untouched items directly instead of paging to
    // the tail via `offset`. See ADR-0020.
    #[serde(default)]
    order: Option<String>,
    /// true adds each row's `related:<id>` items.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // When `true`, each returned item also resolves its own `related:<id>`
    // tags (both directions) into a `related` field — the same expansion
    // `get_item` offers for a single item, applied per row here after
    // `limit`/`offset`, so the cost is bounded by the returned page, not
    // the unpaged total. Defaults to `false` (no `related` field on any
    // item).
    #[serde(default)]
    expand_related: Option<bool>,
    /// Epoch ms; only items touched after it (a comment, transition or tag change counts).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Epoch milliseconds. Only items touched after this instant come back --
    // a delta cursor: pass the newest `updated_at` you already hold and get
    // just what moved. Comments, transitions and tag changes count as touches.
    #[serde(default)]
    updated_after: Option<i64>,
    /// Item fields to return, e.g. ["title","state"]; `id` is always kept.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Item fields to return, e.g. `["title","state","turn"]`; `id` is always
    // included. Omit for every field. Trims what you must read per row.
    #[serde(default)]
    fields: Option<Vec<String>>,
    /// Character budget for the returned items (the first row is always kept). Rows held back are reported as truncated / omitted / next_offset.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Stop adding rows once the JSON of the returned items would exceed this
    // many characters (at least one row is always returned). When rows were
    // held back the result says `truncated`, `omitted` and the `next_offset`
    // to continue from.
    #[serde(default)]
    max_chars: Option<usize>,
    /// With `mine`: adds unregistered_open / prefix_only_open -- open work in topics nobody (or only a broad prefix) is registered for.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Requires `mine` — when `true`, adds an `unregistered_open` field to
    // the result: open, unclaimed item counts by topic, for topics whose
    // `owned_by` (see `list_topics`) is **empty** — no worker at all is
    // registered for them. Surfaces the exact blind spot `mine` alone
    // cannot: a topic everyone forgot to register for still returns 0
    // rows here, silently indistinguishable from "nothing to do". Not "a
    // topic some other worker owns" — that's normal and not this caller's
    // business, and would swamp the real signal on a server with many
    // topics. Also adds `prefix_only_open`: the same counts for topics that
    // are covered, but only through a broader prefix (`owned_by_exact`
    // empty) — an organization-wide registration otherwise makes them read
    // as owned. Costs one extra request against `docket-core` — opt-in so a
    // plain `mine` query pays nothing for it. No effect without `mine`
    // (the flag is only meaningful alongside a query about your own
    // registration).
    #[serde(default)]
    report_gaps: Option<bool>,
    /// With mine/requester/assignee/topic_scope: adds other_spellings -- other spellings of that identity and what sits under them.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Requires one of `mine`/`requester`/`assignee`/`topic_scope` — when
    // `true`, adds an
    // `other_spellings` field to the result: for each identity you filtered
    // on, any *other* identity on this server whose last `/` segment is the
    // same, with what sits under it — `requester`/`assignee`/`topic` are
    // per-role totals over every non-archived item *including closed
    // ones*, while `not_closed` counts the distinct items under that
    // spelling still in flight, which is the number that answers "am I
    // actually missing live work". Surfaces the blind spot a spelling
    // drift creates — `requester=acme/filer` returning fewer rows than the
    // party actually has, with nothing anywhere saying the rest are filed
    // under `filer`. Advisory: two parties can legitimately share a last
    // segment, so this asserts nothing and corrects nothing (fix a real
    // drift with `set_item_requester` for one item, or a declared alias for
    // the identity — see ADR-0022). Costs two extra requests against
    // `docket-core`, so it is opt-in. No effect without one of those three
    // filters — there would be no identity to compare against.
    #[serde(default)]
    report_drift: Option<bool>,
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
    /// Exact topic (case-insensitive).
    #[serde(default)]
    topic: Option<String>,
    /// open, claimed, resolved or closed. Not together with `mine`.
    #[serde(default)]
    state: Option<String>,
    /// Items held by this worker id.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A worker id — narrows results to items whose `assignee` (current
    // holder) is exactly this worker. Same semantics as `list_items`'s
    // field of the same name.
    #[serde(default)]
    assignee: Option<String>,
    /// Items filed by this identity.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Matches the item's `requester` — symmetric to `assignee` above, and
    // case-insensitive like it (ADR-0021).
    #[serde(default)]
    requester: Option<String>,
    /// Items under the topics this worker id is registered for (prefix match).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A registered worker id — narrows results to items under any topic
    // that worker is registered for (prefix match). Same semantics as
    // `list_items`'s field of the same name.
    #[serde(default)]
    topic_scope: Option<String>,
    /// Worker id: your inbox -- items you hold (not closed; add state=closed to include them), resolved items you filed that await your decision, unclaimed open items in your topics. Use it alone.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A worker id — the one-shot "what do I currently hold" filter. Same
    // semantics as `list_items`'s field of the same name.
    #[serde(default)]
    mine: Option<String>,
    /// true = only archived items; default excludes them.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Excludes archived items by default; `true` returns only archived
    // items (explicit archive browse). See ADR-0013.
    #[serde(default)]
    archived: Option<bool>,
    /// Max rows (default 50, cap 200); `total` counts every match.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Max rows returned, applied after every other filter. Server default
    // 50, hard-capped at 200 — see ADR-0014. The tool result's `total`
    // field reports how many rows matched before this cap, so you know
    // whether to page with `offset`.
    #[serde(default)]
    limit: Option<usize>,
    /// Rows to skip.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Rows to skip before applying `limit`. Defaults to 0.
    #[serde(default)]
    offset: Option<usize>,
    /// true omits each row's body.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // When `true`, every returned item's `body` is omitted — set this once
    // you only need enough of each row to decide which item (if any) to
    // fetch in full next. See ADR-0014.
    #[serde(default)]
    summary: Option<bool>,
    /// "asc" = oldest-touched first (default: newest-touched first).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // "asc" or "desc", sorting by `updated_at`. Same semantics as
    // `list_items`'s field of the same name. See ADR-0020.
    #[serde(default)]
    order: Option<String>,
    /// true adds each row's `related:<id>` items.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Same semantics as `list_items`'s field of the same name — resolves
    // each returned item's `related:<id>` tags into a `related` field,
    // bounded by the returned page.
    #[serde(default)]
    expand_related: Option<bool>,
    /// Epoch ms; only items touched after it (a comment, transition or tag change counts).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Epoch milliseconds. Only items touched after this instant come back --
    // a delta cursor: pass the newest `updated_at` you already hold and get
    // just what moved. Comments, transitions and tag changes count as touches.
    #[serde(default)]
    updated_after: Option<i64>,
    /// Item fields to return, e.g. ["title","state"]; `id` is always kept.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Item fields to return, e.g. `["title","state","turn"]`; `id` is always
    // included. Omit for every field. Trims what you must read per row.
    #[serde(default)]
    fields: Option<Vec<String>>,
    /// Character budget for the returned items (the first row is always kept). Rows held back are reported as truncated / omitted / next_offset.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Stop adding rows once the JSON of the returned items would exceed this
    // many characters (at least one row is always returned). When rows were
    // held back the result says `truncated`, `omitted` and the `next_offset`
    // to continue from.
    #[serde(default)]
    max_chars: Option<usize>,
    /// With `mine`: adds unregistered_open / prefix_only_open -- open work in topics nobody (or only a broad prefix) is registered for.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Same semantics as `list_items`'s field of the same name — requires
    // `mine`, adds the `unregistered_open` and `prefix_only_open`
    // registration-gap hints.
    #[serde(default)]
    report_gaps: Option<bool>,
    /// With mine/requester/assignee/topic_scope: adds other_spellings -- other spellings of that identity and what sits under them.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Same semantics as `list_items`'s field of the same name — requires
    // one of `mine`/`requester`/`assignee`/`topic_scope`, adds an
    // `other_spellings` identity-drift hint.
    #[serde(default)]
    report_drift: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClaimOrSubmitParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Omit to use this session's `DOCKET_WORKER_ID` (see `resolve_identity`)
    // — required, explicitly or via that fallback, so a claim can always be
    // traced to a worker.
    #[serde(default)]
    worker_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApproveParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Omit to use this session's `DOCKET_WORKER_ID` (see `resolve_identity`)
    // — required, explicitly or via that fallback (HD-16/HD-17, cycle-57).
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReasonedParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Omit to use this session's `DOCKET_WORKER_ID` (see `resolve_identity`)
    // — required, explicitly or via that fallback (HD-16/HD-17, cycle-57).
    #[serde(default)]
    author: Option<String>,
    reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TagsParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubmitParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The submitting worker. Omit to use this session's `DOCKET_WORKER_ID`
    // (see `resolve_identity`).
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
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The corrected requester identity. Must not be blank.
    requester: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Who is making the correction — recorded on the lifecycle comment the
    // change writes. Omit to use this session's `DOCKET_WORKER_ID` (see
    // `resolve_identity`), same treatment as every other authored operation.
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetAssigneeParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The corrected assignee identity. Must not be blank — this tool only
    /// reassigns, it never clears assignee (see `reopen_item` for that).
    assignee: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Who is making the correction — recorded on the lifecycle comment the
    // change writes. Omit to use this session's `DOCKET_WORKER_ID` (see
    // `resolve_identity`), same treatment as every other authored operation.
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetTopicParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
    /// The corrected topic. Must not be blank. For a spelling people
    /// actually use across many items, declare an alias instead
    /// (`docs/usage.md`) — this is the one-off, single-item fix.
    topic: String,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Who is making the correction — recorded on the lifecycle comment the
    // change writes. Omit to use this session's `DOCKET_WORKER_ID` (see
    // `resolve_identity`), same treatment as every other authored operation.
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
    /// Item id, or its numeric `seq` (`142`, `#142`). Give this or `topic`.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    #[serde(default)]
    item_id: Option<String>,
    /// Comment on a topic instead of an item (a channel workers follow). Give this or `item_id`.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // A topic comment belongs to the topic, not to an item: it has no requester,
    // assignee or turn and never appears in `mine` (ADR-0028). Workers registered for
    // the topic read it through `list_events`.
    #[serde(default)]
    topic: Option<String>,
    /// Your worker id; omit to use this session's DOCKET_WORKER_ID.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // Who's writing this comment. Omit to use this session's
    // `DOCKET_WORKER_ID` (see `resolve_identity`) — required, explicitly or
    // via that fallback, so the thread never lands docket-core's
    // `"unknown"` default (Issue #30). Schema-optional (unlike the original
    // cycle-55 shape) precisely so an omission can still resolve through
    // the env var instead of failing at deserialization.
    #[serde(default)]
    author: Option<String>,
    body: String,
}

/// Shared by every tool whose only input is an item id (`archive_item`). `get_item` has its own (`GetItemParams`, below) since it
/// alone also takes `expand_related`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ItemIdParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item. See `get_item`.
    item_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetItemParams {
    /// Item id, or its numeric `seq` (`142`, `#142`).
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`) — e.g.
    // `142` or `#142` — both resolve to the same item.
    item_id: String,
    /// When `true`, also resolves the item's `related:<id>` tags (both
    /// directions) into the response's `related` field. Defaults to
    /// `false` (no `related` field at all).
    #[serde(default)]
    expand_related: Option<bool>,
    /// Also return the newest N comments inline (`comments`, plus
    /// `comment_total`), so reading an item and its recent thread is one call.
    /// Omit for the item alone.
    #[serde(default)]
    comments: Option<usize>,
}

/// Input of `list_comments`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListCommentsParams {
    /// Item id, or its numeric `seq` (`142`, `#142`). Give this or `topic`.
    // Long form, kept for maintainers (the tool schema carries only the line above):
    // The item's canonical id, or its short numeric alias (`seq`).
    #[serde(default)]
    item_id: Option<String>,
    /// A topic whose comments to read instead of an item's. Give this or `item_id`.
    #[serde(default)]
    topic: Option<String>,
    /// Epoch milliseconds. Only comments written after this instant.
    #[serde(default)]
    since: Option<i64>,
    /// A comment `seq` (each comment carries one): only comments after it. The exact
    /// way to resume a thread or a topic.
    #[serde(default)]
    after: Option<i64>,
    /// Only the newest N comments.
    #[serde(default)]
    tail: Option<usize>,
    /// Newest-first character budget over the returned comments (the newest one
    /// is always returned); `older` says how many earlier ones were left out.
    #[serde(default)]
    max_chars: Option<usize>,
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
    /// When the item entered its current `state` (ADR-0024). Absent from
    /// servers older than that ADR, and `null` from a new enough server
    /// whose event log doesn't cover the item's last transition — the two
    /// are indistinguishable from this DTO alone, the same older-server
    /// defaulting `turn`/`open`/`archived_at` already accept.
    #[serde(default)]
    state_since: Option<i64>,
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

/// Input of `list_topics` / `list_identities`: both are whole-table listings
/// that grow with the server, so both can be narrowed.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTableParams {
    /// Case-insensitive substring; keeps only rows whose name (or a declared
    /// alias of it) contains it, e.g. `gateway`.
    #[serde(default)]
    q: Option<String>,
    /// Max rows returned (default 50). `total` counts every match and
    /// `omitted` says how many the limit held back.
    #[serde(default)]
    limit: Option<usize>,
}

/// One row of `list_identities`. Mirrors `docket-core`'s `IdentityCount`
/// (ADR-0025) — every count field is defaulted so an older `docket-core`
/// that predates one of them still deserializes rather than failing the
/// whole call, the same tolerance `TopicCountDto` applies.
#[derive(Debug, Serialize, Deserialize)]
struct IdentityCountDto {
    identity: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    requester: i64,
    #[serde(default)]
    assignee: i64,
    #[serde(default)]
    topic: i64,
    #[serde(default)]
    not_closed: i64,
    #[serde(default)]
    registered_worker: bool,
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
    /// The part of `owned_by` registered for the topic itself rather than
    /// through a shorter prefix. Defaulted for the same reason as `aliases`.
    #[serde(default)]
    owned_by_exact: Vec<String>,
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
    /// Absent from servers older than ADR-0028; 0 is never a real value.
    #[serde(default)]
    seq: i64,
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
    /// Rows exactly as the server sent them: with `fields` they are not whole items, and the server's own
    /// compact form is the point.
    items: Vec<serde_json::Value>,
    /// Matching rows not in this answer (the server's `X-Omitted`), present only when there are some.
    #[serde(skip_serializing_if = "Option::is_none")]
    omitted: Option<usize>,
    /// Where to continue (`X-Next-Offset`), present with `omitted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<usize>,
    /// Rows matching the filters before `limit`/`offset` were applied — an
    /// MCP tool result has no header channel (unlike the HTTP response this
    /// is read from), so this is how a caller learns whether to page with
    /// `offset` instead of assuming `items` is everything. See ADR-0014.
    total: usize,
}

/// What the server said about how it shaped a list (ADR-0029). `shaped` is false for a docket-core that
/// predates the read contract and ignored the parameters, in which case the adapter shapes the page itself.
#[derive(Debug, Default, Clone, PartialEq)]
struct ReadMeta {
    shaped: bool,
    total: Option<usize>,
    omitted: Option<usize>,
    next_offset: Option<usize>,
}

fn meta_from_headers(h: &reqwest::header::HeaderMap) -> ReadMeta {
    let num = |name: &str| {
        h.get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
    };
    ReadMeta {
        shaped: h.contains_key("X-Shaped"),
        total: num("X-Total-Count"),
        omitted: num("X-Omitted"),
        next_offset: num("X-Next-Offset"),
    }
}

/// Same shape as `respond`, but for `list_items`/`search_items`: reads the
/// `X-Total-Count` header docket-core's HTTP layer sets (ADR-0014) before
/// consuming the body, and re-wraps the bare `ItemDto[]` as `{items,
/// total}` — the one output shape an MCP caller can actually see.
async fn respond_paginated(resp: reqwest::Response) -> Result<(CallToolResult, bool), McpError> {
    let status = resp.status();
    let meta = meta_from_headers(resp.headers());
    let total = meta.total;
    let content_type = content_type(&resp);
    let bytes = resp.bytes().await.map_err(unreachable_error)?;
    if status.is_success() {
        let items: Vec<serde_json::Value> = serde_json::from_slice(&bytes).map_err(|e| {
            McpError::internal_error(unparseable_body(status, &content_type, &e), None)
        })?;
        // Falls back to the page length rather than failing the whole call
        // if the header is ever missing/malformed — a caller still gets a
        // correct, if unconfirmed-total, result instead of an opaque error.
        let total = total.unwrap_or(items.len());
        let block = ContentBlock::json(&PaginatedItems {
            items,
            omitted: meta.omitted,
            next_offset: meta.next_offset,
            total,
        })?;
        Ok((CallToolResult::success(vec![block]), meta.shaped))
    } else {
        let message = serde_json::from_slice::<ErrorBody>(&bytes)
            .map(|b| b.error)
            .unwrap_or_else(|_| format!("docket-core returned {status}"));
        Ok((
            CallToolResult::error(vec![ContentBlock::text(message)]),
            meta.shaped,
        ))
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

/// `updated_after` is applied by docket-core, but docket-mcp and docket-core
/// are released separately and an older core silently ignores a query
/// parameter it does not know -- the caller would get the whole list back for
/// a delta request, with no error. This applies the same filter to the page,
/// and says so when it had to.
fn enforce_updated_after(result: CallToolResult, after: Option<i64>) -> CallToolResult {
    let Some(after) = after else {
        return result;
    };
    if result.is_error == Some(true) {
        return result;
    }
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(serde_json::Value::Object(mut page)) =
        serde_json::from_str::<serde_json::Value>(&text.text)
    else {
        return result;
    };
    let Some(serde_json::Value::Array(rows)) = page.remove("items") else {
        return result;
    };
    let before = rows.len();
    // A row without `updated_at` cannot be judged, so it stays.
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|r| {
            r.get("updated_at")
                .and_then(|v| v.as_i64())
                .is_none_or(|u| u > after)
        })
        .collect();
    let dropped = before - rows.len();
    page.insert("items".to_string(), serde_json::Value::Array(rows));
    if dropped > 0 {
        page.insert(
            "note".to_string(),
            serde_json::Value::String(format!(
                "this docket-core ignored updated_after, so {dropped} row(s) were filtered here; total still counts the unfiltered set (upgrade docket-core)"
            )),
        );
    }
    match ContentBlock::json(serde_json::Value::Object(page)) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
}

/// Item keys a caller may name in `fields`.
const ITEM_FIELDS: &[&str] = &[
    "id",
    "seq",
    "topic",
    "title",
    "body",
    "state",
    "resolution",
    "requester",
    "assignee",
    "turn",
    "open",
    "archived_at",
    "tags",
    "created_at",
    "updated_at",
    "state_since",
    "related",
];

/// Rejects a `fields` entry that is not an item key, naming the valid ones --
/// a typo would otherwise silently return rows with the column missing.
fn check_fields(fields: Option<&[String]>) -> Result<(), CallToolResult> {
    let Some(fields) = fields else {
        return Ok(());
    };
    match fields.iter().find(|f| !ITEM_FIELDS.contains(&f.as_str())) {
        None => Ok(()),
        Some(bad) => Err(CallToolResult::error(vec![ContentBlock::text(format!(
            "unknown field `{bad}`; valid fields: {}",
            ITEM_FIELDS.join(", ")
        ))])),
    }
}

/// The adapter's own copy of the server's `fields` / `max_chars` shaping, used only when the server did not
/// shape the page (an older docket-core ignores the parameters). Narrows a `{items, total}` page to the
/// requested `fields` per row, then as many rows as fit in `max_chars`.
fn shape_page(
    result: CallToolResult,
    fields: Option<&[String]>,
    max_chars: Option<usize>,
    offset: usize,
) -> CallToolResult {
    if result.is_error == Some(true) || (fields.is_none() && max_chars.is_none()) {
        return result;
    }
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(serde_json::Value::Object(mut page)) =
        serde_json::from_str::<serde_json::Value>(&text.text)
    else {
        return result;
    };
    let Some(serde_json::Value::Array(rows)) = page.remove("items") else {
        return result;
    };
    let rows: Vec<serde_json::Value> = match fields {
        None => rows,
        Some(keep) => rows
            .into_iter()
            .map(|row| match row {
                serde_json::Value::Object(map) => serde_json::Value::Object(
                    map.into_iter()
                        .filter(|(k, _)| k == "id" || keep.iter().any(|f| f == k))
                        .collect(),
                ),
                other => other,
            })
            .collect(),
    };
    // A row is added while the running JSON length stays within the budget;
    // the first row always goes in, so a budget smaller than one row cannot
    // leave the caller with an empty page and no way forward.
    let mut kept = rows.len();
    if let Some(budget) = max_chars {
        let mut used = 0usize;
        kept = 0;
        for row in &rows {
            let cost = row.to_string().chars().count() + usize::from(kept > 0);
            if kept > 0 && used + cost > budget {
                break;
            }
            used += cost;
            kept += 1;
        }
    }
    // Same meaning as the server's X-Omitted: every matching row not in this answer.
    let total = page
        .get("total")
        .and_then(|v| v.as_u64())
        .map(|t| t as usize);
    let omitted = total.map_or(rows.len() - kept, |t| t.saturating_sub(offset + kept));
    page.insert(
        "items".to_string(),
        serde_json::Value::Array(rows.into_iter().take(kept).collect()),
    );
    if omitted > 0 {
        page.insert("omitted".to_string(), serde_json::json!(omitted));
        page.insert("next_offset".to_string(), serde_json::json!(offset + kept));
    }
    match ContentBlock::json(serde_json::Value::Object(page)) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
}

/// The comments of one item, shaped for an agent's context: the item id once
/// instead of on every comment, optionally only those after `since`, only the
/// newest `tail`, and within `max_chars` newest-first. `total` counts them all;
/// `older` says how many earlier comments were left out.
fn shape_comments(
    item_id: &str,
    comments: Vec<CommentDto>,
    since: Option<i64>,
    after: Option<i64>,
    tail: Option<usize>,
    max_chars: Option<usize>,
) -> serde_json::Value {
    let total = comments.len();
    let mut kept: Vec<CommentDto> = comments
        .into_iter()
        .filter(|c| since.is_none_or(|s| c.created_at > s))
        // A server older than ADR-0028 numbers no comments (0); `after` cannot
        // apply to those, so they are kept rather than all dropped.
        .filter(|c| after.is_none_or(|a| c.seq == 0 || c.seq > a))
        .collect();
    if let Some(n) = tail {
        let drop = kept.len().saturating_sub(n);
        kept.drain(..drop);
    }
    let slim = slim_comment;
    if let Some(budget) = max_chars {
        // Newest first: the latest comments are the ones a reader needs, so the
        // budget is spent from the end and the newest is always kept.
        let mut used = 0usize;
        let mut from = kept.len();
        for (i, c) in kept.iter().enumerate().rev() {
            let cost = slim(c).to_string().chars().count() + usize::from(from != kept.len());
            if from != kept.len() && used + cost > budget {
                break;
            }
            used += cost;
            from = i;
        }
        kept.drain(..from);
    }
    comments_value("item_id", item_id, &kept, total, total - kept.len())
}

/// The comments result: the item id once, the whole thread's size, the comments, and how many earlier ones
/// were left out (present only when there are some).
fn comments_value(
    owner_key: &str,
    owner: &str,
    kept: &[CommentDto],
    total: usize,
    older: usize,
) -> serde_json::Value {
    let rows = kept.iter().map(slim_comment).collect::<Vec<_>>();
    comments_page(owner_key, owner, rows, total, older)
}

/// `{<owner_key>: owner, total, comments, older?}` -- the one result shape of a thread, whether it
/// belongs to an item (`item_id`) or to a topic (`topic`).
fn comments_page(
    owner_key: &str,
    owner: &str,
    rows: Vec<serde_json::Value>,
    total: usize,
    older: usize,
) -> serde_json::Value {
    let mut out = serde_json::json!({
        owner_key: owner,
        "total": total,
        "comments": rows,
    });
    if older > 0 {
        out["older"] = serde_json::json!(older);
    }
    out
}

fn slim_comment(c: &CommentDto) -> serde_json::Value {
    let mut out = serde_json::json!({
        "id": c.id, "author": c.author, "body": c.body, "created_at": c.created_at,
    });
    if c.seq > 0 {
        out["seq"] = serde_json::json!(c.seq);
    }
    out
}

/// A topic comment as the thread result shows it: the same fields as `slim_comment`.
fn slim_topic_comment(c: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for key in ["id", "author", "body", "created_at", "seq"] {
        if let Some(v) = c.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    serde_json::Value::Object(out)
}

/// Who a `cc` line is written as: the item's requester, else this session's worker. `None` when
/// neither exists -- a comment needs an author, and the line is not worth inventing one for.
fn cc_author(requester: Option<&str>, worker_id: Option<String>) -> Option<String> {
    requester.map(str::to_string).or(worker_id)
}

/// Why a comment call named the wrong number of owners.
const ONE_OWNER: &str = "give exactly one of item_id or topic";

/// What a comment call is about: an item's thread or a topic's.
enum CommentOwner<'a> {
    Item(&'a str),
    Topic(&'a str),
}

fn comment_owner<'a>(
    item_id: &'a Option<String>,
    topic: &'a Option<String>,
) -> Result<CommentOwner<'a>, CallToolResult> {
    let item = item_id.as_deref().filter(|s| !s.trim().is_empty());
    let topic = topic.as_deref().filter(|s| !s.trim().is_empty());
    match (item, topic) {
        (Some(i), None) => Ok(CommentOwner::Item(i)),
        (None, Some(t)) => Ok(CommentOwner::Topic(t)),
        _ => Err(CallToolResult::error(vec![ContentBlock::text(ONE_OWNER)])),
    }
}

/// Rows of a whole-table listing narrowed by `q` and `limit`.
/// Returns the kept rows, how many rows matched, and how many `limit` held back.
fn narrow_table(
    rows: Vec<serde_json::Value>,
    q: Option<&str>,
    limit: Option<usize>,
) -> (Vec<serde_json::Value>, usize, usize) {
    let needle = q
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);
    let matches = |row: &serde_json::Value| -> bool {
        let Some(needle) = needle.as_deref() else {
            return true;
        };
        // An identity row carries a numeric `topic` count, so only a string
        // counts as the name.
        let name = ["topic", "identity"]
            .iter()
            .find_map(|k| row.get(*k).and_then(|v| v.as_str()))
            .unwrap_or_default();
        let aliases = row.get("aliases").and_then(|v| v.as_array());
        std::iter::once(name)
            .chain(aliases.into_iter().flatten().filter_map(|a| a.as_str()))
            .any(|s| s.to_lowercase().contains(needle))
    };
    let matched: Vec<serde_json::Value> = rows.into_iter().filter(|r| matches(r)).collect();
    let total = matched.len();
    // A limit of 0 would hand back nothing and no way forward; one row is the floor.
    let cap = limit.unwrap_or(TABLE_DEFAULT_LIMIT).max(1);
    let kept: Vec<serde_json::Value> = matched.into_iter().take(cap).collect();
    let omitted = total - kept.len();
    (kept, total, omitted)
}

/// Default row cap of `list_topics` / `list_identities`.
const TABLE_DEFAULT_LIMIT: usize = 50;

/// Re-wraps a successful bare-array listing as `{total, <name>, omitted?}`
/// after trimming each row with `trim`. The server narrows and caps it (`meta.shaped`); for an older
/// server this narrows with `q` / `limit` itself.
/// A result it does not recognise comes back untouched.
fn table_result(
    result: CallToolResult,
    meta: &ReadMeta,
    name: &str,
    trim: impl Fn(serde_json::Value) -> serde_json::Value,
    q: Option<&str>,
    limit: Option<usize>,
) -> CallToolResult {
    if result.is_error == Some(true) {
        return result;
    }
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(serde_json::Value::Array(rows)) = serde_json::from_str::<serde_json::Value>(&text.text)
    else {
        return result;
    };
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(trim)
        .filter(|r| !r.is_null())
        .collect();
    // The server already narrowed and capped the table; an older one returned all of it.
    let (kept, total, omitted) = if meta.shaped {
        let n = rows.len();
        (rows, meta.total.unwrap_or(n), meta.omitted.unwrap_or(0))
    } else {
        narrow_table(rows, q, limit)
    };
    let mut out = serde_json::json!({ "total": total, name: kept });
    if omitted > 0 {
        out["omitted"] = serde_json::json!(omitted);
    }
    match ContentBlock::json(out) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
}

/// What a write's result keeps of the item it changed: where the item stands
/// now, not a copy of what the caller just sent. Echoing the whole item made
/// every acknowledgement cost as much as reading it again (about 2.2K chars
/// each, measured over real agent sessions). `get_item` returns the full
/// item for the rare caller that wants it; `claim_item` is the one write
/// that still returns it, because claiming is how an agent picks the work up.
const ITEM_ACK: &[&str] = &[
    "id",
    "seq",
    "topic",
    "state",
    "resolution",
    "assignee",
    "turn",
    "state_since",
];

/// `ITEM_ACK` plus the one field `archive_item` exists to change.
const ARCHIVE_ACK: &[&str] = &[
    "id",
    "seq",
    "topic",
    "state",
    "resolution",
    "assignee",
    "turn",
    "state_since",
    "archived_at",
];

/// A comment's acknowledgement: that it landed, where, and under whose name
/// (the one thing a caller relying on the `DOCKET_WORKER_ID` fallback cannot
/// know in advance) -- not its body back.
const COMMENT_ACK: &[&str] = &["id", "item_id", "author", "created_at", "seq"];

/// The same for a comment written to a topic.
const TOPIC_COMMENT_ACK: &[&str] = &["id", "topic", "author", "created_at", "seq"];

/// Narrows a successful write result to `keep`. Advisories (any key ending in
/// `_advisory`) always pass: they are the part of a result the caller could
/// not have known, and dropping one would turn "worth attaching" into silence.
/// `null` fields are omitted -- absent and `null` mean the same to a caller
/// reading JSON, and an open item has several. Anything unrecognised (an error,
/// a non-object) comes back untouched.
fn ack(result: CallToolResult, keep: &[&str]) -> CallToolResult {
    if result.is_error == Some(true) {
        return result;
    }
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(&text.text)
    else {
        return result;
    };
    let slim: serde_json::Map<String, serde_json::Value> = obj
        .into_iter()
        .filter(|(k, v)| !v.is_null() && (keep.contains(&k.as_str()) || k.ends_with("_advisory")))
        .collect();
    match ContentBlock::json(serde_json::Value::Object(slim)) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
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
fn with_gap_hint(result: CallToolResult, hints: GapHints) -> CallToolResult {
    let result = match hints.unregistered_open {
        Some(hint) => with_extra_field(result, "unregistered_open", hint),
        None => result,
    };
    match hints.prefix_only_open {
        Some(hint) => with_extra_field(result, "prefix_only_open", hint),
        None => result,
    }
}

/// What `report_gaps` adds — see `DocketMcp::fetch_gap_hint`.
#[derive(Default)]
struct GapHints {
    unregistered_open: Option<serde_json::Value>,
    prefix_only_open: Option<serde_json::Value>,
}

/// The `total` of an already-serialized paginated result, if it has one.
fn result_total(result: &CallToolResult) -> Option<i64> {
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return None;
    };
    serde_json::from_str::<serde_json::Value>(&text.text)
        .ok()?
        .get("total")?
        .as_i64()
}

/// Splices one advisory field into an already-serialized tool result, or
/// returns it untouched if anything about that is not possible. Every
/// advisory goes through here, so the "parse, insert, re-serialize, never
/// fail the call over a hint" contract exists once instead of once per hint.
fn with_extra_field(result: CallToolResult, key: &str, hint: serde_json::Value) -> CallToolResult {
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return result;
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text.text) else {
        return result;
    };
    let Some(obj) = value.as_object_mut() else {
        return result;
    };
    obj.insert(key.to_string(), hint);
    match ContentBlock::json(&value) {
        Ok(block) => CallToolResult::success(vec![block]),
        Err(_) => result,
    }
}

#[tool_router]
impl DocketMcp {
    #[tool(
        description = "Register as a worker, reporting which topic prefixes you own. id may be omitted if this session's DOCKET_WORKER_ID is set. Re-registering an id that differs only in case lands on the existing row and returns its canonical spelling -- adopt what comes back. A bare id with no org scope (`widget` where the server knows `acme/widget`) is a *different* identity, so it creates a second worker row instead; that case cannot be folded automatically, so the result carries an id_advisory naming the scoped spellings it collides with. Registration always succeeds either way"
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
        let result = respond::<WorkerDto>(resp).await?;
        if result.is_error == Some(true) {
            return Ok(result);
        }
        // Advisory, never fatal -- same contract `create_item`'s two
        // advisories have. The registration already happened.
        let Ok(Some(note)) = self.fetch_unscoped_advisory(&id, "worker id").await else {
            return Ok(result);
        };
        Ok(with_extra_field(
            result,
            "id_advisory",
            serde_json::Value::String(note),
        ))
    }

    #[tool(
        description = "What's new since your cursor: comments and transitions on items you are requester/assignee of, or that sit under your registered topics -- even while turn stays with the other side (an assignee answering in a comment without submitting) -- and comments written to your registered topics (those events have `topic`, no `item_id`). Always advance `since` to the returned cursor, even after an empty result. for_worker defaults to this session's DOCKET_WORKER_ID."
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

    #[tool(
        description = "File a new item in front of a topic. It is never refused over an advisory, but the result may carry: topic_advisory (no registered worker serves the topic, or only a broad prefix does), topic_spelling_advisory / requester_advisory (a bare name this server also knows with an org scope -- separate identities whose queries never meet; correct the field or ask for an alias). Pass `cc` (a channel topic) to also tell that channel with one line; off otherwise. The result is a short acknowledgement (id, seq, topic, state); get_item returns the whole item."
    )]
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
        let result = ack(respond::<ItemDto>(resp).await?, ITEM_ACK);
        if result.is_error == Some(true) {
            return Ok(result);
        }
        // Advisory, never fatal: a failed advisory lookup must not turn a
        // successful create into a tool error. The item exists either way,
        // so the worst case is the caller simply isn't told.
        let result = match self.fetch_topic_advisory(&p.topic).await {
            Ok(Some(note)) => with_topic_advisory(result, note),
            _ => result,
        };
        // A *separate* field from `topic_advisory`, not a second sentence
        // inside it: that one asks whether anyone serves the topic, this
        // asks whether the topic is spelled as a bare leaf of one already
        // known. Both can fail at once, and merging them would lose which
        // did.
        let result = self
            .with_unscoped_advisory(result, &p.topic, "topic", "topic_spelling_advisory")
            .await;
        let result = self.tell_channel(result, &p).await;
        let Some(requester) = p.requester.as_deref() else {
            return Ok(result);
        };
        Ok(self
            .with_unscoped_advisory(result, requester, "requester", "requester_advisory")
            .await)
    }

    /// The `cc` of `create_item` (ADR-0028): writes one topic comment naming the new item in the
    /// channel the caller asked to tell. Off unless `cc` is given. The item already exists, so a
    /// failure here is an advisory (`cc_advisory`), never a failed create.
    async fn tell_channel(&self, result: CallToolResult, p: &CreateItemParams) -> CallToolResult {
        let Some(channel) = p.cc.as_deref().map(str::trim).filter(|c| !c.is_empty()) else {
            return result;
        };
        let Some(ContentBlock::Text(text)) = result.content.first() else {
            return result;
        };
        let seq = serde_json::from_str::<serde_json::Value>(&text.text)
            .ok()
            .and_then(|v| v["seq"].as_i64());
        let Some(author) = cc_author(p.requester.as_deref(), docket_worker_id()) else {
            return with_extra_field(
                result,
                "cc_advisory",
                "cc was not sent: no requester and no DOCKET_WORKER_ID to write it as".into(),
            );
        };
        let line = match seq {
            Some(seq) => format!("new item #{seq} in {}: {}", p.topic, p.title),
            None => format!("new item in {}: {}", p.topic, p.title),
        };
        let body = serde_json::json!({ "topic": channel, "author": author, "body": line });
        let sent = match self
            .http
            .post(api_url(&self.base_url, &["topic-comments"]))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp.json::<serde_json::Value>().await.ok(),
            _ => None,
        };
        match sent {
            Some(comment) => with_extra_field(
                result,
                "cc",
                serde_json::json!({ "topic": channel, "seq": comment["seq"] }),
            ),
            None => with_extra_field(
                result,
                "cc_advisory",
                format!("cc was not sent to {channel}; the item itself was created").into(),
            ),
        }
    }

    /// Reads an item's thread. The server applies `since` / `tail` / `max_chars` and reports the rest in
    /// headers; an older docket-core returns the whole thread and this shapes it. `Err` is a tool error to
    /// hand back as is.
    async fn read_thread(
        &self,
        item_id: &str,
        since: Option<i64>,
        after: Option<i64>,
        tail: Option<usize>,
        max_chars: Option<usize>,
    ) -> Result<Result<serde_json::Value, CallToolResult>, McpError> {
        let (since_q, after_q, tail_q, max_q) = (
            since.map(|v| v.to_string()),
            after.map(|v| v.to_string()),
            tail.map(|v| v.to_string()),
            max_chars.map(|v| v.to_string()),
        );
        let resp = self
            .http
            .get(items_url(&self.base_url, item_id, &["comments"]))
            .query(&[
                ("since", since_q.as_deref()),
                ("after", after_q.as_deref()),
                ("tail", tail_q.as_deref()),
                ("max_chars", max_q.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        let meta = meta_from_headers(resp.headers());
        let result = respond::<Vec<CommentDto>>(resp).await?;
        if result.is_error == Some(true) {
            return Ok(Err(result));
        }
        let Some(ContentBlock::Text(text)) = result.content.first() else {
            return Ok(Err(result));
        };
        let Ok(comments) = serde_json::from_str::<Vec<CommentDto>>(&text.text) else {
            return Ok(Err(result));
        };
        Ok(Ok(if meta.shaped {
            let n = comments.len();
            comments_value(
                "item_id",
                item_id,
                &comments,
                meta.total.unwrap_or(n),
                meta.omitted.unwrap_or(0),
            )
        } else {
            shape_comments(item_id, comments, since, after, tail, max_chars)
        }))
    }

    /// Reads a topic's comments (ADR-0028). Only a docket-core that has topic comments answers, so
    /// there is no adapter-side shaping: the server applies `since` / `after` / `tail` / `max_chars`
    /// and reports what it left out in headers. `Err` is a tool error to hand back as is.
    async fn read_topic_thread(
        &self,
        topic: &str,
        since: Option<i64>,
        after: Option<i64>,
        tail: Option<usize>,
        max_chars: Option<usize>,
    ) -> Result<Result<serde_json::Value, CallToolResult>, McpError> {
        let (since_q, after_q, tail_q, max_q) = (
            since.map(|v| v.to_string()),
            after.map(|v| v.to_string()),
            tail.map(|v| v.to_string()),
            max_chars.map(|v| v.to_string()),
        );
        let resp = self
            .http
            .get(api_url(&self.base_url, &["topic-comments"]))
            .query(&[
                ("topic", Some(topic)),
                ("since", since_q.as_deref()),
                ("after", after_q.as_deref()),
                ("tail", tail_q.as_deref()),
                ("max_chars", max_q.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        let meta = meta_from_headers(resp.headers());
        let result = respond::<Vec<serde_json::Value>>(resp).await?;
        if result.is_error == Some(true) {
            return Ok(Err(result));
        }
        let Some(ContentBlock::Text(text)) = result.content.first() else {
            return Ok(Err(result));
        };
        let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(&text.text) else {
            return Ok(Err(result));
        };
        let n = rows.len();
        let rows = rows.iter().map(slim_topic_comment).collect();
        Ok(Ok(comments_page(
            "topic",
            topic,
            rows,
            meta.total.unwrap_or(n),
            meta.omitted.unwrap_or(0),
        )))
    }

    /// Attaches [`Self::fetch_unscoped_advisory`]'s sentence under `field`
    /// when it has one to say. A failed lookup, or a successful one with
    /// nothing to report, leaves the result exactly as it was — every
    /// advisory in this file is the same bargain: worth attaching, never
    /// worth failing an operation that already succeeded over.
    async fn with_unscoped_advisory(
        &self,
        result: CallToolResult,
        identity: &str,
        role: &str,
        field: &str,
    ) -> CallToolResult {
        if result.is_error == Some(true) {
            return result;
        }
        match self.fetch_unscoped_advisory(identity, role).await {
            Ok(Some(note)) => with_extra_field(result, field, note.into()),
            _ => result,
        }
    }

    /// `GET /identities/candidates` — one sentence when the identity just
    /// written looks like a short form of one the server already knows,
    /// `None` otherwise. `role` names what the spelling was written as, so
    /// the sentence can say which field to correct.
    ///
    /// **Two conditions, and the first is what makes this safe on the
    /// hottest path there is.** `create_item` runs on every filing, and the
    /// warning is unbidden — so unlike `report_drift`, which the caller
    /// opts into and scopes to an identity it named, this one cannot afford
    /// to fire on a pair that is legitimately two parties. Mere segment
    /// equality would: `acme/booster` and `other-org/booster` share a leaf
    /// and are unrelated, and they would collide on every create, forever.
    /// Gating on `unscoped` — the given spelling is a bare leaf, the
    /// direction a real drift actually runs in — makes the two-scoped case
    /// structurally unreachable rather than merely unlikely.
    ///
    /// Never blocks: the item is already created by the time this runs, the
    /// same contract `fetch_topic_advisory` has.
    async fn fetch_unscoped_advisory(
        &self,
        identity: &str,
        role: &str,
    ) -> anyhow::Result<Option<String>> {
        #[derive(serde::Deserialize)]
        struct Advisory {
            #[serde(default)]
            unscoped: bool,
            #[serde(default)]
            candidates: Vec<String>,
        }
        let resp: Advisory = self
            .http
            .get(api_url(&self.base_url, &["identities", "candidates"]))
            .query(&[("identity", identity)])
            .send()
            .await?
            .json()
            .await?;
        if !resp.unscoped || resp.candidates.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "This {role} `{identity}` has no org scope, and this server already knows \
             scoped identities with the same name: {}. If one of those is the same party, \
             an admin can declare an alias (POST /aliases) — otherwise the two spellings \
             stay separate identities and neither one's queries will ever see the other's \
             items.",
            resp.candidates
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )))
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
            // `Option`, not `#[serde(default)]`: a docket-core older than
            // these fields omits them, and reading that as "empty" would
            // report every served topic as prefix-only.
            owned_by: Option<Vec<String>>,
            owned_by_exact: Option<Vec<String>>,
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
            return Ok(match (resp.owned_by, resp.owned_by_exact) {
                (Some(owned_by), Some(exact)) if exact.is_empty() && !owned_by.is_empty() => {
                    Some(format!(
                        "Topic `{topic}` is covered only by broad registrations ({}) — no \
                         worker registered this topic itself. Those workers see the item in \
                         `mine`, but none is set up to receive `{topic}` specifically, so an \
                         item that needs an answer may sit unclaimed: check later that it was \
                         claimed, and raise it with a person if it was not.",
                        owned_by
                            .iter()
                            .map(|w| format!("`{w}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
                _ => None,
            });
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

    /// `GET /topics` — registration-gap hints accompanying `mine=<worker>`.
    ///
    /// `unregistered_open`: topics with open, unclaimed items whose
    /// `owned_by` is **empty** — no worker at all is registered for them, the
    /// same blind spot that lets `mine` silently return 0 while items sit
    /// under a topic everyone forgot to register for.
    ///
    /// `prefix_only_open`: topics with open, unclaimed items that *are*
    /// covered, but only through a broader prefix — `owned_by_exact` is
    /// empty. An organization-wide registration makes every topic under it
    /// read as owned, which hides exactly the topics no session is set up to
    /// receive; counting `owned_by` alone, a filer cannot tell them apart.
    ///
    /// Deliberately **not** "topics `owned_by` doesn't name this caller" —
    /// that would also match every topic a *different* worker legitimately
    /// owns, which is normal and not this caller's business; on a server
    /// with many topics that would bury the real signal under everyone
    /// else's ordinary backlog. The result is the same for every caller,
    /// since it names no worker — `mine` still gates whether the flag
    /// applies, because it's only meaningful alongside a query about *your
    /// own* registration. Each map is `None` when empty, and both are when
    /// the call fails (advisory only, like `fetch_topic_advisory`).
    async fn fetch_gap_hint(&self) -> anyhow::Result<GapHints> {
        #[derive(serde::Deserialize)]
        struct TopicRow {
            topic: String,
            #[serde(default)]
            owned_by: Vec<String>,
            // Absent from an older docket-core — not the same as empty,
            // see `fetch_topic_advisory`.
            owned_by_exact: Option<Vec<String>>,
            #[serde(default)]
            open_unclaimed: i64,
        }
        let resp = self
            .http
            .get(api_url(&self.base_url, &["topics"]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(GapHints::default());
        }
        let topics: Vec<TopicRow> = resp.json().await?;

        let mut unregistered = serde_json::Map::new();
        let mut prefix_only = serde_json::Map::new();
        for t in topics {
            if t.open_unclaimed == 0 {
                continue;
            }
            if t.owned_by.is_empty() {
                unregistered.insert(t.topic, serde_json::json!(t.open_unclaimed));
            } else if t.owned_by_exact.is_some_and(|exact| exact.is_empty()) {
                prefix_only.insert(t.topic, serde_json::json!(t.open_unclaimed));
            }
        }
        let non_empty = |map: serde_json::Map<String, serde_json::Value>| {
            (!map.is_empty()).then_some(serde_json::Value::Object(map))
        };
        Ok(GapHints {
            unregistered_open: non_empty(unregistered),
            prefix_only_open: non_empty(prefix_only),
        })
    }

    /// The rows a `mine` query's `state` filter hid. `mine` already spans
    /// three kinds of row — items the worker holds, items it filed that now
    /// wait on its decision (`resolved`), and unclaimed items in its topics
    /// (`open`) — so narrowing it by `state` silently drops whole kinds,
    /// most often the `resolved` replies the worker has to answer. Re-asks
    /// without `state` and reports the difference; `None` when nothing was
    /// hidden or the check fails (advisory only).
    async fn fetch_state_filter_advisory(
        &self,
        mine: &str,
        state: &str,
        archived: Option<&str>,
        shown_total: i64,
    ) -> Option<String> {
        // The total travels in `X-Total-Count`, as `respond_paginated` reads it.
        let total: i64 = self
            .http
            .get(api_url(&self.base_url, &["items"]))
            .query(&[
                ("mine", Some(mine)),
                ("archived", archived),
                ("limit", Some("1")),
                ("summary", Some("true")),
            ])
            .send()
            .await
            .ok()?
            .headers()
            .get("X-Total-Count")?
            .to_str()
            .ok()?
            .parse()
            .ok()?;
        let hidden = total - shown_total;
        (hidden > 0).then(|| {
            format!(
                "state={state} hid {hidden} of the {} rows mine={mine} matches. mine already \
                 covers what you hold, what you filed that now waits on your decision \
                 (resolved) and unclaimed work in your topics (open) — drop state to see \
                 every row it is meant to surface.",
                total
            )
        })
    }

    /// The identities a `report_drift` query is *about*: whichever of
    /// `mine`/`requester`/`assignee`/`topic_scope` the caller actually
    /// filtered on. `topic_scope` belongs here for the sharpest reason of
    /// the four: it names a worker identity, and a drifted worker spelling
    /// is registered for nothing at all, so the query returns zero rather
    /// than merely fewer. An
    /// empty result means the flag has no subject and the hint is skipped —
    /// reporting every collision on the server instead would turn an
    /// advisory into a standing nag, which ADR-0025 deliberately does not
    /// build.
    fn drift_subjects(filters: [Option<&str>; 4]) -> Vec<String> {
        let mut subjects: Vec<String> = Vec::new();
        for filter in filters.into_iter().flatten() {
            let filter = filter.trim();
            if !filter.is_empty() && !subjects.iter().any(|s| s.eq_ignore_ascii_case(filter)) {
                subjects.push(filter.to_string());
            }
        }
        subjects
    }

    /// For each queried identity, the other spellings sharing its last
    /// segment, each with its per-role counts.
    ///
    /// The segment rule itself is **not** reimplemented here — that is
    /// `docket-core`'s (`GET /identities/candidates`), and duplicating it in
    /// an adapter is exactly the "every consumer re-derives one of the
    /// library's own domain facts" that ADR-0022 rejected. This only joins
    /// core's answer to core's counts.
    async fn fetch_drift_hint(
        &self,
        subjects: &[String],
    ) -> anyhow::Result<Option<serde_json::Value>> {
        #[derive(serde::Deserialize)]
        struct CandidatesResponse {
            #[serde(default)]
            candidates: Vec<String>,
        }

        let mut candidates: Vec<String> = Vec::new();
        for subject in subjects {
            let resp = self
                .http
                .get(api_url(&self.base_url, &["identities", "candidates"]))
                .query(&[("identity", subject.as_str())])
                .send()
                .await?;
            if !resp.status().is_success() {
                return Ok(None);
            }
            let body: CandidatesResponse = resp.json().await?;
            for candidate in body.candidates {
                if !candidates
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&candidate))
                {
                    candidates.push(candidate);
                }
            }
        }
        if candidates.is_empty() {
            return Ok(None);
        }

        let resp = self
            .http
            .get(api_url(&self.base_url, &["identities"]))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let rows: Vec<IdentityCountDto> = resp.json().await?;

        let mut hint = serde_json::Map::new();
        for candidate in candidates {
            let Some(row) = rows
                .iter()
                .find(|r| r.identity.eq_ignore_ascii_case(&candidate))
            else {
                continue;
            };
            hint.insert(
                row.identity.clone(),
                serde_json::json!({
                    "requester": row.requester,
                    "assignee": row.assignee,
                    "topic": row.topic,
                    "not_closed": row.not_closed,
                    "registered_worker": row.registered_worker,
                }),
            );
        }
        Ok(if hint.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(hint))
        })
    }

    #[tool(
        description = "List items, newest-touched first (order=\"asc\" for oldest first). Filters: topic, state, assignee, requester, topic_scope, archived. `mine` is the inbox: items you hold (not closed), resolved items you filed that wait on your decision, and unclaimed open items in your topics -- use it alone; adding `state` hides whole kinds of rows (the result then carries state_filter_advisory). Paged by limit/offset; `total` counts every match. Shaping: summary drops bodies, fields trims rows, max_chars bounds the size (truncated/omitted/next_offset say what was held back), updated_after returns only what changed, expand_related adds related items. Every row has state_since (when it entered its current state; null = predates the event log). report_gaps (with mine) and report_drift add hints for the cases where an empty or short result is misleading: work in topics nobody is registered for, and a second spelling of an identity."
    )]
    async fn list_items(
        &self,
        Parameters(p): Parameters<ListItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(error) = check_fields(p.fields.as_deref()) {
            return Ok(error);
        }
        let updated_after = p.updated_after.map(|v| v.to_string());
        let fields_param = p.fields.as_ref().map(|f| f.join(","));
        let max_chars_param = p.max_chars.map(|m| m.to_string());
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
                ("updated_after", updated_after.as_deref()),
                ("fields", fields_param.as_deref()),
                ("max_chars", max_chars_param.as_deref()),
            ])
            .send()
            .await
            .map_err(unreachable_error)?;
        let (result, shaped) = respond_paginated(resp).await?;
        if result.is_error == Some(true) {
            return Ok(result);
        }
        // The server shaped the page, or (an older docket-core) this does.
        let result = if shaped {
            result
        } else {
            shape_page(
                enforce_updated_after(result, p.updated_after),
                p.fields.as_deref(),
                p.max_chars,
                p.offset.unwrap_or(0),
            )
        };
        let result = match (p.mine.as_deref(), p.state.as_deref(), result_total(&result)) {
            (Some(mine), Some(state), Some(total)) => {
                match self
                    .fetch_state_filter_advisory(mine, state, archived.as_deref(), total)
                    .await
                {
                    Some(note) => {
                        with_extra_field(result, "state_filter_advisory", serde_json::json!(note))
                    }
                    None => result,
                }
            }
            _ => result,
        };
        let result = if p.report_gaps == Some(true) && p.mine.is_some() {
            // Advisory, never fatal — same treatment create_item gives
            // fetch_topic_advisory's failure mode.
            match self.fetch_gap_hint().await {
                Ok(hints) => with_gap_hint(result, hints),
                Err(_) => result,
            }
        } else {
            result
        };
        Ok(self
            .apply_drift_hint(
                result,
                p.report_drift,
                [
                    p.mine.as_deref(),
                    p.requester.as_deref(),
                    p.assignee.as_deref(),
                    p.topic_scope.as_deref(),
                ],
            )
            .await)
    }

    #[tool(
        description = "Full-text and/or tag search; call it before create_item to check whether the item already exists. Takes list_items' filters and shaping (mine, assignee, requester, topic_scope, summary, order, expand_related, fields, max_chars, updated_after, report_gaps, report_drift) with the same meaning, and returns the same rows."
    )]
    async fn search_items(
        &self,
        Parameters(p): Parameters<SearchItemsParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(error) = check_fields(p.fields.as_deref()) {
            return Ok(error);
        }
        let mut query_pairs: Vec<(&str, &str)> = Vec::new();
        let updated_after = p.updated_after.map(|v| v.to_string());
        if let Some(u) = updated_after.as_deref() {
            query_pairs.push(("updated_after", u));
        }
        let fields_param = p.fields.as_ref().map(|f| f.join(","));
        if let Some(f) = fields_param.as_deref() {
            query_pairs.push(("fields", f));
        }
        let max_chars_param = p.max_chars.map(|m| m.to_string());
        if let Some(m) = max_chars_param.as_deref() {
            query_pairs.push(("max_chars", m));
        }
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
        let (result, shaped) = respond_paginated(resp).await?;
        if result.is_error == Some(true) {
            return Ok(result);
        }
        let result = if shaped {
            result
        } else {
            shape_page(
                enforce_updated_after(result, p.updated_after),
                p.fields.as_deref(),
                p.max_chars,
                p.offset.unwrap_or(0),
            )
        };
        let result = if p.report_gaps == Some(true) && p.mine.is_some() {
            match self.fetch_gap_hint().await {
                Ok(hints) => with_gap_hint(result, hints),
                Err(_) => result,
            }
        } else {
            result
        };
        Ok(self
            .apply_drift_hint(
                result,
                p.report_drift,
                [
                    p.mine.as_deref(),
                    p.requester.as_deref(),
                    p.assignee.as_deref(),
                    p.topic_scope.as_deref(),
                ],
            )
            .await)
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
        description = "Hand a claimed item back to the requester (claimed -> resolved): the only transition that moves turn to the requester. Use it for finished work AND to ask a question only the requester can answer -- put the question in `reason`. An item left in claimed while you wait is invisible to every query the requester runs. The requester answers with reject_item, which returns the turn to you."
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ITEM_ACK))
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ITEM_ACK))
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ITEM_ACK))
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ITEM_ACK))
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ITEM_ACK))
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ITEM_ACK))
    }

    #[tool(
        description = "Fetch one item by canonical id or numeric seq (`142`, `#142`); works on archived items and always includes the body. expand_related=true adds related:<id> items (both directions); comments=N adds the newest N comments inline (comment_total says how many exist). state_since (epoch ms) is when it entered its current state -- a comment moves updated_at, not this."
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
        let result = respond::<ItemDto>(resp).await?;
        let Some(n) = p.comments.filter(|n| *n > 0) else {
            return Ok(result);
        };
        if result.is_error == Some(true) {
            return Ok(result);
        }
        // Advisory-grade: the item was read fine, so a failed thread read must
        // not turn it into an error -- the caller just does not get comments.
        let Ok(Ok(shaped)) = self
            .read_thread(&p.item_id, None, None, Some(n), None)
            .await
        else {
            return Ok(result);
        };
        let result = with_extra_field(result, "comment_total", shaped["total"].clone());
        Ok(with_extra_field(
            result,
            "comments",
            shaped["comments"].clone(),
        ))
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
        respond::<ItemDto>(resp).await.map(|r| ack(r, ARCHIVE_ACK))
    }

    #[tool(
        description = "Correct an item's requester (a typo, a drifted spelling, or one filed without it). approve_item/reject_item point here when their requester check fails: correct the item, never act under the wrong spelling. Any state, idempotent, recorded as a comment naming the old and new value. Other fields: set_item_assignee, set_item_topic."
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
        let result = respond::<ItemDto>(resp).await?;
        // The call the docs name as *the* remedy for a drifted requester can
        // just as easily write the short form and recreate it, so the same
        // gate applies here as at `create_item`.
        Ok(self
            .with_unscoped_advisory(result, &p.requester, "requester", "requester_advisory")
            .await)
    }

    #[tool(
        description = "Correct an item's assignee: for an unclaimed item, or when the holder's identity drifted or vanished. Reassigns only -- it never clears the assignee (reopen_item does that). Any state, idempotent, recorded as a comment."
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
        let result = respond::<ItemDto>(resp).await?;
        Ok(self
            .with_unscoped_advisory(result, &p.assignee, "assignee", "assignee_advisory")
            .await)
    }

    #[tool(
        description = "Correct an item's topic (filed under the wrong one, e.g. after create_item's topic_advisory). A one-off fix for one item; a spelling used across many items needs an alias, which is the administrator's job outside MCP. Any state, idempotent, recorded as a comment."
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
        let result = respond::<ItemDto>(resp).await?;
        // Same field name `create_item` uses for the same question, so a
        // caller does not have to learn two spellings of one advisory.
        Ok(self
            .with_unscoped_advisory(result, &p.topic, "topic", "topic_spelling_advisory")
            .await)
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

    /// The gate both `list_items` and `search_items` route through, so the
    /// "opt-in, needs a subject, never fatal" contract exists once rather
    /// than twice.
    async fn apply_drift_hint(
        &self,
        result: CallToolResult,
        report_drift: Option<bool>,
        filters: [Option<&str>; 4],
    ) -> CallToolResult {
        if report_drift != Some(true) || result.is_error == Some(true) {
            return result;
        }
        let subjects = Self::drift_subjects(filters);
        if subjects.is_empty() {
            return result;
        }
        match self.fetch_drift_hint(&subjects).await {
            Ok(Some(hint)) => with_extra_field(result, "other_spellings", hint),
            _ => result,
        }
    }

    #[tool(
        description = "Every identity in use (requester, assignee, topic, registered worker -- one namespace; case variants and declared aliases fold into one row) with its item counts per role. Reading it is how you spot one party split across two spellings (`filer` beside `acme/filer`), which no per-field query reports. Narrow with q. Fix one item with set_item_requester/assignee/topic; a recurring spelling needs an alias (an administrator's decision, outside MCP)."
    )]
    async fn list_identities(
        &self,
        Parameters(p): Parameters<ListTableParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = p.limit.unwrap_or(TABLE_DEFAULT_LIMIT).max(1).to_string();
        let resp = self
            .http
            .get(api_url(&self.base_url, &["identities"]))
            .query(&[("q", p.q.as_deref()), ("limit", Some(limit.as_str()))])
            .send()
            .await
            .map_err(unreachable_error)?;
        let meta = meta_from_headers(resp.headers());
        let result = respond::<Vec<IdentityCountDto>>(resp).await?;
        Ok(table_result(
            result,
            &meta,
            "identities",
            |row: serde_json::Value| {
                let mut row = row;
                if row["aliases"].as_array().is_some_and(|a| a.is_empty()) {
                    row.as_object_mut().map(|o| o.remove("aliases"));
                }
                row
            },
            p.q.as_deref(),
            p.limit,
        ))
    }

    #[tool(
        description = "List existing topics and how many non-archived items sit under each, most-populated first — call this before list_items/search_items to discover topic names instead of guessing"
    )]
    async fn list_topics(
        &self,
        Parameters(p): Parameters<ListTableParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = p.limit.unwrap_or(TABLE_DEFAULT_LIMIT).max(1).to_string();
        let resp = self
            .http
            .get(api_url(&self.base_url, &["topics"]))
            .query(&[("q", p.q.as_deref()), ("limit", Some(limit.as_str()))])
            .send()
            .await
            .map_err(unreachable_error)?;
        let meta = meta_from_headers(resp.headers());
        let result = respond::<Vec<TopicCountDto>>(resp).await?;
        Ok(table_result(
            result,
            &meta,
            "topics",
            |row: serde_json::Value| {
                let Ok(t) = serde_json::from_value::<TopicCountDto>(row) else {
                    return serde_json::Value::Null;
                };
                let mut out = serde_json::json!({
                    "topic": t.topic, "count": t.count,
                    "open_unclaimed": t.open_unclaimed, "owned_by": t.owned_by,
                });
                if !t.aliases.is_empty() {
                    out["aliases"] = serde_json::json!(t.aliases);
                }
                if !t.owned_by.is_empty() && t.owned_by_exact.is_empty() {
                    out["prefix_only"] = serde_json::json!(true);
                }
                out
            },
            p.q.as_deref(),
            p.limit,
        ))
    }

    #[tool(
        description = "Add a follow-up note to an item — upstream replies, extra repro info, release notices -- or, with `topic` instead of `item_id`, a comment on a topic that workers registered for it read through list_events (it is not an item). Never changes state or turn — narrating a whole workflow through comments alone leaves the item exactly where claim_item/submit_item last left it. `author` is required — identify yourself (your worker id, or another stable caller identity) so the thread stays readable instead of filling up with docket-core's \"unknown\" fallback. May be omitted if this session's DOCKET_WORKER_ID is set"
    )]
    async fn add_comment(
        &self,
        Parameters(p): Parameters<AddCommentParams>,
    ) -> Result<CallToolResult, McpError> {
        let author = match resolve_identity(p.author, docket_worker_id(), "author") {
            Ok(a) => a,
            Err(error) => return Ok(error),
        };
        let owner = match comment_owner(&p.item_id, &p.topic) {
            Ok(owner) => owner,
            Err(error) => return Ok(error),
        };
        match owner {
            CommentOwner::Item(item_id) => {
                let body = serde_json::json!({ "body": p.body, "author": author });
                let resp = self
                    .http
                    .post(items_url(&self.base_url, item_id, &["comments"]))
                    .json(&body)
                    .send()
                    .await
                    .map_err(unreachable_error)?;
                respond::<CommentDto>(resp)
                    .await
                    .map(|r| ack(r, COMMENT_ACK))
            }
            CommentOwner::Topic(topic) => {
                let body = serde_json::json!({ "topic": topic, "body": p.body, "author": author });
                let resp = self
                    .http
                    .post(api_url(&self.base_url, &["topic-comments"]))
                    .json(&body)
                    .send()
                    .await
                    .map_err(unreachable_error)?;
                respond::<serde_json::Value>(resp)
                    .await
                    .map(|r| ack(r, TOPIC_COMMENT_ACK))
            }
        }
    }

    #[tool(
        description = "List an item's comment thread in chronological order -- or a topic's, with `topic`. Each comment carries a `seq`; pass the last one as `after` to read only what came since."
    )]
    async fn list_comments(
        &self,
        Parameters(p): Parameters<ListCommentsParams>,
    ) -> Result<CallToolResult, McpError> {
        let owner = match comment_owner(&p.item_id, &p.topic) {
            Ok(owner) => owner,
            Err(error) => return Ok(error),
        };
        let read = match owner {
            CommentOwner::Item(item_id) => {
                self.read_thread(item_id, p.since, p.after, p.tail, p.max_chars)
                    .await?
            }
            CommentOwner::Topic(topic) => {
                self.read_topic_thread(topic, p.since, p.after, p.tail, p.max_chars)
                    .await?
            }
        };
        match read {
            Ok(shaped) => Ok(CallToolResult::success(vec![ContentBlock::json(shaped)?])),
            Err(error) => Ok(error),
        }
    }
}

// Named explicitly: without `name`, the macro reports `rmcp` and *its* crate
// version as `serverInfo`, so a client (or a person reading `claude mcp get`)
// cannot tell which docket-mcp release a launcher actually started.
#[tool_handler(name = "docket-mcp")]
impl ServerHandler for DocketMcp {}

fn http_client() -> reqwest::Client {
    // Without a timeout, a hung or unreachable docket-core blocks a tool
    // call — and the calling AI session — forever instead of surfacing as
    // an error the model can react to.
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .default_headers(token_headers(std::env::var("DOCKET_TOKEN").ok().as_deref()))
        .build()
        .expect("reqwest client builds with a plain timeout")
}

/// `Authorization: Bearer <token>` for every request, when `DOCKET_TOKEN` is
/// set (ADR-0026). Safe as a client default because this client only ever
/// talks to `DOCKET_CORE_URL`. Marked sensitive so it never shows up in
/// reqwest's debug output. Unset or blank sends no header — a server without
/// auth ignores one anyway, and a server with auth answers `401` with the
/// reason.
fn token_headers(token: Option<&str>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(token) = token.map(str::trim).filter(|t| !t.is_empty())
        && let Ok(mut value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
    {
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    headers
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

    /// Newest modification time under `dir`, recursively.
    fn newest_mtime(dir: &std::path::Path) -> std::time::SystemTime {
        std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {dir:?}: {e}"))
            .map(|entry| {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    newest_mtime(&entry.path())
                } else {
                    entry.metadata().unwrap().modified().unwrap()
                }
            })
            .max()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    }

    /// `cargo test -p docket-mcp` does not rebuild the docket-core binary
    /// these tests spawn. A binary older than core's sources runs the
    /// previous core, so a test that depends on a new core field fails as if
    /// the MCP code were wrong — or passes against behavior core no longer
    /// has. Refuse to run against one rather than report either.
    fn assert_core_binary_is_current(binary: &std::path::Path) {
        let core_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docket-core");
        let sources = newest_mtime(&core_dir.join("src")).max(
            std::fs::metadata(core_dir.join("Cargo.toml"))
                .unwrap()
                .modified()
                .unwrap(),
        );
        let Ok(built) = std::fs::metadata(binary).and_then(|m| m.modified()) else {
            return; // missing binary: spawn_core's own panic says what to run
        };
        assert!(
            built >= sources,
            "{binary:?} is older than docket-core's sources — run `cargo build -p docket-core` (or `cargo build --workspace --bins`) before `cargo test -p docket-mcp`"
        );
    }

    async fn spawn_core(port: u16, db_path: &std::path::Path) -> CoreProcess {
        spawn_core_with_token(port, db_path, None).await
    }

    async fn spawn_core_with_token(
        port: u16,
        db_path: &std::path::Path,
        token: Option<&str>,
    ) -> CoreProcess {
        let binary = docket_core_binary();
        assert_core_binary_is_current(&binary);
        let mut command = Command::new(&binary);
        command
            .env("DOCKET_PORT", port.to_string())
            .env("DOCKET_DB_PATH", db_path)
            .env_remove("DOCKET_TOKEN")
            .env_remove("DOCKET_PASSWORD");
        if let Some(token) = token {
            command.env("DOCKET_TOKEN", token);
        }
        let child = command.spawn().unwrap_or_else(|e| {
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

    /// ADR-0026 end to end: a docket-core started with `DOCKET_TOKEN` refuses
    /// the MCP client's requests unless the client carries the same token.
    #[tokio::test]
    async fn client_token_is_accepted_only_when_it_matches_the_servers() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-auth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let token = "mcp-e2e-token-0123456789abcdef";
        let core = spawn_core_with_token(18490, &dir.join("auth.db"), Some(token)).await;
        let url = api_url(&core.base_url, &["items"]);

        let with = |t: Option<&str>| {
            reqwest::Client::builder()
                .default_headers(token_headers(t))
                .build()
                .unwrap()
        };
        let status = |client: reqwest::Client| {
            let url = url.clone();
            async move { client.get(url).send().await.unwrap().status() }
        };

        assert_eq!(status(with(Some(token))).await, 200);
        assert_eq!(status(with(None)).await, 401);
        assert_eq!(status(with(Some("some-other-token-0123456789"))).await, 401);

        drop(core);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_headers_sends_a_sensitive_bearer_only_for_a_non_blank_token() {
        let headers = token_headers(Some("  abc123  "));
        let value = headers.get(reqwest::header::AUTHORIZATION).unwrap();
        assert_eq!(value, "Bearer abc123");
        assert!(value.is_sensitive());

        assert!(token_headers(None).is_empty());
        assert!(token_headers(Some("")).is_empty());
        assert!(token_headers(Some("   ")).is_empty());
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
                cc: None,
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
                item_id: Some(item_id.clone()),
                topic: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                cc: None,
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
        // reject hands the turn back to the assignee: that, not `open`, is what the ack says.
        assert_eq!(field(&rejected, "turn"), "assignee");

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
        assert_eq!(field(&approved, "resolution"), "done");

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
                cc: None,
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
                cc: None,
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
                cc: None,
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
                cc: None,
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
                cc: None,
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
                item_id: Some(item_id.clone()),
                topic: None,
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
                item_id: Some(item_id.clone()),
                topic: None,
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
                cc: None,
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
                cc: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                cc: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let added = server
            .add_comment(Parameters(AddCommentParams {
                item_id: Some(item_id.clone()),
                topic: None,
                author: Some("maintainer".to_string()),
                body: "root cause found".to_string(),
            }))
            .await
            .unwrap();
        assert_ne!(added.is_error, Some(true));

        let listed = server
            .list_comments(Parameters(ListCommentsParams {
                item_id: Some(item_id.clone()),
                topic: None,
                after: None,
                since: None,
                tail: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let listed_value: serde_json::Value = serde_json::from_str(text_of(&listed)).unwrap();
        assert_eq!(listed_value["comments"].as_array().unwrap().len(), 1);
        assert_eq!(listed_value["comments"][0]["author"], "maintainer");
    }

    /// A write's result answers "did it land, and where does the item stand
    /// now" -- not "here is everything you just sent". Echoing the item back
    /// made every acknowledgement cost as much as reading it again.
    #[tokio::test]
    async fn write_tools_return_a_minimal_ack_but_claim_still_returns_the_item() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-ack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("ack.db");
        let core = spawn_core(18991, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        let keys = |r: &CallToolResult| -> Vec<String> {
            let v: serde_json::Value = serde_json::from_str(text_of(r)).unwrap();
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let long_body = "a long request body ".repeat(40);

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "a title the caller already knows".to_string(),
                body: Some(long_body.clone()),
                tags: vec!["kind:feature-request".to_string()],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        assert_ne!(
            created.is_error,
            Some(true),
            "create failed: {}",
            text_of(&created)
        );
        let created_keys = keys(&created);
        for must in ["id", "seq", "state", "topic"] {
            assert!(
                created_keys.iter().any(|k| k == must),
                "create ack lacks {must}: {created_keys:?}"
            );
        }
        for must_not in ["body", "title", "tags", "created_at", "updated_at"] {
            assert!(
                !created_keys.iter().any(|k| k == must_not),
                "create ack must not echo {must_not}: {created_keys:?}"
            );
        }
        let item_id = field(&created, "id");

        // claim is the moment an agent picks the work up: it still gets the item.
        let claimed = server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("worker-a".to_string()),
            }))
            .await
            .unwrap();
        let claimed_value: serde_json::Value = serde_json::from_str(text_of(&claimed)).unwrap();
        assert_eq!(claimed_value["body"], long_body.as_str());

        let commented = server
            .add_comment(Parameters(AddCommentParams {
                item_id: Some(item_id.clone()),
                topic: None,
                author: Some("worker-a".to_string()),
                body: "progress note the caller just wrote".to_string(),
            }))
            .await
            .unwrap();
        assert_eq!(
            keys(&commented),
            ["author", "created_at", "id", "item_id", "seq"]
        );

        let submitted = server
            .submit_item(Parameters(SubmitParams {
                item_id: item_id.clone(),
                worker_id: Some("worker-a".to_string()),
                reason: None,
            }))
            .await
            .unwrap();
        let submitted_value: serde_json::Value = serde_json::from_str(text_of(&submitted)).unwrap();
        assert_eq!(submitted_value["state"], "resolved");
        assert_eq!(submitted_value["turn"], "requester");
        assert_eq!(submitted_value["assignee"], "worker-a");
        assert!(
            submitted_value["state_since"].is_number() || submitted_value["state_since"].is_null()
        );
        for must_not in [
            "body",
            "title",
            "tags",
            "requester",
            "created_at",
            "updated_at",
        ] {
            assert!(
                submitted_value.get(must_not).is_none(),
                "submit ack must not echo {must_not}: {submitted_value}"
            );
        }

        let approved = server
            .approve_item(Parameters(ApproveParams {
                item_id: item_id.clone(),
                author: Some("worker-b".to_string()),
            }))
            .await
            .unwrap();
        let approved_value: serde_json::Value = serde_json::from_str(text_of(&approved)).unwrap();
        assert_eq!(approved_value["state"], "closed");
        assert_eq!(approved_value["resolution"], "done");
        assert!(approved_value.get("body").is_none());

        // The full item is still one get_item away.
        let fetched = server
            .get_item(Parameters(GetItemParams {
                item_id,
                expand_related: None,
                comments: None,
            }))
            .await
            .unwrap();
        let fetched_value: serde_json::Value = serde_json::from_str(text_of(&fetched)).unwrap();
        assert_eq!(fetched_value["body"], long_body.as_str());
    }

    #[tokio::test]
    async fn create_item_ack_keeps_the_advisories() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-ack-adv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("ack-adv.db");
        let core = spawn_core(18992, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        // No worker serves this topic, so topic_advisory must survive the slimmer ack.
        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "nobody/serves-this".to_string(),
                title: "t".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&created)).unwrap();
        assert!(v.get("topic_advisory").is_some(), "advisory dropped: {v}");
        assert!(v.get("title").is_none());
    }

    fn page_of(rows: &[serde_json::Value], total: usize) -> CallToolResult {
        CallToolResult::success(vec![
            ContentBlock::json(serde_json::json!({ "items": rows, "total": total })).unwrap(),
        ])
    }

    fn sample_rows(n: usize) -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| {
                serde_json::json!({
                    "id": format!("id-{i}"), "seq": i, "topic": "iyulab/docket",
                    "title": format!("title {i}"), "state": "open", "tags": ["a"],
                })
            })
            .collect()
    }

    #[test]
    fn shape_page_keeps_only_the_requested_fields_and_always_the_id() {
        let fields = vec!["title".to_string(), "state".to_string()];
        let shaped = shape_page(page_of(&sample_rows(2), 2), Some(&fields), None, 0);
        let v: serde_json::Value = serde_json::from_str(text_of(&shaped)).unwrap();
        for row in v["items"].as_array().unwrap() {
            let mut keys: Vec<&str> = row
                .as_object()
                .unwrap()
                .keys()
                .map(|k| k.as_str())
                .collect();
            keys.sort();
            assert_eq!(keys, ["id", "state", "title"]);
        }
        assert_eq!(v["total"], 2);
        assert!(v.get("truncated").is_none());
    }

    #[test]
    fn check_fields_names_the_valid_choices_when_one_is_wrong() {
        assert!(check_fields(Some(&["title".to_string(), "tags".to_string()])).is_ok());
        assert!(check_fields(None).is_ok());
        let err = check_fields(Some(&["titel".to_string()])).unwrap_err();
        assert_eq!(err.is_error, Some(true));
        let msg = text_of(&err);
        assert!(
            msg.contains("titel") && msg.contains("title") && msg.contains("state_since"),
            "{msg}"
        );
    }

    #[test]
    fn shape_page_holds_rows_back_at_max_chars_and_says_where_to_continue() {
        let rows = sample_rows(6);
        let one = serde_json::to_string(&rows[0]).unwrap().chars().count();
        // Room for two rows (plus the comma between them), not three.
        let shaped = shape_page(page_of(&rows, 6), None, Some(2 * one + 3), 0);
        let v: serde_json::Value = serde_json::from_str(text_of(&shaped)).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert_eq!(v["omitted"], 4, "every matching row not in this answer");
        assert_eq!(v["next_offset"], 2);
        assert_eq!(v["total"], 6, "total still counts the unpaged set");
        assert!(v.get("truncated").is_none(), "omitted already says it");
        // A later page: offset counts into next_offset, and rows before it are not "omitted".
        let later = shape_page(page_of(&rows[..4], 6), None, Some(2 * one + 3), 2);
        let v: serde_json::Value = serde_json::from_str(text_of(&later)).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert_eq!(
            (v["omitted"].clone(), v["next_offset"].clone()),
            (serde_json::json!(2), serde_json::json!(4))
        );
    }

    #[test]
    fn shape_page_always_returns_at_least_one_row_and_stays_quiet_when_everything_fits() {
        let rows = sample_rows(3);
        let tiny = shape_page(page_of(&rows, 3), None, Some(1), 0);
        let v: serde_json::Value = serde_json::from_str(text_of(&tiny)).unwrap();
        assert_eq!(
            v["items"].as_array().unwrap().len(),
            1,
            "a budget no row fits must not stall the caller"
        );
        assert_eq!(v["omitted"], 2);
        let roomy = shape_page(page_of(&rows, 3), None, Some(1_000_000), 0);
        let v: serde_json::Value = serde_json::from_str(text_of(&roomy)).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 3);
        assert!(v.get("truncated").is_none() && v.get("omitted").is_none());
    }

    #[test]
    fn enforce_updated_after_drops_rows_an_older_core_failed_to_filter_and_says_so() {
        let mut rows = sample_rows(3);
        for (i, r) in rows.iter_mut().enumerate() {
            r["updated_at"] = serde_json::json!(100 + i as i64);
        }
        let out = enforce_updated_after(page_of(&rows, 3), Some(100));
        let v: serde_json::Value = serde_json::from_str(text_of(&out)).unwrap();
        let ids: Vec<&str> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["id-1", "id-2"], "strictly after, like the server");
        let note = v["note"]
            .as_str()
            .expect("the caller must be told the server did not filter");
        assert!(note.contains("updated_after"), "{note}");
    }

    #[test]
    fn enforce_updated_after_is_silent_when_the_server_already_filtered() {
        let mut rows = sample_rows(2);
        for (i, r) in rows.iter_mut().enumerate() {
            r["updated_at"] = serde_json::json!(500 + i as i64);
        }
        let before = page_of(&rows, 2);
        let out = enforce_updated_after(before, Some(100));
        let v: serde_json::Value = serde_json::from_str(text_of(&out)).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert!(v.get("note").is_none());
        // And with no `updated_after` at all the result is exactly as it came.
        let same = enforce_updated_after(page_of(&rows, 2), None);
        assert_eq!(text_of(&same), text_of(&page_of(&rows, 2)));
    }

    #[test]
    fn enforce_updated_after_ignores_rows_without_the_field_and_error_results() {
        // Projected away by `fields`? Not at this stage: this runs before shaping. A row
        // lacking updated_at cannot be judged, so it stays.
        let out = enforce_updated_after(page_of(&sample_rows(2), 2), Some(100));
        let v: serde_json::Value = serde_json::from_str(text_of(&out)).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        let err = CallToolResult::error(vec![ContentBlock::text("boom".to_string())]);
        assert_eq!(text_of(&enforce_updated_after(err, Some(1))), "boom");
    }

    #[test]
    fn shape_page_leaves_an_error_result_alone() {
        let err = CallToolResult::error(vec![ContentBlock::text("boom".to_string())]);
        let shaped = shape_page(err, Some(&["title".to_string()]), Some(10), 0);
        assert_eq!(shaped.is_error, Some(true));
        assert_eq!(text_of(&shaped), "boom");
    }

    #[tokio::test]
    async fn list_items_fields_max_chars_and_updated_after_work_through_the_real_request() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-shape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(18993, &dir.join("shape.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        let mut last_updated = 0;
        for i in 0..5 {
            let created = server
                .create_item(Parameters(new_item(
                    "iyulab/docket",
                    &format!("row {i}"),
                    None,
                )))
                .await
                .unwrap();
            let got = server
                .get_item(Parameters(GetItemParams {
                    item_id: field(&created, "id"),
                    expand_related: None,
                    comments: None,
                }))
                .await
                .unwrap();
            last_updated =
                serde_json::from_str::<serde_json::Value>(text_of(&got)).unwrap()["updated_at"]
                    .as_i64()
                    .unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        let base = || ListItemsParams {
            mine: None,
            ..list_params("")
        };

        let fields = server
            .list_items(Parameters(ListItemsParams {
                fields: Some(vec!["title".to_string()]),
                ..base()
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&fields)).unwrap();
        assert_eq!(v["items"].as_array().unwrap().len(), 5);
        assert!(v["items"][0].get("body").is_none() && v["items"][0].get("tags").is_none());

        let bad = server
            .list_items(Parameters(ListItemsParams {
                fields: Some(vec!["nope".to_string()]),
                ..base()
            }))
            .await
            .unwrap();
        assert_eq!(bad.is_error, Some(true));

        // Page through with a small budget: nothing lost, nothing repeated.
        let mut seen = Vec::new();
        let mut offset = 0usize;
        for _ in 0..10 {
            let page = server
                .list_items(Parameters(ListItemsParams {
                    fields: Some(vec!["title".to_string()]),
                    max_chars: Some(60),
                    offset: Some(offset),
                    ..base()
                }))
                .await
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(text_of(&page)).unwrap();
            for row in v["items"].as_array().unwrap() {
                seen.push(row["id"].as_str().unwrap().to_string());
            }
            match v["next_offset"].as_u64() {
                Some(n) => offset = n as usize,
                None => break,
            }
        }
        assert_eq!(seen.len(), 5);
        let mut dedup = seen.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), 5, "a continued page must not repeat a row");

        // updated_after: strictly later than the newest touch -> nothing.
        let none = server
            .list_items(Parameters(ListItemsParams {
                updated_after: Some(last_updated),
                ..base()
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&none)).unwrap();
        assert_eq!(v["total"], 0);
        let all = server
            .list_items(Parameters(ListItemsParams {
                updated_after: Some(0),
                ..base()
            }))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(text_of(&all)).unwrap()["total"],
            5
        );
    }

    fn comment(i: i64, body: &str) -> CommentDto {
        CommentDto {
            id: format!("c-{i}"),
            item_id: "item-1".to_string(),
            author: "w".to_string(),
            body: body.to_string(),
            created_at: 1000 + i,
            seq: 0,
        }
    }

    #[test]
    fn shape_comments_states_the_item_once_and_keeps_every_comment_by_default() {
        let all: Vec<CommentDto> = (0..4).map(|i| comment(i, "x")).collect();
        let v = shape_comments("item-1", all, None, None, None, None);
        assert_eq!(v["item_id"], "item-1");
        assert_eq!(v["total"], 4);
        let list = v["comments"].as_array().unwrap();
        assert_eq!(list.len(), 4);
        assert!(
            list[0].get("item_id").is_none(),
            "the id is repeated on every row otherwise"
        );
        assert_eq!(list[0]["id"], "c-0");
        assert!(v.get("older").is_none());
    }

    #[test]
    fn shape_comments_tail_and_since_keep_the_newest_in_chronological_order() {
        let all: Vec<CommentDto> = (0..5).map(|i| comment(i, "x")).collect();
        let v = shape_comments("item-1", all, None, None, Some(2), None);
        let ids: Vec<&str> = v["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["c-3", "c-4"]);
        assert_eq!(v["total"], 5);
        assert_eq!(v["older"], 3);

        let all: Vec<CommentDto> = (0..5).map(|i| comment(i, "x")).collect();
        let v = shape_comments("item-1", all, Some(1002), None, None, None);
        let ids: Vec<&str> = v["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["c-3", "c-4"], "strictly after the instant");
        assert_eq!(v["older"], 3);
    }

    #[test]
    fn shape_comments_max_chars_drops_the_oldest_first_and_always_returns_the_newest() {
        let all: Vec<CommentDto> = (0..4).map(|i| comment(i, &"b".repeat(100))).collect();
        let one = serde_json::json!({"id": "c-0", "author": "w", "body": "b".repeat(100), "created_at": 1000})
            .to_string()
            .chars()
            .count();
        let v = shape_comments("item-1", all, None, None, None, Some(2 * one + 5));
        let ids: Vec<&str> = v["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["c-2", "c-3"]);
        assert_eq!(v["older"], 2);

        let all: Vec<CommentDto> = (0..3).map(|i| comment(i, &"b".repeat(100))).collect();
        let v = shape_comments("item-1", all, None, None, None, Some(1));
        assert_eq!(
            v["comments"].as_array().unwrap().len(),
            1,
            "a budget below one comment must still return the newest"
        );
        assert_eq!(v["comments"][0]["id"], "c-2");
    }

    #[test]
    fn shape_comments_of_an_empty_thread_is_an_empty_list() {
        let v = shape_comments("item-1", vec![], Some(5), None, Some(3), Some(10));
        assert_eq!(v["total"], 0);
        assert_eq!(v["comments"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_comments_and_get_item_comments_work_through_the_real_request() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-thread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(18994, &dir.join("thread.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        let created = server
            .create_item(Parameters(new_item("iyulab/docket", "thread", None)))
            .await
            .unwrap();
        let item_id = field(&created, "id");
        for i in 0..4 {
            server
                .add_comment(Parameters(AddCommentParams {
                    item_id: Some(item_id.clone()),
                    topic: None,
                    author: Some("w".to_string()),
                    body: format!("note {i}"),
                }))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        let params = |tail: Option<usize>| ListCommentsParams {
            item_id: Some(item_id.clone()),
            topic: None,
            after: None,
            since: None,
            tail,
            max_chars: None,
        };

        let all = server
            .list_comments(Parameters(params(None)))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&all)).unwrap();
        assert_eq!(v["total"], 4);
        assert_eq!(v["comments"].as_array().unwrap().len(), 4);
        assert_eq!(v["comments"][0]["body"], "note 0");

        let last2 = server
            .list_comments(Parameters(params(Some(2))))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&last2)).unwrap();
        let bodies: Vec<&str> = v["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["body"].as_str().unwrap())
            .collect();
        assert_eq!(bodies, ["note 2", "note 3"]);

        // get_item with `comments` returns the item and its newest comments in one call.
        let with = server
            .get_item(Parameters(GetItemParams {
                item_id: item_id.clone(),
                expand_related: None,
                comments: Some(2),
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&with)).unwrap();
        assert_eq!(v["title"], "thread");
        assert_eq!(v["comment_total"], 4);
        assert_eq!(v["comments"].as_array().unwrap().len(), 2);
        assert_eq!(v["comments"][1]["body"], "note 3");

        let without = server
            .get_item(Parameters(GetItemParams {
                item_id,
                expand_related: None,
                comments: None,
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(text_of(&without)).unwrap();
        assert!(v.get("comments").is_none() && v.get("comment_total").is_none());
    }

    fn table_rows(n: usize) -> Vec<serde_json::Value> {
        (0..n)
            .map(|i| serde_json::json!({ "topic": format!("iyulab/repo-{i}"), "aliases": [format!("short-{i}")] }))
            .collect()
    }

    #[test]
    fn narrow_table_matches_a_substring_of_the_name_or_an_alias_ignoring_case() {
        let mut rows = table_rows(3);
        rows.push(serde_json::json!({ "topic": "acme/Gateway", "aliases": [] }));
        let (kept, total, omitted) = narrow_table(rows.clone(), Some("gateway"), None);
        assert_eq!((kept.len(), total, omitted), (1, 1, 0));
        let (kept, total, _) = narrow_table(rows, Some("SHORT-2"), None);
        assert_eq!((kept.len(), total), (1, 1), "an alias is searchable too");
    }

    #[test]
    fn narrow_table_caps_at_the_limit_by_default_and_counts_what_it_held_back() {
        let (kept, total, omitted) = narrow_table(table_rows(120), None, None);
        assert_eq!(kept.len(), TABLE_DEFAULT_LIMIT);
        assert_eq!((total, omitted), (120, 70));
        let (kept, total, omitted) = narrow_table(table_rows(120), None, Some(7));
        assert_eq!((kept.len(), total, omitted), (7, 120, 113));
        let (kept, _, omitted) = narrow_table(table_rows(5), None, Some(0));
        assert_eq!(
            (kept.len(), omitted),
            (1, 4),
            "limit 0 still returns a row rather than stalling"
        );
    }

    #[test]
    fn narrow_table_keeps_the_original_order() {
        let (kept, _, _) = narrow_table(table_rows(10), Some("repo"), Some(3));
        let names: Vec<&str> = kept.iter().map(|r| r["topic"].as_str().unwrap()).collect();
        assert_eq!(names, ["iyulab/repo-0", "iyulab/repo-1", "iyulab/repo-2"]);
    }

    #[tokio::test]
    async fn list_topics_and_list_identities_narrow_and_report_prefix_only_coverage() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-tables-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(18995, &dir.join("tables.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        // `bot` is registered for the broad prefix `iyulab` only, so it reaches
        // iyulab/docket without anyone being set up for that topic itself.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("bot".to_string()),
                topics: vec!["iyulab".to_string()],
            }))
            .await
            .unwrap();
        for topic in ["iyulab/docket", "acme/widget"] {
            server
                .create_item(Parameters(new_item(topic, "t", Some("acme/filer"))))
                .await
                .unwrap();
        }
        let all = server
            .list_topics(Parameters(ListTableParams::default()))
            .await
            .unwrap();
        let v = json_value(&all);
        assert_eq!(v["total"], 2);
        let docket = v["topics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["topic"] == "iyulab/docket")
            .unwrap();
        assert_eq!(
            docket["prefix_only"], true,
            "covered only through a broader prefix"
        );
        assert!(
            docket.get("aliases").is_none(),
            "an empty alias list is not worth the characters"
        );
        let widget = v["topics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["topic"] == "acme/widget")
            .unwrap();
        assert!(widget.get("prefix_only").is_none());
        assert_eq!(widget["owned_by"], serde_json::json!([]));

        let one = server
            .list_topics(Parameters(ListTableParams {
                q: Some("WIDGET".to_string()),
                limit: None,
            }))
            .await
            .unwrap();
        assert_eq!(json_value(&one)["topics"].as_array().unwrap().len(), 1);

        let ids = server
            .list_identities(Parameters(ListTableParams {
                q: Some("filer".to_string()),
                limit: Some(1),
            }))
            .await
            .unwrap();
        let v = json_value(&ids);
        assert_eq!(v["identities"].as_array().unwrap().len(), 1);
        assert_eq!(v["identities"][0]["identity"], "acme/filer");
        assert!(v["identities"][0].get("aliases").is_none());
    }

    #[test]
    fn meta_from_headers_tells_a_shaping_server_from_an_older_one() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(
            meta_from_headers(&h),
            ReadMeta::default(),
            "no marker: not shaped"
        );
        h.insert("X-Shaped", "1".parse().unwrap());
        h.insert("X-Total-Count", "9".parse().unwrap());
        h.insert("X-Omitted", "7".parse().unwrap());
        h.insert("X-Next-Offset", "2".parse().unwrap());
        assert_eq!(
            meta_from_headers(&h),
            ReadMeta {
                shaped: true,
                total: Some(9),
                omitted: Some(7),
                next_offset: Some(2)
            }
        );
    }

    #[test]
    fn table_result_trusts_a_shaping_server_and_narrows_for_an_older_one_itself() {
        let rows: Vec<serde_json::Value> = (0..3)
            .map(|i| serde_json::json!({ "topic": format!("iyulab/repo-{i}"), "count": i, "aliases": [] }))
            .collect();
        let raw = || CallToolResult::success(vec![ContentBlock::json(&rows).unwrap()]);
        let keep = |r: serde_json::Value| r;

        // The server already narrowed to 3 of 10: pass its rows and counts through.
        let served = ReadMeta {
            shaped: true,
            total: Some(10),
            omitted: Some(7),
            next_offset: None,
        };
        let v: serde_json::Value = serde_json::from_str(text_of(&table_result(
            raw(),
            &served,
            "topics",
            keep,
            Some("zzz"),
            Some(1),
        )))
        .unwrap();
        assert_eq!(
            (
                v["topics"].as_array().unwrap().len(),
                v["total"].clone(),
                v["omitted"].clone()
            ),
            (3, serde_json::json!(10), serde_json::json!(7))
        );

        // An older server returned everything: narrow here.
        let older = ReadMeta::default();
        let v: serde_json::Value = serde_json::from_str(text_of(&table_result(
            raw(),
            &older,
            "topics",
            keep,
            Some("repo-1"),
            None,
        )))
        .unwrap();
        assert_eq!(
            (v["topics"].as_array().unwrap().len(), v["total"].clone()),
            (1, serde_json::json!(1))
        );
    }

    #[tokio::test]
    async fn list_items_reports_the_rows_a_limit_held_back_and_where_to_continue() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-omitted-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(18996, &dir.join("omitted.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        for i in 0..5 {
            server
                .create_item(Parameters(new_item(
                    "iyulab/docket",
                    &format!("row {i}"),
                    None,
                )))
                .await
                .unwrap();
        }
        let base = || ListItemsParams {
            mine: None,
            ..list_params("")
        };
        let page = server
            .list_items(Parameters(ListItemsParams {
                limit: Some(2),
                ..base()
            }))
            .await
            .unwrap();
        let v = json_value(&page);
        assert_eq!(v["total"], 5);
        assert_eq!(
            (v["omitted"].clone(), v["next_offset"].clone()),
            (serde_json::json!(3), serde_json::json!(2))
        );
        let last = server
            .list_items(Parameters(ListItemsParams {
                limit: Some(2),
                offset: Some(4),
                ..base()
            }))
            .await
            .unwrap();
        assert!(
            json_value(&last).get("omitted").is_none(),
            "nothing left to continue to"
        );
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                    cc: None,
                }))
                .await
                .unwrap();
        }

        let topics = server
            .list_topics(Parameters(ListTableParams::default()))
            .await
            .unwrap();
        assert_ne!(topics.is_error, Some(true));
        let topics_value = json_value(&topics);
        let list = topics_value["topics"].as_array().unwrap();
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
                cc: None,
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
                cc: None,
            }))
            .await
            .unwrap();

        let text = text_of(&mistargeted);
        assert!(
            text.contains("\"id\"") && text.contains("\"state\":\"open\""),
            "the item is still created (acknowledged by id and state): {text}"
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
                cc: None,
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
                cc: None,
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
                cc: None,
            }))
            .await
            .unwrap();

        let topics = server
            .list_topics(Parameters(ListTableParams::default()))
            .await
            .unwrap();
        let value = json_value(&topics);
        let list = value["topics"].as_array().unwrap();
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
                cc: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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

    fn list_params(mine: &str) -> ListItemsParams {
        ListItemsParams {
            topic: None,
            state: None,
            assignee: None,
            requester: None,
            topic_scope: None,
            mine: Some(mine.to_string()),
            archived: None,
            limit: None,
            offset: None,
            summary: None,
            order: None,
            expand_related: None,
            report_gaps: None,
            report_drift: None,
            updated_after: None,
            fields: None,
            max_chars: None,
        }
    }

    fn new_item(topic: &str, title: &str, requester: Option<&str>) -> CreateItemParams {
        CreateItemParams {
            topic: topic.to_string(),
            title: title.to_string(),
            body: None,
            tags: vec![],
            requester: requester.map(str::to_string),
            cc: None,
        }
    }

    /// An organization-wide registration makes every topic under it read as
    /// owned. `report_gaps` names the topics reached *only* that way apart
    /// from true orphans, and `create_item` warns the filer of one — a topic
    /// some worker registered for itself draws neither.
    #[tokio::test]
    async fn prefix_only_coverage_is_reported_apart_from_ownership() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("prefix-only.db");
        let core = spawn_core(18464, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        for (id, topic) in [("iyulab/scout", "iyulab"), ("iyulab/one", "iyulab/one")] {
            server
                .register_worker(Parameters(RegisterWorkerParams {
                    id: Some(id.to_string()),
                    topics: vec![topic.to_string()],
                }))
                .await
                .unwrap();
        }

        let to_prefix_only = server
            .create_item(Parameters(new_item("iyulab/two", "notice", None)))
            .await
            .unwrap();
        let advisory = json_value(&to_prefix_only)["topic_advisory"].clone();
        assert!(
            advisory
                .as_str()
                .is_some_and(|a| a.contains("`iyulab/scout`")),
            "a prefix-only topic warns the filer and names the broad worker: {advisory}"
        );
        let to_owned = server
            .create_item(Parameters(new_item("iyulab/one", "work", None)))
            .await
            .unwrap();
        assert!(
            json_value(&to_owned).get("topic_advisory").is_none(),
            "a topic registered for itself draws no advisory"
        );

        let mut params = list_params("iyulab/one");
        params.report_gaps = Some(true);
        let value = json_value(&server.list_items(Parameters(params)).await.unwrap());
        assert_eq!(
            value["prefix_only_open"],
            serde_json::json!({"iyulab/two": 1})
        );
        assert!(
            value.get("unregistered_open").is_none(),
            "covered by a prefix is not an orphan: {value}"
        );
    }

    /// `mine` spans what a worker holds, what it filed that now waits on its
    /// decision, and unclaimed work in its topics. A `state` filter on top of
    /// it hides whole kinds of row — the result says how many.
    #[tokio::test]
    async fn mine_with_state_reports_the_rows_the_filter_hid() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("mine-state.db");
        let core = spawn_core(18465, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("acme/app".to_string()),
                topics: vec!["acme/app".to_string()],
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(new_item("acme/app", "unclaimed", None)))
            .await
            .unwrap();
        let filed = json_value(
            &server
                .create_item(Parameters(new_item(
                    "acme/lib",
                    "a question",
                    Some("acme/app"),
                )))
                .await
                .unwrap(),
        );
        let id = filed["id"].as_str().unwrap().to_string();
        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: id.clone(),
                worker_id: Some("acme/lib".to_string()),
            }))
            .await
            .unwrap();
        server
            .submit_item(Parameters(SubmitParams {
                item_id: id,
                worker_id: Some("acme/lib".to_string()),
                reason: Some("answered".to_string()),
            }))
            .await
            .unwrap();

        let mut params = list_params("acme/app");
        params.state = Some("open".to_string());
        let value = json_value(&server.list_items(Parameters(params)).await.unwrap());
        assert_eq!(value["total"], 1);
        let advisory = value["state_filter_advisory"].as_str().unwrap_or_default();
        assert!(
            advisory.contains("hid 1 of the 2 rows"),
            "the resolved reply the filter hid is counted: {value}"
        );

        let value = json_value(
            &server
                .list_items(Parameters(list_params("acme/app")))
                .await
                .unwrap(),
        );
        assert_eq!(value["total"], 2);
        assert!(value.get("state_filter_advisory").is_none());
    }

    #[tokio::test]
    async fn list_identities_enumerates_every_role_of_the_identity_class() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("list-identities.db");
        let core = spawn_core(18450, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/filer".to_string()),
                cc: None,
            }))
            .await
            .unwrap();

        let result = server
            .list_identities(Parameters(ListTableParams::default()))
            .await
            .unwrap();
        let value = json_value(&result);
        let row = value["identities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["identity"] == "acme/filer")
            .expect("the requester spelling is enumerated: {value}");
        assert_eq!(row["requester"], 1);
        assert_eq!(row["topic"], 0);
    }

    /// The failure #47 reported: a query under one spelling returns fewer
    /// items than the party actually has, and nothing anywhere says so.
    /// `report_drift=true` has to name the other spelling and what sits
    /// under it, in the same call that came up short.
    #[tokio::test]
    async fn list_items_report_drift_names_the_other_spelling_of_the_queried_identity() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("report-drift.db");
        let core = spawn_core(18451, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        for (requester, title) in [
            ("acme/filer", "long spelling a"),
            ("acme/filer", "long spelling b"),
            ("filer", "short spelling"),
        ] {
            server
                .create_item(Parameters(CreateItemParams {
                    topic: "acme/widget".to_string(),
                    title: title.to_string(),
                    body: None,
                    tags: vec![],
                    requester: Some(requester.to_string()),
                    cc: None,
                }))
                .await
                .unwrap();
        }

        let plain = server
            .list_items(Parameters(ListItemsParams {
                topic: None,
                state: None,
                assignee: None,
                requester: Some("acme/filer".to_string()),
                topic_scope: None,
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
                expand_related: None,
                report_gaps: None,
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let plain_value = json_value(&plain);
        assert_eq!(
            plain_value["items"].as_array().unwrap().len(),
            2,
            "the third item is filed under the other spelling and is not returned"
        );
        assert!(
            plain_value.get("other_spellings").is_none(),
            "no hint without report_drift=true"
        );

        let with_hint = server
            .list_items(Parameters(ListItemsParams {
                topic: None,
                state: None,
                assignee: None,
                requester: Some("acme/filer".to_string()),
                topic_scope: None,
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
                expand_related: None,
                report_gaps: None,
                report_drift: Some(true),
                updated_after: None,
                fields: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let hint_value = json_value(&with_hint);
        assert_eq!(
            hint_value["other_spellings"]["filer"]["requester"],
            serde_json::json!(1),
            "the drifted spelling and its counts must be named: {hint_value}"
        );
    }

    /// `topic_scope` names a worker identity exactly as `mine` does, and a
    /// drifted worker spelling makes it return *nothing* rather than merely
    /// less -- so leaving it out of the subject set would omit the harshest
    /// case from the hint that exists to explain a short result.
    #[tokio::test]
    async fn list_items_report_drift_treats_topic_scope_as_an_identity_filter() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("report-drift-topic-scope.db");
        let core = spawn_core(18453, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        // The working registration, holding items.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("acme/widget-bot".to_string()),
                topics: vec!["acme/widget".to_string()],
            }))
            .await
            .unwrap();
        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();

        // A session whose derived id lost the org scope asks for its queue.
        let result = server
            .list_items(Parameters(ListItemsParams {
                topic: None,
                state: None,
                assignee: None,
                requester: None,
                topic_scope: Some("widget-bot".to_string()),
                mine: None,
                archived: None,
                limit: None,
                offset: None,
                summary: None,
                order: None,
                expand_related: None,
                report_gaps: None,
                report_drift: Some(true),
                updated_after: None,
                fields: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let value = json_value(&result);
        assert_eq!(
            value["items"].as_array().unwrap().len(),
            0,
            "the drifted spelling is registered for nothing, so the queue looks empty"
        );
        assert_eq!(
            value["other_spellings"]["acme/widget-bot"]["registered_worker"],
            serde_json::json!(true),
            "the registered spelling must be named as the reason: {value}"
        );
    }

    /// `register_worker` upserts a *case* drift onto the existing row and
    /// returns the canonical spelling (ADR-0021 chose that over a 409). A
    /// bare-vs-scoped drift is a different identity, so the upsert cannot
    /// see it -- a second worker row appears silently, and from then on
    /// that session's `mine`/`topic_scope` are a different jurisdiction
    /// than the one holding the work.
    #[tokio::test]
    async fn register_worker_warns_when_the_id_is_a_bare_leaf_of_an_existing_scoped_identity() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("register-id-advisory.db");
        let core = spawn_core(18456, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("acme/packages".to_string()),
                topics: vec!["acme/packages".to_string()],
            }))
            .await
            .unwrap();

        let drifted = server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("packages".to_string()),
                topics: vec!["acme/packages".to_string()],
            }))
            .await
            .unwrap();
        let value = json_value(&drifted);
        assert_eq!(
            value["id"], "packages",
            "registration still succeeds under the spelling given: {value}"
        );
        let note = value["id_advisory"]
            .as_str()
            .unwrap_or_else(|| panic!("advisory attached: {value}"));
        assert!(
            note.contains("acme/packages"),
            "names the scoped spelling: {note}"
        );
    }

    /// Re-registering the spelling that is already canonical is the normal
    /// startup path every session takes. It must stay silent.
    #[tokio::test]
    async fn register_worker_does_not_warn_for_a_scoped_id() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("register-id-advisory-scoped.db");
        let core = spawn_core(18457, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        for _ in 0..2 {
            let result = server
                .register_worker(Parameters(RegisterWorkerParams {
                    id: Some("acme/packages".to_string()),
                    topics: vec!["acme/packages".to_string()],
                }))
                .await
                .unwrap();
            assert!(
                json_value(&result).get("id_advisory").is_none(),
                "the ordinary startup registration must stay silent"
            );
        }
    }

    /// The drift this catches is a caller writing the bare leaf where the
    /// rest of the server writes the scoped form -- 8 of the 11 pairs
    /// measured on a live server had exactly that shape. Warning at
    /// `create_item` is what stops a *new* one being added; the enumeration
    /// and `report_drift` only find ones already there.
    #[tokio::test]
    async fn create_item_warns_when_the_requester_is_a_bare_leaf_of_an_existing_scoped_identity() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("create-requester-advisory.db");
        let core = spawn_core(18454, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "established spelling".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/filer".to_string()),
                cc: None,
            }))
            .await
            .unwrap();

        let drifted = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "short spelling".to_string(),
                body: None,
                tags: vec![],
                requester: Some("filer".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let value = json_value(&drifted);
        assert_eq!(
            value["state"], "open",
            "the item is still created -- the warning never blocks: {value}"
        );
        let note = value["requester_advisory"]
            .as_str()
            .unwrap_or_else(|| panic!("advisory attached: {value}"));
        assert!(
            note.contains("acme/filer"),
            "names the scoped spelling: {note}"
        );
    }

    /// `set_item_requester` is what the docs point at as *the* remedy for a
    /// drifted requester. Typing the short form there recreates exactly what
    /// the call was reached for -- and the reason to warn is the premise
    /// this whole line of work rests on: people write short forms naturally,
    /// which being "identity-aware" does not prevent.
    #[tokio::test]
    async fn set_item_requester_warns_when_the_new_value_is_a_bare_leaf() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set-requester-advisory.db");
        let core = spawn_core(18461, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "established".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/filer".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let target = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "needs a requester".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        let item_id = json_value(&target)["id"].as_str().unwrap().to_string();

        let corrected = server
            .set_item_requester(Parameters(SetRequesterParams {
                item_id,
                requester: "filer".to_string(),
                author: Some("acme/bot".to_string()),
            }))
            .await
            .unwrap();
        let value = json_value(&corrected);
        assert_eq!(
            value["requester"], "filer",
            "the correction is applied -- the advisory never blocks it: {value}"
        );
        let note = value["requester_advisory"]
            .as_str()
            .unwrap_or_else(|| panic!("advisory attached: {value}"));
        assert!(
            note.contains("acme/filer"),
            "names the scoped spelling: {note}"
        );
    }

    /// Correcting *to* the scoped spelling is the intended use of this call.
    /// Warning there would fire on every successful repair.
    #[tokio::test]
    async fn set_item_requester_is_silent_when_the_new_value_is_scoped() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set-requester-advisory-scoped.db");
        let core = spawn_core(18462, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let target = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: Some("filer".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let item_id = json_value(&target)["id"].as_str().unwrap().to_string();

        let corrected = server
            .set_item_requester(Parameters(SetRequesterParams {
                item_id,
                requester: "acme/filer".to_string(),
                author: Some("acme/bot".to_string()),
            }))
            .await
            .unwrap();
        assert!(
            json_value(&corrected).get("requester_advisory").is_none(),
            "repairing towards the scoped spelling must stay silent"
        );
    }

    #[tokio::test]
    async fn set_item_assignee_and_topic_warn_on_a_bare_leaf_too() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("set-assignee-topic-advisory.db");
        let core = spawn_core(18463, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        // Two items, because the advisory is computed *after* the write: a
        // move that takes the last item out of `acme/widget` leaves that
        // topic non-existent, so there would be no collision left to
        // report -- correct, but not the case under test here.
        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "stays behind".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/bot".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let target = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/bot".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let item_id = json_value(&target)["id"].as_str().unwrap().to_string();

        let reassigned = server
            .set_item_assignee(Parameters(SetAssigneeParams {
                item_id: item_id.clone(),
                assignee: "bot".to_string(),
                author: Some("acme/bot".to_string()),
            }))
            .await
            .unwrap();
        assert!(
            json_value(&reassigned)["assignee_advisory"].is_string(),
            "`bot` is a bare leaf of `acme/bot`: {}",
            json_value(&reassigned)
        );

        let moved = server
            .set_item_topic(Parameters(SetTopicParams {
                item_id,
                topic: "widget".to_string(),
                author: Some("acme/bot".to_string()),
            }))
            .await
            .unwrap();
        assert!(
            json_value(&moved)["topic_spelling_advisory"].is_string(),
            "`widget` is a bare leaf of `acme/widget`: {}",
            json_value(&moved)
        );
    }

    /// `topic_advisory` asks whether anyone *serves* the topic; this asks
    /// whether the topic is spelled as a bare leaf of one already known.
    /// They are different questions, and a topic can fail the second while
    /// passing the first -- a worker registered on the bare spelling serves
    /// it, so the unserved gate stays silent while items keep landing under
    /// an identity separate from the scoped one.
    #[tokio::test]
    async fn create_item_warns_for_a_bare_leaf_topic_even_when_that_topic_is_served() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("create-topic-spelling.db");
        let core = spawn_core(18458, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        // The scoped spelling exists as a topic.
        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "scoped".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        // ...and a worker is registered on the bare one, so it *is* served.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("acme/bot".to_string()),
                topics: vec!["widget".to_string()],
            }))
            .await
            .unwrap();

        let drifted = server
            .create_item(Parameters(CreateItemParams {
                topic: "widget".to_string(),
                title: "bare".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        let value = json_value(&drifted);
        assert!(
            value.get("topic_advisory").is_none(),
            "the unserved gate is silent -- a worker does serve this topic: {value}"
        );
        let note = value["topic_spelling_advisory"]
            .as_str()
            .unwrap_or_else(|| panic!("the spelling advisory fires instead: {value}"));
        assert!(
            note.contains("acme/widget"),
            "names the scoped topic: {note}"
        );
    }

    /// The two advisories are separate fields on purpose: merging them would
    /// lose which of the two questions actually failed, and an unserved bare
    /// leaf fails both at once.
    #[tokio::test]
    async fn create_item_can_attach_both_topic_advisories_at_once() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("create-topic-both.db");
        let core = spawn_core(18459, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "scoped".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();

        // Nobody is registered for anything, so the bare topic is unserved
        // *and* a bare leaf of a known scoped one.
        let drifted = server
            .create_item(Parameters(CreateItemParams {
                topic: "widget".to_string(),
                title: "bare".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        let value = json_value(&drifted);
        assert!(
            value["topic_advisory"].is_string(),
            "no worker serves it: {value}"
        );
        assert!(
            value["topic_spelling_advisory"].is_string(),
            "and it is a bare leaf of `acme/widget`: {value}"
        );
    }

    /// A scoped topic nobody serves is the ordinary first-item-of-a-new-repo
    /// case. It already gets `topic_advisory`; adding a spelling warning
    /// there would be noise on a filing that is not a drift at all.
    #[tokio::test]
    async fn create_item_does_not_attach_a_spelling_advisory_to_a_scoped_topic() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("create-topic-scoped.db");
        let core = spawn_core(18460, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        let other = server
            .create_item(Parameters(CreateItemParams {
                topic: "other-org/widget".to_string(),
                title: "b".to_string(),
                body: None,
                tags: vec![],
                requester: None,
                cc: None,
            }))
            .await
            .unwrap();
        assert!(
            json_value(&other).get("topic_spelling_advisory").is_none(),
            "two scoped topics sharing a leaf are routinely unrelated"
        );
    }

    /// Two scoped identities sharing a leaf are a normal, legitimate
    /// occurrence (`acme/widget` and `other-org/widget` are unrelated), and
    /// `create_item` runs on the hottest path there is. Firing there would
    /// make the warning noise on every filing forever -- which is why the
    /// gate is the bare-leaf direction, not mere segment equality.
    #[tokio::test]
    async fn create_item_does_not_warn_for_two_scoped_identities_sharing_a_leaf() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("create-requester-advisory-scoped.db");
        let core = spawn_core(18455, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "a".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/booster".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let other = server
            .create_item(Parameters(CreateItemParams {
                topic: "acme/widget".to_string(),
                title: "b".to_string(),
                body: None,
                tags: vec![],
                requester: Some("other-org/booster".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        assert!(
            json_value(&other).get("requester_advisory").is_none(),
            "a scoped identity is not a suspected short form"
        );
    }

    /// `report_drift` needs an identity to compare against. Without one it is
    /// a no-op rather than dumping every collision on the server — the same
    /// "requires a subject" contract `report_gaps` has with `mine`.
    #[tokio::test]
    async fn list_items_report_drift_without_an_identity_filter_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("report-drift-no-identity.db");
        let core = spawn_core(18452, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        for requester in ["acme/filer", "filer"] {
            server
                .create_item(Parameters(CreateItemParams {
                    topic: "acme/widget".to_string(),
                    title: "a".to_string(),
                    body: None,
                    tags: vec![],
                    requester: Some(requester.to_string()),
                    cc: None,
                }))
                .await
                .unwrap();
        }

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
                report_gaps: None,
                report_drift: Some(true),
                updated_after: None,
                fields: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        assert!(
            json_value(&result).get("other_spellings").is_none(),
            "no identity was queried, so there is nothing to report drift against"
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                    cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
            .list_comments(Parameters(ListCommentsParams {
                item_id: Some(item_id.clone()),
                topic: None,
                after: None,
                since: None,
                tail: None,
                max_chars: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
            .list_comments(Parameters(ListCommentsParams {
                item_id: Some(item_id.clone()),
                topic: None,
                after: None,
                since: None,
                tail: None,
                max_chars: None,
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
                cc: None,
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
                comments: None,
            }))
            .await
            .unwrap();
        assert_eq!(json_value(&refetched)["topic"], "iyulab/docket");

        let comments = server
            .list_comments(Parameters(ListCommentsParams {
                item_id: Some(item_id.clone()),
                topic: None,
                after: None,
                since: None,
                tail: None,
                max_chars: None,
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
                cc: None,
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
                        report_drift: None,
                        updated_after: None,
                        fields: None,
                        max_chars: None,
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
            .list_comments(Parameters(ListCommentsParams {
                item_id: Some(item_id.clone()),
                topic: None,
                after: None,
                since: None,
                tail: None,
                max_chars: None,
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
                cc: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let searched_items = json_value(&searched)["items"].as_array().unwrap().clone();
        assert_eq!(searched_items.len(), 1);
        assert_eq!(searched_items[0]["id"], held_id);
    }

    /// `state_since` survives the DTO — the layer where a new `docket-core`
    /// field is silently dropped if nobody adds it, since `ItemDto` is a
    /// strict struct and the tool result is re-serialized from it, not
    /// passed through. Also pins the distinction the field exists for
    /// (ADR-0024): a comment moves `updated_at` and must leave
    /// `state_since` where the transition put it.
    #[tokio::test]
    async fn state_since_survives_the_dto_and_ignores_a_comment() {
        let dir = std::env::temp_dir().join(format!(
            "docket-mcp-test-state-since-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("state-since.db");
        let core = spawn_core(18449, &db_path).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };

        let created = server
            .create_item(Parameters(CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: "standing somewhere".to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/filer".to_string()),
                cc: None,
            }))
            .await
            .unwrap();
        let id = field(&created, "id");
        server
            .claim_item(Parameters(ClaimOrSubmitParams {
                item_id: id.clone(),
                worker_id: Some("w1".to_string()),
            }))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let submitted = server
            .submit_item(Parameters(SubmitParams {
                item_id: id.clone(),
                worker_id: Some("w1".to_string()),
                reason: None,
            }))
            .await
            .unwrap();
        let at_submit = json_value(&submitted)["state_since"].clone();
        assert!(
            at_submit.is_i64(),
            "the DTO must carry the field, not drop it: {submitted:?}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        server
            .add_comment(Parameters(AddCommentParams {
                item_id: Some(id.clone()),
                topic: None,
                author: Some("acme/filer".to_string()),
                body: "still thinking".to_string(),
            }))
            .await
            .unwrap();

        let fetched = json_value(
            &server
                .get_item(Parameters(GetItemParams {
                    item_id: id.clone(),
                    expand_related: None,
                    comments: None,
                }))
                .await
                .unwrap(),
        );
        assert_eq!(fetched["state_since"], at_submit);
        assert!(
            fetched["updated_at"].as_i64().unwrap() > at_submit.as_i64().unwrap(),
            "the comment did move `updated_at` — that is what makes this a real check"
        );

        let listed = server
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
                summary: Some(true),
                order: None,
                expand_related: None,
                report_gaps: None,
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let rows = json_value(&listed)["items"].as_array().unwrap().clone();
        assert_eq!(
            rows[0]["state_since"], at_submit,
            "summary mode drops `body`, not this"
        );
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
                cc: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                cc: None,
            }))
            .await
            .unwrap();
        let item_id = field(&created, "id");

        let fetched = server
            .get_item(Parameters(GetItemParams {
                item_id: item_id.clone(),
                expand_related: None,
                comments: None,
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
                comments: None,
            }))
            .await
            .unwrap();
        assert_ne!(fetched_archived.is_error, Some(true));
        assert_eq!(json_value(&fetched_archived)["id"], item_id);

        let missing = server
            .get_item(Parameters(GetItemParams {
                item_id: "never-created".to_string(),
                expand_related: None,
                comments: None,
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
                cc: None,
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
                cc: None,
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
                comments: None,
            }))
            .await
            .unwrap();
        assert!(json_value(&without_expand).get("related").is_none());

        let with_expand = server
            .get_item(Parameters(GetItemParams {
                item_id: a_id.clone(),
                expand_related: Some(true),
                comments: None,
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
                cc: None,
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
                cc: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                report_drift: None,
                updated_after: None,
                fields: None,
                max_chars: None,
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
                cc: None,
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
                comments: None,
            }))
            .await
            .unwrap();
        assert_ne!(by_bare_seq.is_error, Some(true));
        assert_eq!(json_value(&by_bare_seq)["id"], item_id);

        let by_hash_seq = server
            .get_item(Parameters(GetItemParams {
                item_id: format!("#{seq}"),
                expand_related: None,
                comments: None,
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

    // ---- ADR-0028 through the tool surface: a comment on a topic ----

    #[test]
    fn a_comment_call_names_exactly_one_owner() {
        let some = |s: &str| Some(s.to_string());
        assert!(matches!(
            comment_owner(&some("142"), &None),
            Ok(CommentOwner::Item("142"))
        ));
        assert!(matches!(
            comment_owner(&None, &some("channel/agents")),
            Ok(CommentOwner::Topic("channel/agents"))
        ));
        assert!(comment_owner(&None, &None).is_err(), "neither");
        assert!(
            comment_owner(&some("142"), &some("channel/agents")).is_err(),
            "both"
        );
        assert!(
            comment_owner(&some("  "), &some("channel/agents")).is_ok(),
            "a blank id is not an id"
        );
    }

    #[tokio::test]
    async fn topic_comments_work_through_the_real_request() {
        let dir =
            std::env::temp_dir().join(format!("docket-mcp-test-topic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(19120, &dir.join("topic.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        let write = |topic: Option<&str>, item: Option<&str>, body: &str| {
            let server = &server;
            let params = AddCommentParams {
                item_id: item.map(str::to_string),
                topic: topic.map(str::to_string),
                author: Some("acme/alice".to_string()),
                body: body.to_string(),
            };
            async move { server.add_comment(Parameters(params)).await.unwrap() }
        };

        // Naming neither, or both, is a tool error rather than a guess.
        assert_eq!(write(None, None, "x").await.is_error, Some(true));
        assert_eq!(
            write(Some("channel/agents"), Some("1"), "x").await.is_error,
            Some(true)
        );

        // A write acknowledges where it landed and its seq, not the body back.
        let first = write(Some("channel/agents"), None, "first note").await;
        assert_ne!(first.is_error, Some(true));
        let ack = json_value(&first);
        assert_eq!(ack["topic"], "channel/agents");
        assert!(ack["seq"].as_i64().unwrap() > 0);
        assert!(ack.get("body").is_none());
        for i in 0..3 {
            write(Some("channel/agents"), None, &format!("later {i}")).await;
        }

        let read = |after: Option<i64>, tail: Option<usize>| {
            let server = &server;
            let params = ListCommentsParams {
                item_id: None,
                topic: Some("channel/agents".to_string()),
                after,
                since: None,
                tail,
                max_chars: None,
            };
            async move { json_value(&server.list_comments(Parameters(params)).await.unwrap()) }
        };
        let all = read(None, None).await;
        assert_eq!(all["topic"], "channel/agents");
        assert_eq!(all["total"], 4);
        assert!(all.get("item_id").is_none());
        assert_eq!(all["comments"][0]["body"], "first note");

        // `after` resumes from the last seq seen; `tail` keeps the newest.
        let cursor = ack["seq"].as_i64().unwrap();
        let resumed = read(Some(cursor), None).await;
        assert_eq!(resumed["comments"].as_array().unwrap().len(), 3);
        let newest = read(None, Some(1)).await;
        assert_eq!(newest["comments"][0]["body"], "later 2");
        assert_eq!(newest["older"], 3);

        // A worker registered for the topic follows it through list_events.
        server
            .register_worker(Parameters(RegisterWorkerParams {
                id: Some("acme/reader".to_string()),
                topics: vec!["channel".to_string()],
            }))
            .await
            .unwrap();
        let events = server
            .list_events(Parameters(ListEventsParams {
                for_worker: Some("acme/reader".to_string()),
                since: Some(0),
                limit: None,
            }))
            .await
            .unwrap();
        let body = json_value(&events);
        let first_event = &body["events"][0];
        assert_eq!(first_event["topic"], "channel/agents");
        assert!(first_event.get("item_id").is_none());
        assert_eq!(body["events"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn item_comments_carry_a_seq_and_resume_after_one() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-seq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(19121, &dir.join("seq.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        let created = server
            .create_item(Parameters(new_item("iyulab/docket", "thread", None)))
            .await
            .unwrap();
        let item_id = field(&created, "id");
        let mut seqs = Vec::new();
        for i in 0..3 {
            let ack = server
                .add_comment(Parameters(AddCommentParams {
                    item_id: Some(item_id.clone()),
                    topic: None,
                    author: Some("w".to_string()),
                    body: format!("n{i}"),
                }))
                .await
                .unwrap();
            seqs.push(json_value(&ack)["seq"].as_i64().unwrap());
        }
        let resumed = server
            .list_comments(Parameters(ListCommentsParams {
                item_id: Some(item_id),
                topic: None,
                after: Some(seqs[0]),
                since: None,
                tail: None,
                max_chars: None,
            }))
            .await
            .unwrap();
        let v = json_value(&resumed);
        let bodies: Vec<&str> = v["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["body"].as_str().unwrap())
            .collect();
        assert_eq!(bodies, ["n1", "n2"]);
        assert_eq!(v["comments"][0]["seq"], seqs[1]);
    }

    // ---- ADR-0028: telling a channel about a new item (`cc`) ----

    #[tokio::test]
    async fn cc_tells_a_channel_with_one_line_and_is_off_unless_given() {
        let dir = std::env::temp_dir().join(format!("docket-mcp-test-cc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = spawn_core(19122, &dir.join("cc.db")).await;
        let server = DocketMcp {
            http: http_client(),
            base_url: core.base_url.clone(),
        };
        let file = |cc: Option<&str>, title: &str| {
            let server = &server;
            let params = CreateItemParams {
                topic: "iyulab/docket".to_string(),
                title: title.to_string(),
                body: None,
                tags: vec![],
                requester: Some("acme/alice".to_string()),
                cc: cc.map(str::to_string),
            };
            async move { server.create_item(Parameters(params)).await.unwrap() }
        };
        let thread = || async {
            let read = server
                .list_comments(Parameters(ListCommentsParams {
                    item_id: None,
                    topic: Some("channel/agents".to_string()),
                    after: None,
                    since: None,
                    tail: None,
                    max_chars: None,
                }))
                .await
                .unwrap();
            if read.is_error == Some(true) {
                return Vec::new();
            }
            json_value(&read)["comments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["body"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };

        // No `cc`: the channel hears nothing.
        let quiet = file(None, "stays quiet").await;
        assert!(json_value(&quiet).get("cc").is_none());
        assert!(thread().await.is_empty());

        // `cc`: one line in the channel, and the result says where it went.
        let told = file(Some("channel/agents"), "needs an opinion").await;
        let v = json_value(&told);
        assert_eq!(v["cc"]["topic"], "channel/agents");
        assert!(v["cc"]["seq"].as_i64().unwrap() > 0);
        assert!(v.get("cc_advisory").is_none());
        let seq = v["seq"].as_i64().unwrap();
        assert_eq!(
            thread().await,
            [format!(
                "new item #{seq} in iyulab/docket: needs an opinion"
            )]
        );

        // A blank `cc` is not a channel.
        let blank = file(Some("   "), "blank cc").await;
        assert!(json_value(&blank).get("cc").is_none());
        assert_eq!(thread().await.len(), 1);

        // The item is created either way, and the core was never sent `cc` as an item field.
        let listed = server
            .list_items(Parameters(list_params("")))
            .await
            .unwrap();
        assert_eq!(json_value(&listed)["total"], 3);
    }

    #[test]
    fn a_cc_is_written_as_the_requester_or_else_the_session_worker() {
        assert_eq!(
            cc_author(Some("acme/alice"), Some("acme/bot".to_string())).as_deref(),
            Some("acme/alice")
        );
        assert_eq!(
            cc_author(None, Some("acme/bot".to_string())).as_deref(),
            Some("acme/bot")
        );
        assert_eq!(cc_author(None, None), None, "no one to write it as");
    }
}
