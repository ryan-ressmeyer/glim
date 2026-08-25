pub mod api;
pub mod storage;

pub const PHASE_2A_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/health"),
    ("POST", "/api/v1/sessions"),
    ("GET", "/api/v1/sessions/{public_id}"),
    ("DELETE", "/api/v1/sessions/{public_id}"),
    ("GET", "/api/v1/sessions/{public_id}/posts"),
    ("POST", "/api/v1/sessions/{public_id}/heartbeat"),
    ("GET", "/api/v1/projects/{project_id}/posts"),
    ("GET", "/api/v1/posts"),
    ("GET", "/api/v1/posts/{post_id}"),
];

use axum::{
    Json, Router,
    http::{HeaderValue, header},
    middleware,
    response::{Html, IntoResponse},
    routing::get,
};
use serde::Serialize;

const INDEX_HTML: &str = include_str!("../web/dist/index.html");
const APP_JS: &[u8] = include_bytes!("../web/dist/assets/app.js");

#[derive(Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
}

/// Builds the compatibility application. Stateful v1 routes remain present and
/// return `storage_unavailable`; persistent daemon root/config wiring is deferred.
pub fn app() -> Router {
    app_with_state(api::ApiState::default())
}

/// Builds the daemon application with one synchronous SQLite store shared across
/// requests. Storage work runs on Tokio's blocking pool rather than runtime workers.
pub fn app_with_store(store: storage::Store) -> Router {
    app_with_state(api::ApiState::with_store(store))
}

fn app_with_state(state: api::ApiState) -> Router {
    let v1 = Router::new()
        .route("/health", get(health))
        .merge(api::routes())
        .fallback(api::route_not_found)
        .method_not_allowed_fallback(api::method_not_allowed);
    Router::new()
        .route("/", get(root))
        .route("/assets/app.js", get(frontend_script))
        .nest("/api/v1", v1)
        .fallback(api::root_not_found)
        .layer(middleware::from_fn(api::validate_v1_path))
        .with_state(state)
}

async fn root() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn frontend_script() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        APP_JS,
    )
}

async fn health() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}
