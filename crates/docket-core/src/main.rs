use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
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
    let app = build_router(Arc::new(store));

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

fn build_router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/workers", post(register_worker))
        .route("/items", post(create_item).get(list_items))
        .route("/items/{id}", get(get_item))
        .route("/items/{id}/claim", post(claim_item))
        .route("/items/{id}/submit", post(submit_item))
        .route("/items/{id}/approve", post(approve_item))
        .with_state(store)
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
}

async fn create_item(
    State(store): State<Arc<Store>>,
    Json(req): Json<CreateItemRequest>,
) -> Result<(StatusCode, Json<Item>), ApiError> {
    let item = store.create_item(&req.topic, &req.title, req.body.as_deref())?;
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
}

async fn list_items(
    State(store): State<Arc<Store>>,
    Query(q): Query<ListItemsQuery>,
) -> Result<Json<Vec<Item>>, ApiError> {
    let state = q.state.as_deref().and_then(ItemState::parse);
    let items = store.list_items(q.topic.as_deref(), state)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_app() -> Router {
        build_router(Arc::new(
            Store::open(":memory:").expect("in-memory store opens"),
        ))
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
}
