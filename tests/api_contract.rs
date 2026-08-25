use std::{
    io::Cursor,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use glim::storage::{PublicationFile, PublicationRequest, Store};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn assert_v1_error(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Body,
    expected_status: StatusCode,
    expected_code: &str,
) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected_status, "{method} {uri}");
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json",
        "{method} {uri}"
    );
    let payload: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["error"]["code"], expected_code, "{method} {uri}");
    assert!(payload["error"]["details"].is_object(), "{method} {uri}");
}

fn resolve_body(key: &str) -> Value {
    json!({"integration_namespace":"pi","external_key":key,"project_label":"Glim","working_directory":"/tmp/glim"})
}

#[tokio::test]
async fn resolve_lookup_heartbeat_and_close_have_stable_lifecycle_contracts() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let (status, first) = request(
        app.clone(),
        "POST",
        "/api/v1/sessions",
        Some(resolve_body("one")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let public_id = first["public_id"].as_str().unwrap();
    assert_eq!(first["integration_namespace"], "pi");
    assert_eq!(first["external_key"], "one");
    assert_eq!(first["project"]["label"], "Glim");
    assert_eq!(first["project"]["working_directory"], "/tmp/glim");
    let (_, repeated) = request(
        app.clone(),
        "POST",
        "/api/v1/sessions",
        Some(resolve_body("one")),
    )
    .await;
    assert_eq!(repeated["id"], first["id"]);
    assert_eq!(repeated["public_id"], first["public_id"]);

    let (status, found) = request(
        app.clone(),
        "GET",
        &format!("/api/v1/sessions/{public_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found, repeated);
    rusqlite::Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at = 0 WHERE public_id = ?1",
            [public_id],
        )
        .unwrap();
    let before = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let (status, heartbeat) = request(
        app.clone(),
        "POST",
        &format!("/api/v1/sessions/{public_id}/heartbeat"),
        None,
    )
    .await;
    let after = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat["updated"], true);
    assert!(
        heartbeat["last_activity_at"]
            .as_i64()
            .is_some_and(|value| (before..=after).contains(&value))
    );

    for occurred_at in [0, i64::MAX] {
        let (status, error) = request(
            app.clone(),
            "POST",
            &format!("/api/v1/sessions/{public_id}/heartbeat"),
            Some(json!({"occurred_at": occurred_at})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "unexpected_request_body");
    }
    let (status, error) = request(
        app.clone(),
        "POST",
        "/api/v1/sessions/missing/heartbeat",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "session_not_found");

    let (status, report) = request(
        app.clone(),
        "DELETE",
        &format!("/api/v1/sessions/{public_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["sessions_deleted"], 1);
    let (_, repeated_close) = request(
        app.clone(),
        "DELETE",
        &format!("/api/v1/sessions/{public_id}"),
        None,
    )
    .await;
    assert_eq!(repeated_close["sessions_deleted"], 0);
    let (status, error) = request(
        app.clone(),
        "GET",
        &format!("/api/v1/sessions/{public_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "session_not_found");
    let (status, error) = request(app, "GET", "/api/v1/posts/1", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "post_not_found");
}

#[tokio::test]
async fn listing_and_lookup_preserve_nested_json_and_differentiate_errors() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "post", "Glim", "/tmp/glim")
        .unwrap();
    let project_id = session.project_id;
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Plot".into(),
                commentary: "one\n\ntwo".into(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "plot.txt".into(),
                    caption: Some("a\nb".into()),
                    blob: store.stage_publication_blob(Cursor::new(b"bytes")).unwrap(),
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap();
    let app = glim::app_with_store(store);

    for uri in [
        format!("/api/v1/sessions/{}/posts?limit=1", session.public_id),
        format!("/api/v1/projects/{project_id}/posts"),
        "/api/v1/posts".into(),
    ] {
        let (status, page) = request(app.clone(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["posts"][0]["id"], post.id);
        assert_eq!(page["posts"][0]["commentary"], "one\n\ntwo");
        assert_eq!(page["posts"][0]["files"][0]["caption"], "a\nb");
        assert_eq!(page["posts"][0]["files"][0]["blob"]["byte_size"], 5);
    }
    let (status, found) = request(
        app.clone(),
        "GET",
        &format!("/api/v1/posts/{}", post.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["title"], "Plot");

    for (uri, code) in [
        ("/api/v1/sessions/missing/posts", "session_not_found"),
        ("/api/v1/projects/999/posts", "project_not_found"),
        ("/api/v1/posts/999", "post_not_found"),
    ] {
        let (status, error) = request(app.clone(), "GET", uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["error"]["code"], code);
    }
    let (status, error) = request(app, "GET", "/api/v1/posts?cursor=bad", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_page_cursor");
}

#[tokio::test]
async fn malformed_inputs_and_unconfigured_state_use_one_sanitized_envelope() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let (status, unknown) = request(app.clone(), "POST", "/api/v1/sessions", Some(json!({"integration_namespace":"pi","external_key":"x","project_label":"Glim","working_directory":"/tmp/glim","typo":true}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown["error"]["code"], "malformed_json");
    assert!(
        unknown["error"]["message"]
            .as_str()
            .unwrap()
            .contains("JSON")
    );
    assert!(!unknown.to_string().contains("sqlite"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let malformed: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(malformed["error"]["code"], "malformed_json");
    let (status, path) = request(app, "GET", "/api/v1/posts/not-a-number", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(path["error"]["code"], "malformed_path");

    let (status, unavailable) = request(glim::app(), "GET", "/api/v1/posts", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unavailable["error"]["code"], "storage_unavailable");
}

#[tokio::test]
async fn corrupt_post_metadata_is_sanitized_without_poisoning_later_reads() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "corrupt", "Glim", "/tmp/glim")
        .unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Corrupt".into(),
                commentary: "Metadata".into(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "one.bin".into(),
                    caption: None,
                    blob: store.stage_publication_blob(Cursor::new(b"x")).unwrap(),
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap();
    rusqlite::Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .execute_batch("PRAGMA ignore_check_constraints = ON; UPDATE blobs SET byte_size = -1;")
        .unwrap();
    let app = glim::app_with_store(store);

    let (status, error) = request(
        app.clone(),
        "GET",
        &format!("/api/v1/posts/{}", post.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"]["code"], "storage_io_error");
    assert!(error["error"]["details"].is_object());
    assert!(!error.to_string().contains("byte size"));

    let (status, found) = request(
        app,
        "GET",
        &format!("/api/v1/sessions/{}", session.public_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["public_id"], session.public_id);
}

#[tokio::test]
async fn every_v1_routing_and_extractor_failure_uses_the_json_envelope() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());

    assert_v1_error(
        app.clone(),
        "GET",
        "/api/v1/unknown",
        Body::empty(),
        StatusCode::NOT_FOUND,
        "api_route_not_found",
    )
    .await;
    assert_v1_error(
        app.clone(),
        "PUT",
        "/api/v1/health",
        Body::empty(),
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    )
    .await;
    assert_v1_error(
        app.clone(),
        "GET",
        "/api/v1/posts/not-a-number",
        Body::empty(),
        StatusCode::BAD_REQUEST,
        "malformed_path",
    )
    .await;
    assert_v1_error(
        app.clone(),
        "GET",
        "/api/v1/posts?limit=nope",
        Body::empty(),
        StatusCode::BAD_REQUEST,
        "malformed_query",
    )
    .await;
    assert_v1_error(
        app.clone(),
        "GET",
        "/api/v1/posts?unknown=true",
        Body::empty(),
        StatusCode::BAD_REQUEST,
        "malformed_query",
    )
    .await;
    assert_v1_error(
        app.clone(),
        "POST",
        "/api/v1/sessions",
        Body::from("{"),
        StatusCode::BAD_REQUEST,
        "malformed_json",
    )
    .await;
    assert_v1_error(app.clone(), "POST", "/api/v1/sessions", Body::from(json!({"integration_namespace":"pi","external_key":"x","project_label":"Glim","working_directory":"/tmp/glim","unknown":true}).to_string()), StatusCode::BAD_REQUEST, "malformed_json").await;
    assert_v1_error(
        app.clone(),
        "POST",
        "/api/v1/sessions/missing/heartbeat",
        Body::from(vec![b'x'; 2_100_000]),
        StatusCode::BAD_REQUEST,
        "malformed_json",
    )
    .await;

    for uri in ["/api/v1/sessions/%ZZ", "/api/v1/sessions/%FF"] {
        if Request::builder().uri(uri).body(Body::empty()).is_ok() {
            assert_v1_error(
                app.clone(),
                "GET",
                uri,
                Body::empty(),
                StatusCode::BAD_REQUEST,
                "malformed_path",
            )
            .await;
        }
    }

    let outside = app
        .oneshot(
            Request::builder()
                .uri("/api/v10/unknown")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(outside.status(), StatusCode::NOT_FOUND);
    assert!(outside.headers().get("content-type").is_none());
}

#[tokio::test]
async fn internal_database_errors_are_sanitized() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    rusqlite::Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .execute_batch("DROP TABLE posts")
        .unwrap();

    let (status, error) = request(app, "GET", "/api/v1/posts", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"]["code"], "database_error");
    let serialized = error.to_string().to_lowercase();
    assert!(!serialized.contains("sqlite"));
    assert!(!serialized.contains("no such table"));
    assert!(!serialized.contains(root.path().to_str().unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloned_router_serializes_concurrent_store_requests_without_panicking() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let calls = (0..16)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move {
                request(
                    app,
                    "POST",
                    "/api/v1/sessions",
                    Some(resolve_body("shared")),
                )
                .await
            })
        })
        .collect::<Vec<_>>();
    let mut ids = Vec::new();
    for call in calls {
        let (status, body) = call.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        ids.push(body["id"].clone());
    }
    assert!(ids.iter().all(|id| id == &ids[0]));
}
