use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use glim::{
    daemon::AccessToken,
    storage::{PublicationFile, PublicationRequest, PublicationSupportAsset, Store},
};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "https://glim.example:3443";

fn authenticated_app(root: &TempDir) -> axum::Router {
    glim::app_with_store_and_token_auth(
        Store::open(root.path()).unwrap(),
        AccessToken::parse(&"a".repeat(64)).unwrap(),
        ORIGIN.to_owned(),
        true,
    )
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn token_mode_protects_pages_api_sse_ranges_and_unknown_routes() {
    let root = TempDir::new().unwrap();
    let app = authenticated_app(&root);

    for uri in ["/api/v1/health", "/login", "/assets/app.js"] {
        assert_eq!(
            request(app.clone(), "GET", uri, &[], Body::empty())
                .await
                .status(),
            StatusCode::OK,
            "{uri}"
        );
    }
    let page = request(app.clone(), "GET", "/feed", &[], Body::empty()).await;
    assert_eq!(page.status(), StatusCode::SEE_OTHER);
    assert_eq!(page.headers()[header::LOCATION], "/login");

    for uri in [
        "/api/v1/posts",
        "/api/v1/posts/events",
        "/api/v1/posts/1/files/0/content",
        "/api/v1/posts/1/files/0/support/app.js",
        "/api/v1/unknown",
    ] {
        let response = request(app.clone(), "GET", uri, &[], Body::empty()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        let payload = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<Value>(&payload).unwrap()["error"]["code"],
            "authentication_required"
        );
    }

    let bearer = format!("Bearer {}", "a".repeat(64));
    assert_eq!(
        request(
            app.clone(),
            "GET",
            "/api/v1/posts",
            &[("authorization", &bearer)],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(
            app.clone(),
            "GET",
            "/api/v1/posts/events",
            &[("authorization", &bearer)],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(
            app,
            "GET",
            "/api/v1/posts/1/files/0/content",
            &[("authorization", &bearer), ("range", "bytes=0-0")],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn html_capability_is_scoped_to_one_support_subtree() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("test", "auth-html", "Auth", "/tmp/auth")
        .unwrap();
    let html = store
        .stage_publication_blob(std::io::Cursor::new(b"<script src=\"app.js\"></script>"))
        .unwrap();
    let script = store
        .stage_publication_blob(std::io::Cursor::new(b"window.capabilityLoaded = true"))
        .unwrap();
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "HTML".into(),
                commentary: "Capability".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "entry.html".into(),
                    caption: None,
                    blob: html,
                    support_assets: vec![PublicationSupportAsset {
                        relative_path: "app.js".into(),
                        blob: script,
                    }],
                }],
            },
            1,
        )
        .unwrap();
    let app = glim::app_with_store_and_token_auth(
        store,
        AccessToken::parse(&"a".repeat(64)).unwrap(),
        ORIGIN.to_owned(),
        true,
    );
    let bearer = format!("Bearer {}", "a".repeat(64));
    let issued = request(
        app.clone(),
        "POST",
        "/api/v1/posts/1/files/0/html-capability",
        &[("authorization", &bearer)],
        Body::empty(),
    )
    .await;
    assert_eq!(issued.status(), StatusCode::OK);
    let payload = issued.into_body().collect().await.unwrap().to_bytes();
    let prefix = serde_json::from_slice::<Value>(&payload).unwrap()["path_prefix"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(prefix.starts_with("/cap/"));
    assert!(prefix.ends_with("/api/v1/posts/1/files/0/support/"));

    let support = request(
        app.clone(),
        "GET",
        &format!("{prefix}app.js"),
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(support.status(), StatusCode::OK);
    assert_eq!(
        support.into_body().collect().await.unwrap().to_bytes(),
        "window.capabilityLoaded = true"
    );
    let escaped = prefix.replace("/posts/1/files/0/", "/posts/1/files/1/");
    assert_eq!(
        request(
            app.clone(),
            "GET",
            &format!("{escaped}app.js"),
            &[],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            app,
            "POST",
            "/api/v1/posts/1/files/0/html-capability",
            &[],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn login_cookie_is_bounded_and_cookie_mutations_require_exact_origin() {
    let root = TempDir::new().unwrap();
    let app = authenticated_app(&root);
    let wrong = request(
        app.clone(),
        "POST",
        "/api/v1/auth/session",
        &[("content-type", "application/json")],
        Body::from(r#"{"token":"wrong"}"#),
    )
    .await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let login = request(
        app.clone(),
        "POST",
        "/api/v1/auth/session",
        &[("content-type", "application/json")],
        Body::from(format!(r#"{{"token":"{}"}}"#, "a".repeat(64))),
    )
    .await;
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    assert_eq!(login.headers()[header::CACHE_CONTROL], "no-store");
    let set_cookie = login.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.starts_with("glim_session="));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Secure"));
    assert!(set_cookie.contains("Max-Age=43200"));
    let cookie = set_cookie.split(';').next().unwrap();

    assert_eq!(
        request(
            app.clone(),
            "GET",
            "/api/v1/posts",
            &[("cookie", cookie)],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::OK
    );
    for origin in [None, Some("https://attacker.invalid"), Some(ORIGIN)] {
        let mut headers = vec![("cookie", cookie)];
        if let Some(origin) = origin {
            headers.push(("origin", origin));
        }
        let response = request(
            app.clone(),
            "POST",
            "/api/v1/sessions/abc123/heartbeat",
            &headers,
            Body::empty(),
        )
        .await;
        assert_eq!(
            response.status(),
            if origin == Some(ORIGIN) {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::FORBIDDEN
            }
        );
    }

    let logout = request(
        app.clone(),
        "DELETE",
        "/api/v1/auth/session",
        &[("cookie", cookie), ("origin", ORIGIN)],
        Body::empty(),
    )
    .await;
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert_eq!(logout.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        logout.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(
        request(
            app.clone(),
            "GET",
            "/api/v1/posts",
            &[("cookie", cookie)],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );

    let bearer = format!("Bearer {}", "a".repeat(64));
    assert_eq!(
        request(
            app,
            "POST",
            "/api/v1/sessions/abc123/heartbeat",
            &[("authorization", &bearer)],
            Body::empty(),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
}
