use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

#[tokio::test]
async fn health_reports_ok_and_package_version() {
    let response = glim::app()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload,
        json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
        })
    );
}

#[tokio::test]
async fn root_serves_the_embedded_frontend() {
    let response = glim::app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/html; charset=utf-8"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("Glimse"));
    assert!(html.contains("/assets/app.js"));
}

#[tokio::test]
async fn feed_page_routes_serve_the_embedded_frontend() {
    for uri in [
        "/feed",
        "/login",
        "/sessions/2zY8Ab",
        "/projects/42",
        "/projects/9007199254740991",
    ] {
        let response = glim::app()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8",
            "{uri}"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("Glimse"));
    }
}

#[tokio::test]
async fn malformed_feed_page_shapes_are_not_spa_fallbacks() {
    for uri in [
        "/sessions",
        "/sessions/abc",
        "/sessions/0OIlxx",
        "/sessions/abc-def",
        "/sessions/abc/extra",
        "/projects",
        "/projects/0",
        "/projects/-1",
        "/projects/9007199254740992",
        "/projects/9223372036854775807",
        "/projects/9223372036854775808",
        "/projects/1/extra",
        "/global",
    ] {
        let response = glim::app()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn compiled_pdf_worker_is_embedded() {
    let response = glim::app()
        .oneshot(
            Request::builder()
                .uri("/assets/pdf.worker.mjs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );
    assert!(
        !response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
}

#[tokio::test]
async fn compiled_script_assets_disable_content_sniffing() {
    for uri in ["/assets/app.js", "/assets/pdf.worker.mjs"] {
        let response = glim::app()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff",
            "{uri}"
        );
    }
}

#[tokio::test]
async fn compiled_frontend_script_is_embedded() {
    let response = glim::app()
        .oneshot(
            Request::builder()
                .uri("/assets/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!body.is_empty());
}
