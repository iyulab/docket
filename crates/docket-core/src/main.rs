use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use axum::Extension;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::Query as ExtraQuery;
use docket_core::domain::State as ItemState;
use docket_core::{Item, Store, StoreError};
use serde::{Deserialize, Serialize};

#[tokio::main]
async fn main() {
    let bind = std::env::var("DOCKET_BIND").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = std::env::var("DOCKET_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8420);
    let db_path = std::env::var("DOCKET_DB_PATH").unwrap_or_else(|_| "docket.db".to_string());

    let store = Store::open(&db_path).expect("failed to open store");
    let console_dir =
        std::env::var("DOCKET_CONSOLE_DIR").unwrap_or_else(|_| "console/dist".to_string());
    let app = build_router(Arc::new(store), std::path::Path::new(&console_dir));

    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .expect("DOCKET_BIND/DOCKET_PORT must form a valid socket address");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    println!("docket-core listening on http://{addr} (db: {db_path})");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
}

/// Waits for Ctrl+C so the process doesn't die mid-write — SQLite in WAL
/// mode tolerates a hard kill, but there's no reason to rely on that.
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
}

fn api_routes() -> Router<Arc<Store>> {
    Router::new()
        .route("/workers", post(register_worker))
        .route("/workers/{id}", get(get_worker))
        .route("/items", post(create_item).get(list_items))
        .route(
            "/items/{id}",
            get(get_item).patch(update_item).delete(delete_item),
        )
        .route("/items/{id}/claim", post(claim_item))
        .route("/items/{id}/submit", post(submit_item))
        .route("/items/{id}/approve", post(approve_item))
        .route("/items/{id}/remove", post(remove_item))
        .route("/items/{id}/merge", post(merge_item))
        .route("/items/{id}/force-close", post(force_close_item))
        .route("/items/{id}/force-approve", post(force_approve_item))
        .route("/items/{id}/reject", post(reject_item))
        .route("/items/{id}/reopen", post(reopen_item))
        .route("/items/{id}/block", post(block_item))
        .route("/items/{id}/defer", post(defer_item))
        .route("/items/{id}/archive", post(archive_item))
        .route(
            "/items/{id}/tags",
            post(add_item_tags).delete(remove_item_tags),
        )
        .route("/items/{id}/comments", post(add_comment).get(list_comments))
        .route("/tags", get(list_tags))
        .route("/topics", get(list_topics))
        .fallback(api_not_found)
}

/// Fallback for unmatched `/api/*` paths. Without an explicit fallback here, axum
/// inherits the nested router's fallback from the outer router (the static SPA
/// fallback) — meaning requests like `/api/nonexistent` would wrongly return
/// `index.html` instead of a JSON error. API routes must always return JSON 404s.
async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: "not found".to_string(),
        }),
    )
}

/// What the static service falls back to when no file matches the request path.
///
/// A browser navigating a client-side console route must get `index.html` back.
/// An API client must not: handing it a `200 text/html` shell turns a
/// mis-shaped path into a *successful* call whose body then fails to parse,
/// which is strictly worse than a 404 — it can't be told apart from a broken
/// transport. That is how an unencoded `/` inside a worker id surfaced
/// (docket-works#36): `GET /workers/{org}/{repo}` is two segments, misses the
/// single-segment `/workers/{id}` route, and lands here.
///
/// [`api_not_found`] already guards the `/api` nest, but the same routes are
/// also merged at the root (where every client actually calls them), and there
/// the router's fallback is this static service. Content negotiation is what
/// covers the root without giving up SPA routing: only a request that asks for
/// HTML gets the shell, everything else gets the same JSON 404 the nest
/// returns. A 404'd asset fetch (`Accept: */*`) now reports itself as missing
/// instead of silently resolving to the shell, too.
fn spa_fallback(console_dir: &std::path::Path) -> axum::routing::MethodRouter {
    let index_path = console_dir.join("index.html");
    axum::routing::any(move |headers: HeaderMap| {
        let index_path = index_path.clone();
        async move {
            let wants_html = headers
                .get(axum::http::header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|accept| accept.contains("text/html"));
            // Read per request rather than at startup so a console rebuilt
            // under a running server can't serve fresh assets against a stale
            // shell (mismatched asset hashes render as a blank page).
            match (wants_html, tokio::fs::read(&index_path).await) {
                (true, Ok(html)) => (
                    [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    html,
                )
                    .into_response(),
                _ => api_not_found().await.into_response(),
            }
        }
    })
}

/// Tracks when `docket-core` last handled a request, using a monotonic
/// clock so system-time adjustments never skew the idle calculation.
/// `/status` itself is deliberately excluded from touching this (see
/// `build_router`) — otherwise `docket-core-updater`'s own polling would
/// count as activity and the server would never appear idle.
struct LastRequest(Mutex<Instant>);

impl LastRequest {
    fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }

    fn touch(&self) {
        *self.0.lock().unwrap() = Instant::now();
    }

    fn idle_seconds(&self) -> u64 {
        self.0.lock().unwrap().elapsed().as_secs()
    }
}

async fn track_idle(
    Extension(last_request): Extension<Arc<LastRequest>>,
    request: Request,
    next: Next,
) -> Response {
    last_request.touch();
    next.run(request).await
}

#[derive(Serialize)]
struct StatusBody {
    version: String,
    idle_seconds: u64,
}

/// Not wrapped by `track_idle` (see `build_router`) — polling this endpoint
/// must never reset the idle clock it reports on.
async fn status_handler(Extension(last_request): Extension<Arc<LastRequest>>) -> Json<StatusBody> {
    Json(StatusBody {
        version: format!("v{}", env!("CARGO_PKG_VERSION")),
        idle_seconds: last_request.idle_seconds(),
    })
}

/// `console_dir` (the built docket-console, e.g. `console/dist`) can be missing —
/// `ServeDir`/`ServeFile` defer file I/O to request time, so a missing directory
/// only produces 404s per-request, not a server startup failure.
fn build_router(store: Arc<Store>, console_dir: &std::path::Path) -> Router {
    let static_service =
        tower_http::services::ServeDir::new(console_dir).fallback(spa_fallback(console_dir));
    let last_request = Arc::new(LastRequest::new());

    let tracked = Router::new()
        .merge(api_routes())
        .nest("/api", api_routes())
        .fallback_service(static_service)
        .layer(middleware::from_fn(track_idle))
        .with_state(store);

    Router::new()
        .route("/status", get(status_handler))
        .merge(tracked)
        .layer(Extension(last_request))
}

/// Wraps [`StoreError`] so this binary crate can implement the foreign
/// `IntoResponse` trait for it (orphan rule — `StoreError` lives in the
/// `docket_core` lib crate).
///
/// Maps onto the HTTP status the M1 completion criteria cares about:
/// `Conflict` (losing a claim race, wrong owner) is `409`, not `500` — a
/// worker retrying `list` and claiming elsewhere depends on being able to
/// tell "someone else got it" apart from "the server is broken".
struct ApiError(StoreError);

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        ApiError(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            StoreError::NotFound => StatusCode::NOT_FOUND,
            StoreError::Conflict(_) => StatusCode::CONFLICT,
            StoreError::Validation(_) => StatusCode::BAD_REQUEST,
            StoreError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(ErrorBody {
                error: self.0.to_string(),
            }),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Deserialize)]
struct RegisterWorkerRequest {
    id: String,
    #[serde(default)]
    topics: Vec<String>,
}

async fn register_worker(
    State(store): State<Arc<Store>>,
    Json(req): Json<RegisterWorkerRequest>,
) -> Result<Json<docket_core::Worker>, ApiError> {
    Ok(Json(store.register_worker(&req.id, &req.topics)?))
}

/// Fetches one worker by id — 404 if never registered. This is the only
/// way a caller can positively confirm registration: `list_items`'s
/// `topic_scope` filter treats an unregistered worker the same as one with
/// no matching topics (empty result, not an error — see the read/write
/// not-found asymmetry in docs/usage.md §4), so it can't answer "does this
/// worker exist" on its own.
async fn get_worker(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<docket_core::Worker>, ApiError> {
    Ok(Json(store.get_worker(&id)?))
}

#[derive(Deserialize)]
struct CreateItemRequest {
    topic: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    /// Who this item is being worked for — see
    /// [ADR-0011](../../../../docs/decisions/ADR-0011-requester-assignee-naming.md).
    #[serde(default)]
    requester: Option<String>,
}

async fn create_item(
    State(store): State<Arc<Store>>,
    Json(req): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<Item>), ApiError> {
    let item = store.create_item(
        &req.topic,
        &req.title,
        req.body.as_deref(),
        &req.tags,
        req.requester.as_deref(),
    )?;
    Ok((StatusCode::CREATED, Json(item)))
}

#[derive(Deserialize)]
struct ListItemsQuery {
    /// Exact-match topic filter.
    topic: Option<String>,
    state: Option<String>,
    /// A worker id — narrows the list to items whose `assignee` is exactly
    /// this worker. See
    /// [ADR-0011](../../../../docs/decisions/ADR-0011-requester-assignee-naming.md).
    assignee: Option<String>,
    /// Exact-match on `requester` — symmetric to `assignee`, above.
    requester: Option<String>,
    /// A registered worker's id — narrows the list to items under any topic
    /// that worker is registered for (prefix match, see
    /// [`docket_core::domain::topic_matches`]) — this is a topic-jurisdiction
    /// filter, unrelated to who currently holds any given item (that's
    /// `assignee`, above). This is the "discover it via list" step of the M1
    /// completion criteria. Was `owned_by`, split and renamed by ADR-0010.
    topic_scope: Option<String>,
    /// A worker id — narrows the list to items that worker actually holds a
    /// live stake in right now, OR should be looking at because nobody has
    /// claimed it yet within its own topic jurisdiction: it currently holds
    /// the item (`assignee` match), or it filed the item and the item is
    /// sitting in `resolved` waiting on that requester's approval, or the
    /// item is `open` (unclaimed) under a topic this worker is registered
    /// for (same jurisdiction test as `topic_scope`, above — this is the
    /// one case where `mine` *does* consult topic jurisdiction, since an
    /// unclaimed item in your topic has no other owner to be "held" by).
    /// Combines what a caller otherwise has to know to run as three
    /// separate queries and merge themselves — see
    /// [docket-works#35](https://github.com/iyulab/docket-works/issues/35).
    /// ANDs with every other filter on this struct, same as `assignee`/
    /// `requester` do individually.
    mine: Option<String>,
    /// Excludes archived items by default (`None`/`Some(false)`); `Some(true)`
    /// returns only archived items. See ADR-0013.
    #[serde(default)]
    archived: Option<bool>,
    /// Full-text match against title+body. Presence of `q` and/or `tag`
    /// routes this request through `Store::search_items` instead of
    /// `Store::list_items` — see the branch below.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    tag_match: Option<String>,
    /// Max rows returned, applied after every filter above. Defaults to
    /// `DEFAULT_LIST_LIMIT`, clamped to `[1, MAX_LIST_LIMIT]` regardless of
    /// what the caller passes — see ADR-0014.
    #[serde(default)]
    limit: Option<usize>,
    /// Rows to skip before applying `limit`. Defaults to 0.
    #[serde(default)]
    offset: Option<usize>,
    /// When `true`, every returned item's `body` is `null` regardless of
    /// what's stored — a listing/search caller often doesn't need the full
    /// body of every row, only enough to decide which item (if any) to
    /// fetch in full next via `GET /items/{id}` (unaffected by this flag).
    /// See ADR-0014's "summary mode" re-open trigger.
    #[serde(default)]
    summary: Option<bool>,
    /// `asc` or `desc`, sorting by `updated_at`. Defaults to `desc`
    /// (today's fixed behavior); an unrecognized value also falls back to
    /// `desc` rather than erroring, same as an unrecognized `tag_match`.
    /// See [ADR-0020](../../../docs/decisions/ADR-0020-list-search-order-parameter.md).
    #[serde(default)]
    order: Option<String>,
    /// When `true`, each returned item also resolves its own `related:<id>`
    /// tags (both directions) into a `related` field — the same expansion
    /// `get_item` offers for a single item, applied per row here after
    /// pagination (so the cost is bounded by `limit`, not the unpaged
    /// total). Defaults to `false` (byte-identical response to before, no
    /// `related` key on any item). See
    /// [docket-works#33](https://github.com/iyulab/docket-works/issues/33).
    #[serde(default)]
    expand_related: Option<bool>,
}

/// See ADR-0014: keeps a single-topic or unfiltered query well under the
/// MCP tool-output token cap that motivated this without a caller having to
/// know to ask for a bound.
const DEFAULT_LIST_LIMIT: usize = 50;
/// However large a caller's explicit `limit` is, the response still can't
/// reproduce the original unbounded-response failure by accident.
const MAX_LIST_LIMIT: usize = 200;

/// Uses `axum_extra`'s `Query` rather than `axum::extract::Query`: only the
/// former decodes repeated keys (`?tag=a&tag=b`) into a `Vec`, which the
/// plain extractor rejects with a 400.
async fn list_items(
    State(store): State<Arc<Store>>,
    ExtraQuery(q): ExtraQuery<ListItemsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let limit = q
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let offset = q.offset.unwrap_or(0);
    let state = q.state.as_deref().and_then(ItemState::parse);
    let order = q
        .order
        .as_deref()
        .and_then(docket_core::domain::SortOrder::parse)
        .unwrap_or_default();
    let items = if q.q.is_some() || !q.tag.is_empty() {
        let tag_match = q
            .tag_match
            .as_deref()
            .and_then(docket_core::domain::TagMatch::parse)
            .unwrap_or(docket_core::domain::TagMatch::Any);
        store.search_items(
            q.topic.as_deref(),
            state,
            &q.tag,
            tag_match,
            q.q.as_deref(),
            q.archived,
            order,
        )?
    } else {
        store.list_items(q.topic.as_deref(), state, q.archived, order)?
    };
    let items = match q.topic_scope {
        Some(worker_id) => {
            // An unregistered worker id is treated as "owns no topics", not
            // a 404 — this is a read filter, and every other list_items
            // filter (topic/assignee/requester) answers a non-matching
            // value with an empty result, never an error (see docs/usage.md
            // §4's read/write not-found asymmetry).
            let topics = match store.get_worker(&worker_id) {
                Ok(worker) => worker.topics,
                Err(StoreError::NotFound) => Vec::new(),
                Err(e) => return Err(e.into()),
            };
            items
                .into_iter()
                .filter(|item| {
                    topics
                        .iter()
                        .any(|owned| docket_core::domain::topic_matches(owned, &item.topic))
                })
                .collect()
        }
        None => items,
    };
    // Identity comparisons fold case here and everywhere else — see ADR-0021.
    // These filters live in Rust, over rows SQL already returned, which is why
    // a column collation could not have covered them.
    let items = match q.assignee {
        Some(worker_id) => items
            .into_iter()
            .filter(|item| {
                docket_core::domain::identity_eq_opt(item.assignee.as_deref(), &worker_id)
            })
            .collect(),
        None => items,
    };
    let items: Vec<Item> = match q.requester {
        Some(requester) => items
            .into_iter()
            .filter(|item| {
                docket_core::domain::identity_eq_opt(item.requester.as_deref(), &requester)
            })
            .collect(),
        None => items,
    };
    let items: Vec<Item> = match q.mine {
        Some(worker_id) => {
            // Same not-found-is-empty treatment as `topic_scope`, above — an
            // unregistered worker simply has no topics to match against.
            let topics = match store.get_worker(&worker_id) {
                Ok(worker) => worker.topics,
                Err(StoreError::NotFound) => Vec::new(),
                Err(e) => return Err(e.into()),
            };
            items
                .into_iter()
                .filter(|item| {
                    docket_core::domain::identity_eq_opt(item.assignee.as_deref(), &worker_id)
                        || (docket_core::domain::identity_eq_opt(
                            item.requester.as_deref(),
                            &worker_id,
                        ) && item.state == ItemState::Resolved)
                        || (item.state == ItemState::Open
                            && item.assignee.is_none()
                            && topics.iter().any(|owned| {
                                docket_core::domain::topic_matches(owned, &item.topic)
                            }))
                })
                .collect()
        }
        None => items,
    };
    // Applied last, after every filter above — a SQL-level LIMIT would be
    // computed against the pre-filter row set and could under-fill or empty
    // a page even when more matching rows exist (ADR-0014).
    let total = items.len();
    let mut items: Vec<Item> = items.into_iter().skip(offset).take(limit).collect();
    if q.summary.unwrap_or(false) {
        for item in &mut items {
            item.body = None;
        }
    }
    let expand_related = q.expand_related.unwrap_or(false);
    let items: Vec<ItemWithRelated> = items
        .into_iter()
        .map(|item| -> Result<ItemWithRelated, StoreError> {
            let related = if expand_related {
                Some(store.related_items(&item.id)?)
            } else {
                None
            };
            Ok(ItemWithRelated { item, related })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let mut headers = HeaderMap::new();
    headers.insert(
        "X-Total-Count",
        HeaderValue::from_str(&total.to_string()).expect("a decimal count is always ASCII"),
    );
    Ok((headers, Json(items)))
}

#[derive(Deserialize)]
struct GetItemQuery {
    /// When `true`, also resolves the item's `related:<id>` tags (both
    /// directions) into a `related` field — see
    /// [docket-works#33](https://github.com/iyulab/docket-works/issues/33).
    /// Defaults to `false`, in which case the response is byte-identical to
    /// today's (no `related` key at all, not even an empty array).
    #[serde(default)]
    expand_related: Option<bool>,
}

/// Wraps `Item` so `related` is only ever present in the response when
/// asked for (`skip_serializing_if`) — a request that doesn't set
/// `expand_related` gets a byte-identical response to before this wrapper
/// existed, since `Item` itself never gained this field. Used by `get_item`
/// and, per-row after pagination, by `list_items`/`search_items`
/// (`ListItemsQuery::expand_related`) — every other route returning `Item`
/// (create/claim/...) is unaffected.
#[derive(Serialize)]
struct ItemWithRelated {
    #[serde(flatten)]
    item: Item,
    #[serde(skip_serializing_if = "Option::is_none")]
    related: Option<Vec<docket_core::domain::RelatedItemRef>>,
}

async fn get_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Query(q): Query<GetItemQuery>,
) -> Result<Json<ItemWithRelated>, ApiError> {
    let item = store.get_item(&id)?;
    let related = if q.expand_related.unwrap_or(false) {
        Some(store.related_items(&item.id)?)
    } else {
        None
    };
    Ok(Json(ItemWithRelated { item, related }))
}

#[derive(Deserialize)]
struct UpdateItemRequest {
    /// Corrects `requester` on an existing item — the only field this covers
    /// so far. `requester` is normally set once at creation (ADR-0010); this
    /// covers both an item that never got one and one whose identity drifted
    /// (ADR-0019). Editing `title`/`body`/`topic` post-creation is a separate,
    /// not-yet-built gap (see ROADMAP.md).
    requester: String,
    /// Who made the correction, recorded on the lifecycle comment the change
    /// writes. Optional here and defaulting to `"unknown"`, the same treatment
    /// `POST /items/{id}/comments` gives a direct HTTP caller — `docket-mcp`
    /// requires it be resolvable at its own layer instead.
    #[serde(default)]
    author: Option<String>,
}

async fn update_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateItemRequest>,
) -> Result<Json<Item>, ApiError> {
    let author = req.author.as_deref().unwrap_or("unknown");
    Ok(Json(store.set_item_requester(
        &id,
        author,
        &req.requester,
    )?))
}

#[derive(Deserialize)]
struct WorkerScopedRequest {
    worker_id: String,
}

async fn claim_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<WorkerScopedRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.claim_item(&id, &req.worker_id)?))
}

#[derive(Deserialize)]
struct SubmitItemRequest {
    worker_id: String,
    /// Optional note recorded as a lifecycle comment with the transition —
    /// what the assignee is handing back, when that isn't just "done". See
    /// ADR-0010's 2026-09-08 update.
    #[serde(default)]
    reason: Option<String>,
}

async fn submit_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<SubmitItemRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.submit_item(
        &id,
        &req.worker_id,
        req.reason.as_deref(),
    )?))
}

#[derive(Deserialize)]
struct AuthoredRequest {
    #[serde(default = "default_comment_author")]
    author: String,
}

/// The four closing operations take an optional body: `author` has a serde
/// default, but `Json<T>` alone rejects a request with no `Content-Type`
/// header with `415` *before* serde ever runs — which broke every bodiless
/// caller. `Option<Json<T>>` uses axum's `OptionalFromRequest` impl, which
/// yields `None` when `Content-Type` is absent entirely (a *wrong*
/// content-type is still a `415`), so an omitted body lands on the same
/// `"unknown"` default an omitted `author` field would. See ADR-0012's
/// "defaults to `"unknown"` if omitted, no hard failure".
fn authored_by(body: Option<Json<AuthoredRequest>>) -> String {
    body.map(|Json(req)| req.author)
        .unwrap_or_else(default_comment_author)
}

async fn approve_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    body: Option<Json<AuthoredRequest>>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.approve_item(&id, &authored_by(body))?))
}

async fn remove_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    body: Option<Json<AuthoredRequest>>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.remove_item(&id, &authored_by(body))?))
}

/// Unlike the other four admin closes, `merge` has a required field — a
/// `resolution = duplicate` with no reference to what it duplicates is
/// exactly the traceability gap this exists to close, so this can't be
/// bodiless the way `AuthoredRequest`-based ops are. See ADR-0015.
#[derive(Deserialize)]
struct MergeRequest {
    duplicate_of_id: String,
    #[serde(default = "default_comment_author")]
    author: String,
}

async fn merge_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<MergeRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.merge_item(
        &id,
        &req.duplicate_of_id,
        &req.author,
    )?))
}

async fn force_close_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    body: Option<Json<AuthoredRequest>>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.force_close_item(&id, &authored_by(body))?))
}

async fn force_approve_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    body: Option<Json<AuthoredRequest>>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.force_approve_item(&id, &authored_by(body))?))
}

#[derive(Deserialize)]
struct ReasonedRequest {
    #[serde(default = "default_comment_author")]
    author: String,
    reason: String,
}

async fn reject_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<ReasonedRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.reject_item(&id, &req.author, &req.reason)?))
}

async fn reopen_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<ReasonedRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.reopen_item(&id, &req.author, &req.reason)?))
}

async fn block_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<ReasonedRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.block_item(&id, &req.author, &req.reason)?))
}

async fn defer_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<ReasonedRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.defer_item(&id, &req.author, &req.reason)?))
}

async fn archive_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.archive_item(&id)?))
}

async fn delete_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    store.delete_item(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TagsRequest {
    tags: Vec<String>,
}

async fn add_item_tags(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<TagsRequest>,
) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(store.add_tags(&id, &req.tags)?))
}

async fn remove_item_tags(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<TagsRequest>,
) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(store.remove_tags(&id, &req.tags)?))
}

#[derive(Deserialize)]
struct ListTagsQuery {
    topic: Option<String>,
}

async fn list_tags(
    State(store): State<Arc<Store>>,
    Query(q): Query<ListTagsQuery>,
) -> Result<Json<Vec<docket_core::domain::TagCount>>, ApiError> {
    Ok(Json(store.list_tags(q.topic.as_deref())?))
}

async fn list_topics(
    State(store): State<Arc<Store>>,
) -> Result<Json<Vec<docket_core::domain::TopicCount>>, ApiError> {
    Ok(Json(store.list_topics()?))
}

#[derive(Deserialize)]
struct AddCommentRequest {
    /// Defaults to `"unknown"` if omitted — every comment needs an author
    /// for the thread to be legible, but the design doc doesn't require
    /// the caller to be a registered worker.
    #[serde(default = "default_comment_author")]
    author: String,
    body: String,
}

fn default_comment_author() -> String {
    "unknown".to_string()
}

async fn add_comment(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<AddCommentRequest>,
) -> Result<(StatusCode, Json<docket_core::domain::Comment>), ApiError> {
    let comment = store.add_comment(&id, &req.author, &req.body)?;
    Ok((StatusCode::CREATED, Json(comment)))
}

async fn list_comments(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Vec<docket_core::domain::Comment>>, ApiError> {
    Ok(Json(store.list_comments(&id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn test_app() -> Router {
        build_router(
            Arc::new(Store::open(":memory:").expect("in-memory store opens")),
            std::path::Path::new("/nonexistent-console-dir-for-tests"),
        )
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).expect("response body is valid JSON")
    }

    fn json_request(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// The M1 completion criterion, end to end through the router: create,
    /// discover via ownership-scoped list, claim, submit, approve.
    #[tokio::test]
    async fn full_lifecycle_through_http() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let item = json_body(resp).await;
        let id = item["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/workers",
                serde_json::json!({"id": "w1", "topics": ["iyulab"]}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?topic_scope=w1&state=open")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let listed = json_body(resp).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["state"], "resolved");

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/approve"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let closed = json_body(resp).await;
        assert_eq!(closed["state"], "closed");
        assert_eq!(closed["resolution"], "done");
    }

    /// `requester` round-trips from creation, `assignee` is set by claim,
    /// and `turn` tracks each state transition — see
    /// [ADR-0011](../../../../docs/decisions/ADR-0011-requester-assignee-naming.md).
    #[tokio::test]
    async fn requester_assignee_turn_track_the_lifecycle_over_http() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t", "requester": "reporter-1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let item = json_body(resp).await;
        let id = item["id"].as_str().unwrap().to_string();
        assert_eq!(item["requester"], "reporter-1");
        assert_eq!(item["assignee"], serde_json::Value::Null);
        assert_eq!(item["turn"], "assignee");

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let claimed = json_body(resp).await;
        assert_eq!(claimed["requester"], "reporter-1");
        assert_eq!(claimed["assignee"], "w1");
        assert_eq!(claimed["turn"], "assignee");

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["turn"], "requester");

        // ADR-0019: approve must come from the item's requester once one is
        // set — an omitted `author` (defaulting to "unknown") would now
        // conflict against "reporter-1".
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/approve"),
                serde_json::json!({"author": "reporter-1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["turn"], serde_json::Value::Null);
    }

    /// `assignee` matches exactly against the item's assignee field;
    /// `topic_scope` (the old `owned_by` behavior) matches by the worker's
    /// registered topics instead — the two must stay independent, see
    /// [ADR-0011](../../../../docs/decisions/ADR-0011-requester-assignee-naming.md).
    #[tokio::test]
    async fn assignee_filter_matches_assignee_not_topic_scope() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/workers",
                serde_json::json!({"id": "w1", "topics": ["iyulab"]}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        // w1 is registered for the item's topic, but hasn't claimed it —
        // `assignee=w1` must not match on topic jurisdiction alone.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?assignee=w1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_body(resp).await.as_array().unwrap().len(), 0);

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?assignee=w1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = json_body(resp).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], id);
    }

    /// `requester` is symmetric to `assignee` (above) — exact match against
    /// the requester field, unaffected by claim/assignment.
    #[tokio::test]
    async fn requester_filter_matches_requester_exactly() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t", "requester": "reporter-a"}),
            ))
            .await
            .unwrap();
        let id_a = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t", "requester": "reporter-b"}),
            ))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?requester=reporter-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = json_body(resp).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], id_a);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?requester=nobody-filed-anything-here")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_body(resp).await.as_array().unwrap().len(), 0);
    }

    /// `mine` ORs `assignee` and (`requester` + `resolved`) — a worker that
    /// currently holds an item matches, and so does a requester whose filed
    /// item is sitting in `resolved` waiting on their approval, but a
    /// requester whose item is still `claimed` does not (nothing to approve
    /// yet). See the "docket-mcp에 '내가 지금 쥐고 있는 것'..." issue.
    /// The reported failure, asserted end to end: a requester's own "what should
    /// I look at" sweep must return its items under either spelling. Before
    /// ADR-0021 `mine="iyulab/Filer"` returned the `Filer` items and never the
    /// `filer` ones, and the requester did not see two of its own resolved items
    /// for six days (docket-works#37).
    #[tokio::test]
    async fn mine_and_the_ownership_filters_fold_identity_case() {
        let app = test_app();
        for spelling in ["iyulab/Filer", "iyulab/filer"] {
            let resp = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    "/items",
                    serde_json::json!({
                        "topic": "iyulab/docket",
                        "title": spelling,
                        "requester": spelling,
                    }),
                ))
                .await
                .unwrap();
            let id = json_body(resp).await["id"].as_str().unwrap().to_string();
            app.clone()
                .oneshot(json_request(
                    "POST",
                    &format!("/items/{id}/claim"),
                    serde_json::json!({"worker_id": "acme/Worker"}),
                ))
                .await
                .unwrap();
            app.clone()
                .oneshot(json_request(
                    "POST",
                    &format!("/items/{id}/submit"),
                    serde_json::json!({"worker_id": "acme/worker"}),
                ))
                .await
                .unwrap();
        }

        // Either spelling of the requester sees both items — the split is gone
        // in both directions, not just the majority one.
        for spelling in ["iyulab/Filer", "iyulab/filer", "IYULAB/FILER"] {
            for filter in ["mine", "requester"] {
                let resp = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(format!("/items?{filter}={spelling}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let items = json_body(resp).await;
                assert_eq!(
                    items.as_array().unwrap().len(),
                    2,
                    "{filter}={spelling} must see both spellings"
                );
            }
        }

        // Same for the assignee side, which drifted independently in the wild.
        for spelling in ["acme/Worker", "acme/worker"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/items?assignee={spelling}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(json_body(resp).await.as_array().unwrap().len(), 2);
        }
    }

    /// `topic_scope`/`mine`'s unclaimed-inbox arm resolves jurisdiction through
    /// the worker's registered topics, so folding has to reach that path too.
    /// No drift has been observed here — this is the identity class being
    /// consistent, per ADR-0021.
    #[tokio::test]
    async fn topic_jurisdiction_folds_case() {
        let app = test_app();
        app.clone()
            .oneshot(json_request(
                "POST",
                "/workers",
                serde_json::json!({"id": "acme/scout", "topics": ["IYULAB"]}),
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();

        for filter in ["topic_scope=acme/Scout", "mine=acme/SCOUT"] {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/items?{filter}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                json_body(resp).await.as_array().unwrap().len(),
                1,
                "{filter}"
            );
        }
    }

    #[tokio::test]
    async fn mine_filter_ors_assignee_and_pending_approval_requester() {
        let app = test_app();

        // w1 is registered for `iyulab` — makes it eligible for the
        // unclaimed-inbox branch (docket-works#35) under that jurisdiction.
        app.clone()
            .oneshot(json_request(
                "POST",
                "/workers",
                serde_json::json!({"id": "w1", "topics": ["iyulab"]}),
            ))
            .await
            .unwrap();

        // Held by w1 (assignee), filed by someone else — matches mine=w1.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "held"}),
            ))
            .await
            .unwrap();
        let held_id = json_body(resp).await["id"].as_str().unwrap().to_string();
        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{held_id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        // Filed by r1, claimed+submitted by w2 — now resolved, waiting on
        // r1's approval — matches mine=r1.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "pending-approval", "requester": "r1"}),
            ))
            .await
            .unwrap();
        let pending_id = json_body(resp).await["id"].as_str().unwrap().to_string();
        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{pending_id}/claim"),
                serde_json::json!({"worker_id": "w2"}),
            ))
            .await
            .unwrap();
        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{pending_id}/submit"),
                serde_json::json!({"worker_id": "w2"}),
            ))
            .await
            .unwrap();

        // Filed by r1, still claimed (not yet submitted) — must NOT match
        // mine=r1 — there's nothing for r1 to act on yet.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "still-in-progress", "requester": "r1"}),
            ))
            .await
            .unwrap();
        let in_progress_id = json_body(resp).await["id"].as_str().unwrap().to_string();
        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{in_progress_id}/claim"),
                serde_json::json!({"worker_id": "w2"}),
            ))
            .await
            .unwrap();

        // Open, unclaimed, under w1's topic jurisdiction (`iyulab`) — the new
        // unclaimed-inbox branch (docket-works#35) — matches mine=w1 even
        // though w1 never touched it.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "unclaimed-in-scope"}),
            ))
            .await
            .unwrap();
        let unclaimed_in_scope_id = json_body(resp).await["id"].as_str().unwrap().to_string();

        // Open, unclaimed, but under a topic w1 has no jurisdiction over —
        // must NOT match mine=w1.
        app.clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "other-org/other-repo", "title": "unclaimed-out-of-scope"}),
            ))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?mine=w1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = json_body(resp).await;
        let listed_ids: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect();
        assert_eq!(listed_ids.len(), 2);
        assert!(listed_ids.contains(&held_id.as_str()));
        assert!(listed_ids.contains(&unclaimed_in_scope_id.as_str()));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?mine=r1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = json_body(resp).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], pending_id);
        let _ = in_progress_id; // asserted absent by the length checks above
    }

    /// The one way to give a pre-existing item a `requester` after the fact —
    /// e.g. backfilling items filed before ADR-0010 added the field.
    #[tokio::test]
    async fn patch_item_sets_requester() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let item = json_body(resp).await;
        let id = item["id"].as_str().unwrap().to_string();
        assert_eq!(item["requester"], serde_json::Value::Null);

        let resp = app
            .clone()
            .oneshot(json_request(
                "PATCH",
                &format!("/items/{id}"),
                serde_json::json!({"requester": "backfilled-reporter"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["requester"], "backfilled-reporter");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_body(resp).await["requester"], "backfilled-reporter");
    }

    #[tokio::test]
    async fn patch_item_rejects_blank_requester() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        let resp = app
            .oneshot(json_request(
                "PATCH",
                &format!("/items/{id}"),
                serde_json::json!({"requester": "   "}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn patch_item_404_for_missing_item() {
        let app = test_app();
        let resp = app
            .oneshot(json_request(
                "PATCH",
                "/items/nonexistent-id",
                serde_json::json!({"requester": "reporter-1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn claim_conflict_is_409_not_500() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        let first = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);

        let second = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w2"}),
            ))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn admin_close_routes_set_resolution_and_reject_a_closed_item() {
        let app = test_app();

        async fn create(app: &Router) -> String {
            let resp = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    "/items",
                    serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
                ))
                .await
                .unwrap();
            json_body(resp).await["id"].as_str().unwrap().to_string()
        }

        async fn close(app: &Router, id: &str, op: &str) -> serde_json::Value {
            let resp = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    &format!("/items/{id}/{op}"),
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            json_body(resp).await
        }

        for (op, resolution) in [
            ("remove", "invalid"),
            ("force-close", "wontfix"),
            ("force-approve", "done"),
        ] {
            let id = create(&app).await;
            let closed = close(&app, &id, op).await;
            assert_eq!(closed["state"], "closed");
            assert_eq!(closed["resolution"], resolution);

            let resp = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    &format!("/items/{id}/{op}"),
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::CONFLICT);
        }
    }

    /// `merge` diverges from `remove`/`force-close`/`force-approve` (the
    /// other three state-unrestricted admin closes, ADR-0012/ADR-0017): it
    /// requires `duplicate_of_id`, and
    /// atomically tags the item `duplicate-of:<id>` — the traceability gap
    /// this closes is "resolution=duplicate alone can't say duplicate of
    /// what".
    #[tokio::test]
    async fn merge_requires_duplicate_of_id_and_tags_the_item() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        // A bare-object request is rejected at the JSON-schema level (axum's
        // `Json` extractor, missing required field) — unlike
        // remove/force-close/force-approve, merge has no bodiless path.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/merge"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // Blank duplicate_of_id is a validation error, same as reject/
        // reopen's blank `reason`.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/merge"),
                serde_json::json!({"duplicate_of_id": "   "}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/merge"),
                serde_json::json!({"duplicate_of_id": "original-item-id", "author": "reviewer"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let merged = json_body(resp).await;
        assert_eq!(merged["state"], "closed");
        assert_eq!(merged["resolution"], "duplicate");
        assert!(
            merged["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "duplicate-of:original-item-id")
        );

        // Already closed — same conflict behavior as the other admin ops.
        let resp = app
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/merge"),
                serde_json::json!({"duplicate_of_id": "original-item-id"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn reject_and_reopen_routes_exist_and_require_json_body() {
        let app = test_app();

        // Create and setup an item: open -> claimed -> resolved -> closed
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/approve"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Test reopen with explicit author and reason on a closed item
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/reopen"),
                serde_json::json!({"author": "requester-1", "reason": "please reconsider"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reopened = json_body(resp).await;
        assert_eq!(reopened["state"], "claimed");

        // Now test reject on a resolved item (after resubmitting)
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Test reject with explicit author and reason
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/reject"),
                serde_json::json!({"author": "reviewer-1", "reason": "needs more work"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let rejected = json_body(resp).await;
        assert_eq!(rejected["state"], "claimed");
        assert_eq!(rejected["resolution"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn reject_with_omitted_author_defaults_to_unknown() {
        let app = test_app();

        // Setup: open -> claimed -> resolved
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        // Reject without providing author in body
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/reject"),
                serde_json::json!({"reason": "no good"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let rejected = json_body(resp).await;
        assert_eq!(rejected["state"], "claimed");

        // Verify the default author "unknown" was recorded in a comment
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{id}/comments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let comments = json_body(resp).await;
        let reject_comment = comments
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"].as_str().unwrap().contains("no good"));
        assert!(reject_comment.is_some());
        assert_eq!(reject_comment.unwrap()["author"], "unknown");
    }

    #[tokio::test]
    async fn reject_with_blank_reason_returns_error() {
        let app = test_app();

        // Setup: open -> claimed -> resolved
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        // Try to reject with blank reason
        let resp = app
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/reject"),
                serde_json::json!({"author": "reviewer", "reason": "   "}),
            ))
            .await
            .unwrap();
        // Should return BAD_REQUEST for validation error (blank reason)
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn reopen_with_omitted_author_defaults_to_unknown() {
        let app = test_app();

        // Setup: open -> claimed -> submitted -> approved (closed)
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/approve"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();

        // Reopen without providing author in body
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/reopen"),
                serde_json::json!({"reason": "found an issue"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reopened = json_body(resp).await;
        assert_eq!(reopened["state"], "claimed");

        // Verify the default author "unknown" was recorded
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{id}/comments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let comments = json_body(resp).await;
        let reopen_comment = comments
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"].as_str().unwrap().contains("found an issue"));
        assert!(reopen_comment.is_some());
        assert_eq!(reopen_comment.unwrap()["author"], "unknown");
    }

    #[tokio::test]
    async fn reopen_with_blank_reason_returns_error() {
        let app = test_app();

        // Setup: open -> claimed -> submitted -> approved (closed)
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/approve"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();

        // Try to reopen with blank reason
        let resp = app
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/reopen"),
                serde_json::json!({"author": "requester", "reason": ""}),
            ))
            .await
            .unwrap();
        // Should return BAD_REQUEST for validation error (blank reason)
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn approve_route_without_json_body_defaults_to_unknown_author() {
        let app = test_app();

        // Create and setup an item: open -> claimed -> resolved
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/claim"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/submit"),
                serde_json::json!({"worker_id": "w1"}),
            ))
            .await
            .unwrap();

        // Send POST with completely empty body and no Content-Type header —
        // exactly what a browser `fetch(url, {method: 'POST'})` sends.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/items/{id}/approve"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // `Option<Json<_>>` treats an absent Content-Type as "no body given",
        // so the approve succeeds and `author` falls back to the same
        // "unknown" default an omitted `author` field would get (ADR-0012:
        // "defaults to `unknown` if omitted, no hard failure").
        assert_eq!(resp.status(), StatusCode::OK);
        let approved = json_body(resp).await;
        assert_eq!(approved["state"], "closed");
        assert_eq!(approved["resolution"], "done");

        // approve_item records its author as a comment — that's where the
        // defaulted author is observable.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{id}/comments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let comments = json_body(resp).await;
        let approve_comment = comments
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["body"] == "approved")
            .expect("approve recorded a comment");
        assert_eq!(approve_comment["author"], "unknown");
    }

    #[tokio::test]
    async fn create_item_accepts_tags_and_search_finds_by_query() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({
                    "topic": "iyulab/node-packages",
                    "title": "form Enter bypasses preventDefault",
                    "tags": ["severity:medium"]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let created = json_body(resp).await;
        assert_eq!(created["tags"], serde_json::json!(["severity:medium"]));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?q=preventDefault")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let found = json_body(resp).await;
        assert_eq!(found.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_item_with_blank_topic_is_400_not_created() {
        let app = test_app();
        let resp = app
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "  ", "title": "t"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Repeated `tag=` keys are the shape `docket-mcp`'s `search_items` sends,
    /// and the shape plain `axum::extract::Query` rejects outright. Seeds a
    /// non-matching item too — a single-item fixture cannot tell a working
    /// filter apart from one that silently returns everything.
    #[tokio::test]
    async fn repeated_tag_query_params_filter_the_list() {
        let app = test_app();
        for (title, tags) in [
            ("tagged-a", serde_json::json!(["a"])),
            ("tagged-b", serde_json::json!(["b"])),
            ("untagged", serde_json::json!([])),
        ] {
            let resp = app
                .clone()
                .oneshot(json_request(
                    "POST",
                    "/items",
                    serde_json::json!({"topic": "iyulab/docket", "title": title, "tags": tags}),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::CREATED);
        }

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?tag=a&tag=b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut titles: Vec<String> = json_body(resp)
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["title"].as_str().unwrap().to_string())
            .collect();
        titles.sort();
        assert_eq!(titles, vec!["tagged-a", "tagged-b"]);

        // A single repeated-key value must narrow further still.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?tag=a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let found = json_body(resp).await;
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["title"], "tagged-a");
    }

    #[tokio::test]
    async fn add_and_remove_tags_routes() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/tags"),
                serde_json::json!({"tags": ["awaiting-release"]}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            json_body(resp).await,
            serde_json::json!(["awaiting-release"])
        );

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/items/{id}/tags"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"tags": ["awaiting-release"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await, serde_json::json!([]));
    }

    /// docket-works#33: `expand_related` defaults to omitting the `related`
    /// key entirely (byte-identical to the pre-existing response shape,
    /// unlike an explicit `null`/`[]`), and only builds it out when asked.
    #[tokio::test]
    async fn get_item_expand_related_defaults_off_and_resolves_both_directions() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "a"}),
            ))
            .await
            .unwrap();
        let a_id = json_body(resp).await["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "b"}),
            ))
            .await
            .unwrap();
        let b_id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{a_id}/tags"),
                serde_json::json!({"tags": [format!("related:{b_id}")]}),
            ))
            .await
            .unwrap();

        // Default (no query param): `related` key absent entirely.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{a_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = json_body(resp).await;
        assert!(!body.as_object().unwrap().contains_key("related"));

        // expand_related=true on the referencing side.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{a_id}?expand_related=true"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = json_body(resp).await;
        let related = body["related"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["id"], b_id);
        assert_eq!(related[0]["relation"], "references");

        // expand_related=true on the referenced-back side.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{b_id}?expand_related=true"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = json_body(resp).await;
        let related = body["related"].as_array().unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0]["id"], a_id);
        assert_eq!(related[0]["relation"], "referenced_by");
    }

    /// The same `expand_related` expansion `get_item` offers for a single
    /// item, applied per row on `list_items` — a batch-expand follow-on to
    /// docket-works#33. Defaults off (byte-identical rows); when requested,
    /// only the returned page is expanded, not the unpaged total — proven
    /// here by tagging `a` (referencing `b`) *before* creating `c`, so the
    /// default (`updated_at` descending) page-2 result is `[c, a]`, excluding
    /// `b` entirely, yet `a`'s `related` still resolves correctly since the
    /// lookup queries the store directly rather than the page's own rows.
    #[tokio::test]
    async fn list_items_expand_related_defaults_off_and_expands_only_the_returned_page() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "a"}),
            ))
            .await
            .unwrap();
        let a_id = json_body(resp).await["id"].as_str().unwrap().to_string();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "b"}),
            ))
            .await
            .unwrap();
        let b_id = json_body(resp).await["id"].as_str().unwrap().to_string();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{a_id}/tags"),
                serde_json::json!({"tags": [format!("related:{b_id}")]}),
            ))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;

        // Created last, after a's tag write — most recently updated.
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "c"}),
            ))
            .await
            .unwrap();
        let c_id = json_body(resp).await["id"].as_str().unwrap().to_string();

        // Default: no `related` key on any row.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let items = json_body(resp).await;
        for item in items.as_array().unwrap() {
            assert!(!item.as_object().unwrap().contains_key("related"));
        }

        // expand_related=true, page 1 of 2 (default order: updated_at desc)
        // — c (created last) then a (tagged last) — b is paged out.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket&limit=2&expand_related=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let items = json_body(resp).await;
        let items = items.as_array().unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec![c_id.as_str(), a_id.as_str()]);
        assert_eq!(items[0]["related"].as_array().unwrap().len(), 0);
        assert_eq!(items[1]["related"].as_array().unwrap().len(), 1);
        assert_eq!(items[1]["related"][0]["id"], b_id);
        assert_eq!(items[1]["related"][0]["relation"], "references");
    }

    #[tokio::test]
    async fn list_tags_route_returns_vocabulary() {
        let app = test_app();
        app.clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t", "tags": ["blocked"]}),
            ))
            .await
            .unwrap();

        let resp = app
            .oneshot(Request::builder().uri("/tags").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let tags = json_body(resp).await;
        assert_eq!(tags[0]["tag"], "blocked");
        assert_eq!(tags[0]["count"], 1);
    }

    #[tokio::test]
    async fn list_topics_route_counts_and_orders_by_frequency() {
        let app = test_app();
        for topic in ["iyulab/docket", "iyulab/docket", "iyulab/router"] {
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/items",
                    serde_json::json!({"topic": topic, "title": "t"}),
                ))
                .await
                .unwrap();
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/topics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let topics = json_body(resp).await;
        assert_eq!(topics[0]["topic"], "iyulab/docket");
        assert_eq!(topics[0]["count"], 2);
        assert_eq!(topics[1]["topic"], "iyulab/router");
        assert_eq!(topics[1]["count"], 1);
    }

    /// ADR-0014: a caller that asks for nothing gets `DEFAULT_LIST_LIMIT`
    /// rows, not an unbounded response — and can still discover the true
    /// total via the `X-Total-Count` header.
    #[tokio::test]
    async fn list_items_defaults_to_a_bounded_page_with_a_total_count_header() {
        let app = test_app();
        for i in 0..(DEFAULT_LIST_LIMIT + 5) {
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/items",
                    serde_json::json!({"topic": "iyulab/docket", "title": format!("item-{i}")}),
                ))
                .await
                .unwrap();
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let total_header = resp
            .headers()
            .get("X-Total-Count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        assert_eq!(total_header, Some(DEFAULT_LIST_LIMIT + 5));
        let items = json_body(resp).await;
        assert_eq!(items.as_array().unwrap().len(), DEFAULT_LIST_LIMIT);
    }

    /// `limit`/`offset` apply after every other filter (topic/state/tag/
    /// assignee/requester/topic_scope) — this exercises that combination,
    /// not just the bare unfiltered case above.
    #[tokio::test]
    async fn list_items_limit_and_offset_page_through_a_filtered_result() {
        let app = test_app();
        for i in 0..5 {
            app.clone()
                .oneshot(json_request(
                    "POST",
                    "/items",
                    serde_json::json!({"topic": "iyulab/docket", "title": format!("item-{i}")}),
                ))
                .await
                .unwrap();
        }
        // A different topic must not count toward the paged total.
        app.clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/router", "title": "elsewhere"}),
            ))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket&limit=2&offset=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let total_header = resp
            .headers()
            .get("X-Total-Count")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        assert_eq!(total_header, Some(5));
        let items = json_body(resp).await;
        assert_eq!(items.as_array().unwrap().len(), 2);

        // An explicit limit past the cap is clamped, not honored verbatim.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/items?topic=iyulab/docket&limit={}",
                        MAX_LIST_LIMIT + 50
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let items = json_body(resp).await;
        assert_eq!(items.as_array().unwrap().len(), 5);
    }

    /// See ADR-0020: `order=asc` reverses the default `updated_at`-descending
    /// ordering; an unrecognized value falls back to the default rather than
    /// erroring (same treatment `tag_match` already gets).
    #[tokio::test]
    async fn order_query_param_controls_ascending_vs_descending_and_defaults_to_desc() {
        let app = test_app();
        let first = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "first"}),
            ))
            .await
            .unwrap();
        let first_id = json_body(first).await["id"].as_str().unwrap().to_string();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "second"}),
            ))
            .await
            .unwrap();
        let second_id = json_body(second).await["id"].as_str().unwrap().to_string();

        let desc = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let desc_items = json_body(desc).await;
        assert_eq!(desc_items[0]["id"], second_id);
        assert_eq!(desc_items[1]["id"], first_id);

        let asc = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket&order=asc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let asc_items = json_body(asc).await;
        assert_eq!(asc_items[0]["id"], first_id);
        assert_eq!(asc_items[1]["id"], second_id);

        // An unrecognized `order` value degrades to the default, not a 400.
        let unrecognized = app
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket&order=sideways")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unrecognized.status(), StatusCode::OK);
        let unrecognized_items = json_body(unrecognized).await;
        assert_eq!(unrecognized_items[0]["id"], second_id);
        assert_eq!(unrecognized_items[1]["id"], first_id);
    }

    #[tokio::test]
    async fn summary_true_nulls_body_without_affecting_other_fields() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t", "body": "long body"}),
            ))
            .await
            .unwrap();
        assert_eq!(json_body(resp).await["body"], "long body");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket&summary=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let items = json_body(resp).await;
        let item = &items.as_array().unwrap()[0];
        assert_eq!(item["body"], serde_json::Value::Null);
        assert_eq!(item["title"], "t");

        // Unaffected without the flag.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?topic=iyulab/docket")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let items = json_body(resp).await;
        assert_eq!(items.as_array().unwrap()[0]["body"], "long body");
    }

    /// A read filter never errors on a non-matching or unregistered
    /// reference — it answers with an empty result, same as `topic`/
    /// `assignee`/`requester` (see docs/usage.md §4). Only mutate calls
    /// (`claim_item`, `add_comment`, …) 404 on a missing reference.
    #[tokio::test]
    async fn list_topic_scope_unknown_worker_is_empty() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?topic_scope=nobody")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await, serde_json::json!([]));
    }

    /// `GET /workers/{id}` is the one way to positively confirm registration
    /// — unlike `list_items(topic_scope=)`, this fetches one specific known
    /// resource by id, so it 404s when that resource doesn't exist (same
    /// category as `GET /items/{id}`), not a filter that answers "no match"
    /// with an empty result.
    #[tokio::test]
    async fn get_worker_route_200_when_registered_404_when_not() {
        let app = test_app();
        app.clone()
            .oneshot(json_request(
                "POST",
                "/workers",
                serde_json::json!({"id": "w1", "topics": ["iyulab"]}),
            ))
            .await
            .unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/workers/w1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let worker = json_body(resp).await;
        assert_eq!(worker["id"], "w1");
        assert_eq!(worker["topics"], serde_json::json!(["iyulab"]));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/workers/ghost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn api_alias_matches_bare_route() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let items = json_body(resp).await;
        assert_eq!(items.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn unmatched_api_path_is_json_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let body = json_body(resp).await;
        assert_eq!(body["error"], "not found");
    }

    fn temp_console_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "docket-core-console-test-{label}-{}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn console_index_served_at_root() {
        let dir = temp_console_dir("index-at-root");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<html>docket-console-test-marker</html>",
        )
        .unwrap();

        let app = build_router(
            Arc::new(Store::open(":memory:").expect("in-memory store opens")),
            &dir,
        );
        let resp = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("docket-console-test-marker"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A browser navigating a client-side route announces `text/html`, and only
    /// that gets the SPA shell — a caller that didn't ask for HTML gets the same
    /// JSON 404 the API returns. See `spa_fallback` / docket-works#36.
    #[tokio::test]
    async fn spa_fallback_serves_index_only_when_html_is_requested() {
        let dir = temp_console_dir("spa-fallback");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<html>docket-console-test-marker</html>",
        )
        .unwrap();

        let app = build_router(
            Arc::new(Store::open(":memory:").expect("in-memory store opens")),
            &dir,
        );
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/some/client/side/route")
                    .header("accept", "text/html,application/xhtml+xml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("docket-console-test-marker"));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/some/client/side/route")
                    .header("accept", "*/*")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(resp).await["error"], "not found");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn static_asset_is_served_as_itself_not_index_fallback() {
        let dir = temp_console_dir("real-asset");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<html>docket-console-test-marker</html>",
        )
        .unwrap();
        std::fs::write(
            dir.join("assets").join("app.js"),
            "console.log('docket-console-asset-marker');",
        )
        .unwrap();

        let app = build_router(
            Arc::new(Store::open(":memory:").expect("in-memory store opens")),
            &dir,
        );
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/assets/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "console.log('docket-console-asset-marker');"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn root_is_404_when_console_dir_is_missing() {
        let resp = test_app()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// A misspelled API path at the root used to answer `200 text/html` while
    /// the same path under `/api` answered a JSON 404 — the asymmetry behind
    /// docket-works#36, since every client calls the root-merged routes. The
    /// two now agree for any caller that isn't a browser.
    #[tokio::test]
    async fn bare_unmatched_path_matches_api_404_for_non_browser_callers() {
        let dir = temp_console_dir("bare-vs-api-asymmetry");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<html>docket-console-test-marker</html>",
        )
        .unwrap();

        let app = build_router(
            Arc::new(Store::open(":memory:").expect("in-memory store opens")),
            &dir,
        );
        for uri in ["/itemz", "/api/itemz"] {
            let resp = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(json_body(resp).await["error"], "not found", "{uri}");
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Regression for docket-works#36. A worker id is conventionally `org/repo`,
    /// so an unencoded id makes `/workers/{id}` a *two*-segment path that misses
    /// the route entirely and lands on the static fallback. Before the fix that
    /// answered `200 text/html`, which every JSON client read as a broken
    /// transport rather than a wrong path — and it did so whether or not the
    /// worker existed, so `get_worker` never worked for a real id at all.
    ///
    /// Note the fixture id: a slash-free id (`w1`, as the older tests use) can
    /// never reproduce this, which is why it shipped green.
    #[tokio::test]
    async fn worker_id_with_slash_needs_encoding_and_never_returns_html() {
        let dir = temp_console_dir("worker-id-slash");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<html>docket-console-test-marker</html>",
        )
        .unwrap();

        let app = build_router(
            Arc::new(Store::open(":memory:").expect("in-memory store opens")),
            &dir,
        );
        app.clone()
            .oneshot(json_request(
                "POST",
                "/workers",
                serde_json::json!({"id": "acme/widget", "topics": ["acme"]}),
            ))
            .await
            .unwrap();

        // Encoded: one segment, matches the route, resolves the registration.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/workers/acme%2Fwidget")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(resp).await["id"], "acme/widget");

        // Unencoded: still unroutable (the route is single-segment by design),
        // but it must fail as a JSON 404 — never as a 200 HTML shell.
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/workers/acme/widget")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(resp).await["error"], "not found");

        // Same for an id that was never registered — the caller can tell
        // "no such worker" from "the server is broken" in both cases now.
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/workers/acme%2Fghost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn add_and_list_comments_routes() {
        let app = test_app();
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "t"}),
            ))
            .await
            .unwrap();
        let id = json_body(resp).await["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/comments"),
                serde_json::json!({"author": "maintainer", "body": "looking into it"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(json_body(resp).await["author"], "maintainer");

        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{id}/comments"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let comments = json_body(resp).await;
        assert_eq!(comments.as_array().unwrap().len(), 1);
        assert_eq!(comments[0]["body"], "looking into it");
    }

    #[tokio::test]
    async fn status_reports_version_and_idle_seconds() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert_eq!(body["version"], format!("v{}", env!("CARGO_PKG_VERSION")));
        assert!(body["idle_seconds"].as_u64().is_some());
    }

    #[tokio::test]
    async fn status_itself_does_not_reset_the_idle_clock() {
        let app = test_app();

        // A non-/status request touches the idle clock...
        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        // ...but polling /status repeatedly must not reset it back to ~0.
        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let first_idle = json_body(first).await["idle_seconds"].as_u64().unwrap();

        let second = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let second_idle = json_body(second).await["idle_seconds"].as_u64().unwrap();

        assert!(
            first_idle >= 1,
            "expected idle_seconds >= 1, got {first_idle}"
        );
        assert!(
            second_idle >= first_idle,
            "polling /status must not reset the idle clock: first={first_idle}, second={second_idle}"
        );
    }

    #[tokio::test]
    async fn a_bare_request_resets_idle_seconds() {
        let app = test_app();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let before = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let idle_before = json_body(before).await["idle_seconds"].as_u64().unwrap();
        assert!(idle_before >= 1);

        app.clone()
            .oneshot(
                Request::builder()
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let after = app
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let idle_after = json_body(after).await["idle_seconds"].as_u64().unwrap();
        assert!(
            idle_after < idle_before,
            "a request other than /status must reset the idle clock: before={idle_before}, after={idle_after}"
        );
    }

    /// DELETE /items/{id} deletes an item and returns 204 No Content.
    /// GET /items/{id} after deletion returns 404.
    #[tokio::test]
    async fn delete_item_removes_and_returns_204() {
        let app = test_app();

        // Create an item
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "to-delete"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let item = json_body(resp).await;
        let id = item["id"].as_str().unwrap().to_string();

        // Delete it
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/items/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Verify it's gone with 404
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/items/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// POST /items/{id}/archive archives an item and returns the updated item
    /// with archived_at set to a non-null timestamp.
    #[tokio::test]
    async fn archive_item_sets_archived_at() {
        let app = test_app();

        // Create an item
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "to-archive"}),
            ))
            .await
            .unwrap();
        let item = json_body(resp).await;
        let id = item["id"].as_str().unwrap().to_string();
        assert_eq!(item["archived_at"], serde_json::Value::Null);

        // Archive it
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/archive"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let archived = json_body(resp).await;
        assert!(
            archived["archived_at"].is_u64(),
            "archived_at must be a timestamp (u64)"
        );
    }

    /// POST /items/{id}/archive is idempotent — archiving twice doesn't error.
    #[tokio::test]
    async fn archive_item_is_idempotent() {
        let app = test_app();

        // Create an item
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "idempotent-archive"}),
            ))
            .await
            .unwrap();
        let item = json_body(resp).await;
        let id = item["id"].as_str().unwrap().to_string();

        // Archive it once
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/archive"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let first_archived = json_body(resp).await;
        let first_timestamp = first_archived["archived_at"].as_u64().unwrap();

        // Archive it again
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{id}/archive"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let second_archived = json_body(resp).await;
        // The timestamp should remain the same (idempotent)
        assert_eq!(
            second_archived["archived_at"].as_u64().unwrap(),
            first_timestamp
        );
    }

    /// GET /items with no `archived` param excludes archived items by default.
    /// GET /items?archived=true returns only archived items.
    /// GET /items?archived=false returns only non-archived items (same as default).
    #[tokio::test]
    async fn archived_query_param_filters_correctly() {
        let app = test_app();

        // Create a normal item
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "normal"}),
            ))
            .await
            .unwrap();
        let normal_id = json_body(resp).await["id"].as_str().unwrap().to_string();

        // Create and archive another item
        let resp = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/items",
                serde_json::json!({"topic": "iyulab/docket", "title": "archived-one"}),
            ))
            .await
            .unwrap();
        let archive_id = json_body(resp).await["id"].as_str().unwrap().to_string();

        app.clone()
            .oneshot(json_request(
                "POST",
                &format!("/items/{archive_id}/archive"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();

        // GET /items (no param) — should return only the normal item
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let listed = json_body(resp).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], normal_id);

        // GET /items?archived=true — should return only the archived item
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?archived=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let archived_list = json_body(resp).await;
        assert_eq!(archived_list.as_array().unwrap().len(), 1);
        assert_eq!(archived_list[0]["id"], archive_id);

        // GET /items?archived=false — should return only the normal item (same as default)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items?archived=false")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let non_archived = json_body(resp).await;
        assert_eq!(non_archived.as_array().unwrap().len(), 1);
        assert_eq!(non_archived[0]["id"], normal_id);
    }
}
