pub mod storage;

use axum::{
    Json, Router,
    http::{HeaderValue, header},
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

pub fn app() -> Router {
    Router::new()
        .route("/", get(root))
        .route("/assets/app.js", get(frontend_script))
        .route("/api/v1/health", get(health))
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
