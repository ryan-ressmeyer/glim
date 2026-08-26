pub mod api;
pub mod cli;
pub mod daemon;
pub mod storage;

pub const API_V1_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/health"),
    ("POST", "/api/v1/auth/session"),
    ("DELETE", "/api/v1/auth/session"),
    ("POST", "/api/v1/sessions"),
    ("GET", "/api/v1/sessions/{public_id}"),
    ("DELETE", "/api/v1/sessions/{public_id}"),
    ("GET", "/api/v1/sessions/{public_id}/posts"),
    ("GET", "/api/v1/sessions/{public_id}/posts/events"),
    ("POST", "/api/v1/sessions/{public_id}/heartbeat"),
    ("GET", "/api/v1/projects/{project_id}/posts"),
    ("GET", "/api/v1/projects/{project_id}/posts/events"),
    ("GET", "/api/v1/posts/events"),
    ("GET", "/api/v1/posts"),
    ("POST", "/api/v1/posts"),
    ("GET", "/api/v1/posts/{post_id}"),
    (
        "POST",
        "/api/v1/posts/{post_id}/files/{position}/html-capability",
    ),
    ("GET", "/api/v1/posts/{post_id}/files/{position}/content"),
    ("HEAD", "/api/v1/posts/{post_id}/files/{position}/content"),
    (
        "GET",
        "/api/v1/posts/{post_id}/files/{position}/support/{asset_path}",
    ),
    (
        "HEAD",
        "/api/v1/posts/{post_id}/files/{position}/support/{asset_path}",
    ),
];

use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

const INDEX_HTML: &str = include_str!("../web/dist/index.html");
const APP_JS: &[u8] = include_bytes!("../web/dist/assets/app.js");
const PDF_WORKER_JS: &[u8] = include_bytes!("../web/dist/assets/pdf.worker.mjs");

#[derive(Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
}

/// Builds the compatibility application without a configured store. Stateful v1
/// routes remain present and return `storage_unavailable`.
pub fn app() -> Router {
    app_with_state(api::ApiState::default())
}

/// Builds the daemon application with one synchronous SQLite store shared across
/// requests. Storage work runs on Tokio's blocking pool rather than runtime workers.
pub fn app_with_store(store: storage::Store) -> Router {
    app_with_state(api::ApiState::with_store(store))
}

pub fn app_with_store_and_token_auth(
    store: storage::Store,
    access_token: daemon::AccessToken,
    expected_origin: String,
    secure_cookie: bool,
) -> Router {
    app_with_state(api::ApiState::with_store_and_token_auth(
        store,
        access_token,
        expected_origin,
        secure_cookie,
    ))
}

fn app_with_state(state: api::ApiState) -> Router {
    let v1 = Router::new()
        .route("/health", get(health))
        .merge(api::routes())
        .fallback(api::route_not_found)
        .method_not_allowed_fallback(api::method_not_allowed);
    let authentication_state = state.clone();
    Router::new()
        .route("/", get(root))
        .route("/feed", get(root))
        .route("/login", get(login_page))
        .route("/sessions/{public_id}", get(session_page))
        .route("/projects/{project_id}", get(project_page))
        .route("/assets/app.js", get(frontend_script))
        .route("/assets/pdf.worker.mjs", get(pdf_worker_script))
        .merge(api::capability_routes())
        .nest("/api/v1", v1)
        .fallback(api::root_not_found)
        .layer(middleware::from_fn(api::validate_v1_path))
        .layer(middleware::from_fn_with_state(
            authentication_state,
            api::authenticate_request,
        ))
        .with_state(state)
}

async fn login_page() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn root() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn session_page(Path(public_id): Path<String>) -> Response {
    const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if public_id.len() < 6
        || !public_id
            .chars()
            .all(|character| BASE58.contains(character))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    Html(INDEX_HTML).into_response()
}

async fn project_page(Path(project_id): Path<String>) -> Response {
    // Browser page routes accept only IDs exactly representable by JavaScript.
    const MAX_BROWSER_SAFE_ID: i64 = 9_007_199_254_740_991;
    if project_id
        .parse::<i64>()
        .ok()
        .is_none_or(|value| value <= 0 || value > MAX_BROWSER_SAFE_ID)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    Html(INDEX_HTML).into_response()
}

async fn frontend_script() -> impl IntoResponse {
    script_asset(APP_JS)
}

async fn pdf_worker_script() -> impl IntoResponse {
    script_asset(PDF_WORKER_JS)
}

fn script_asset(bytes: &'static [u8]) -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        bytes,
    )
}

async fn health() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}
