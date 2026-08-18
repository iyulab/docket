use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use axum::Extension;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
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
        .route("/items", post(create_item).get(list_items))
        .route("/items/{id}", get(get_item))
        .route("/items/{id}/claim", post(claim_item))
        .route("/items/{id}/submit", post(submit_item))
        .route("/items/{id}/approve", post(approve_item))
        .route("/items/{id}/remove", post(remove_item))
        .route("/items/{id}/merge", post(merge_item))
        .route("/items/{id}/force-close", post(force_close_item))
        .route(
            "/items/{id}/tags",
            post(add_item_tags).delete(remove_item_tags),
        )
        .route("/items/{id}/comments", post(add_comment).get(list_comments))
        .route("/tags", get(list_tags))
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
    let index_file = tower_http::services::ServeFile::new(console_dir.join("index.html"));
    let static_service = tower_http::services::ServeDir::new(console_dir).fallback(index_file);
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

#[derive(Deserialize)]
struct CreateItemRequest {
    topic: String,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

async fn create_item(
    State(store): State<Arc<Store>>,
    Json(req): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<Item>), ApiError> {
    let item = store.create_item(&req.topic, &req.title, req.body.as_deref(), &req.tags)?;
    Ok((StatusCode::CREATED, Json(item)))
}

#[derive(Deserialize)]
struct ListItemsQuery {
    /// Exact-match topic filter.
    topic: Option<String>,
    state: Option<String>,
    /// A registered worker's id — narrows the list to items under any topic
    /// that worker owns (prefix match, see [`docket_core::domain::topic_matches`]).
    /// This is the "discover it via list" step of the M1 completion criteria.
    owned_by: Option<String>,
    /// Full-text match against title+body. Presence of `q` and/or `tag`
    /// routes this request through `Store::search_items` instead of
    /// `Store::list_items` — see the branch below.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    tag: Vec<String>,
    #[serde(default)]
    tag_match: Option<String>,
}

/// Uses `axum_extra`'s `Query` rather than `axum::extract::Query`: only the
/// former decodes repeated keys (`?tag=a&tag=b`) into a `Vec`, which the
/// plain extractor rejects with a 400.
async fn list_items(
    State(store): State<Arc<Store>>,
    ExtraQuery(q): ExtraQuery<ListItemsQuery>,
) -> Result<Json<Vec<Item>>, ApiError> {
    let state = q.state.as_deref().and_then(ItemState::parse);
    let items = if q.q.is_some() || !q.tag.is_empty() {
        let tag_match = q
            .tag_match
            .as_deref()
            .and_then(docket_core::domain::TagMatch::parse)
            .unwrap_or(docket_core::domain::TagMatch::Any);
        store.search_items(q.topic.as_deref(), state, &q.tag, tag_match, q.q.as_deref())?
    } else {
        store.list_items(q.topic.as_deref(), state)?
    };
    let items = match q.owned_by {
        Some(worker_id) => {
            let worker = store.get_worker(&worker_id)?;
            items
                .into_iter()
                .filter(|item| {
                    worker
                        .topics
                        .iter()
                        .any(|owned| docket_core::domain::topic_matches(owned, &item.topic))
                })
                .collect()
        }
        None => items,
    };
    Ok(Json(items))
}

async fn get_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.get_item(&id)?))
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

async fn submit_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
    Json(req): Json<WorkerScopedRequest>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.submit_item(&id, &req.worker_id)?))
}

async fn approve_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.approve_item(&id)?))
}

async fn remove_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.remove_item(&id)?))
}

async fn merge_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.merge_item(&id)?))
}

async fn force_close_item(
    State(store): State<Arc<Store>>,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    Ok(Json(store.force_close_item(&id)?))
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
                    .uri("/items?owned_by=w1&state=open")
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
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/items/{id}/approve"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let closed = json_body(resp).await;
        assert_eq!(closed["state"], "closed");
        assert_eq!(closed["resolution"], "done");
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
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/items/{id}/{op}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            json_body(resp).await
        }

        for (op, resolution) in [
            ("remove", "invalid"),
            ("merge", "duplicate"),
            ("force-close", "wontfix"),
        ] {
            let id = create(&app).await;
            let closed = close(&app, &id, op).await;
            assert_eq!(closed["state"], "closed");
            assert_eq!(closed["resolution"], resolution);

            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/items/{id}/{op}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::CONFLICT);
        }
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
    async fn list_owned_by_unknown_worker_is_404() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/items?owned_by=nobody")
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

    #[tokio::test]
    async fn spa_fallback_serves_index_for_unknown_client_route() {
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
            .oneshot(
                Request::builder()
                    .uri("/some/client/side/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("docket-console-test-marker"));

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

    #[tokio::test]
    async fn bare_unmatched_path_falls_back_to_html_unlike_api() {
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
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/itemz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("docket-console-test-marker"));

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
}
