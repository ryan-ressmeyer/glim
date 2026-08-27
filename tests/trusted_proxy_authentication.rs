use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use glim::{daemon::AccessToken, storage::Store};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "https://glim.example";
const TRUSTED: &str = "127.0.0.1:43100";
const UNTRUSTED: &str = "127.0.0.2:43100";

fn trusted_proxy_app(root: &TempDir) -> axum::Router {
    glim::app_with_store_and_trusted_proxy(
        Store::open(root.path()).unwrap(),
        ["127.0.0.1".parse().unwrap()].into_iter().collect(),
        ORIGIN.to_owned(),
    )
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    peer: &str,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    app.oneshot(request).await.unwrap()
}

#[tokio::test]
async fn trusted_proxy_uses_only_the_immediate_peer_and_keeps_health_public() {
    let root = TempDir::new().unwrap();
    let app = trusted_proxy_app(&root);

    assert_eq!(
        request(app.clone(), "GET", "/api/v1/health", UNTRUSTED, &[])
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(app.clone(), "GET", "/api/v1/status", TRUSTED, &[])
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(app.clone(), "GET", "/api/v1/status", UNTRUSTED, &[])
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(app.clone(), "GET", "/api/v1/posts", TRUSTED, &[])
            .await
            .status(),
        StatusCode::OK
    );

    let spoofed = request(
        app.clone(),
        "GET",
        "/api/v1/posts",
        UNTRUSTED,
        &[
            ("forwarded", "for=127.0.0.1;proto=https;host=glim.example"),
            ("x-forwarded-for", "127.0.0.1"),
            ("x-real-ip", "127.0.0.1"),
            ("x-forwarded-host", "glim.example"),
            ("x-forwarded-proto", "https"),
        ],
    )
    .await;
    assert_eq!(spoofed.status(), StatusCode::FORBIDDEN);
    let payload = spoofed.into_body().collect().await.unwrap().to_bytes();
    let payload: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(payload["error"]["code"], "proxy_authorization_required");
    assert_eq!(payload["error"]["details"], serde_json::json!({}));

    let page = request(app.clone(), "GET", "/feed", UNTRUSTED, &[]).await;
    assert_eq!(page.status(), StatusCode::FORBIDDEN);
    assert!(page.headers().get(header::LOCATION).is_none());

    assert_eq!(
        request(app.clone(), "GET", "/login", TRUSTED, &[])
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(app, "POST", "/api/v1/auth/session", TRUSTED, &[])
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn trusted_proxy_mutations_require_the_exact_public_origin() {
    let root = TempDir::new().unwrap();
    let app = trusted_proxy_app(&root);

    for (headers, expected) in [
        (vec![], StatusCode::NOT_FOUND),
        (
            vec![("sec-fetch-site", "same-origin")],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![("origin", "https://attacker.invalid")],
            StatusCode::FORBIDDEN,
        ),
        (vec![("origin", ORIGIN)], StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            request(
                app.clone(),
                "POST",
                "/api/v1/sessions/abc123/heartbeat",
                TRUSTED,
                &headers,
            )
            .await
            .status(),
            expected
        );
    }

    let mut malformed_origin = Request::builder()
        .method("POST")
        .uri("/api/v1/sessions/abc123/heartbeat")
        .body(Body::empty())
        .unwrap();
    malformed_origin.headers_mut().insert(
        header::ORIGIN,
        header::HeaderValue::from_bytes(&[0xff]).unwrap(),
    );
    malformed_origin
        .extensions_mut()
        .insert(ConnectInfo(TRUSTED.parse::<SocketAddr>().unwrap()));
    assert_eq!(
        app.clone()
            .oneshot(malformed_origin)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    assert_ne!(
        request(
            app,
            "OPTIONS",
            "/api/v1/sessions/abc123/heartbeat",
            TRUSTED,
            &[],
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn local_and_token_modes_ignore_proxy_identity_headers() {
    let root = TempDir::new().unwrap();
    let forwarded = [("x-forwarded-for", "127.0.0.1"), ("x-real-ip", "127.0.0.1")];

    assert_eq!(
        request(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            "GET",
            "/api/v1/posts",
            UNTRUSTED,
            &forwarded,
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(
            glim::app_with_store_and_token_auth(
                Store::open(root.path()).unwrap(),
                AccessToken::parse(&"a".repeat(64)).unwrap(),
                ORIGIN.to_owned(),
                true,
            ),
            "GET",
            "/api/v1/posts",
            TRUSTED,
            &forwarded,
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}
