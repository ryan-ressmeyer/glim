use std::{
    io::Cursor,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{Body, Bytes},
    http::{Request, StatusCode},
};
use glim::storage::{PublicationFile, PublicationRequest, Store, StoreLimits};
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
                git: None,
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
                git: None,
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

fn multipart_body(boundary: &str, manifest: &Value, parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\nContent-Type: application/json\r\n\r\n{}\r\n", manifest).as_bytes());
    for (name, bytes) in parts {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"ignored-client-name\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

#[tokio::test]
async fn multipart_publication_preserves_manifest_order_text_assets_and_exact_hashes() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let manifest = json!({
        "integration_namespace":"pi", "external_key":"stream", "project_label":"Glim",
        "working_directory":"/identity/only", "title":"Streaming", "commentary":"line one\n\nline two",
        "files":[
            {"part":"second-arrival", "filename":"entry.md", "caption":"cap\nline", "support_assets":[{"part":"asset", "relative_path":"z-last.bin"},{"part":"asset-two", "relative_path":"a-first.bin"}]},
            {"part":"first-arrival", "filename":"raw.bin", "caption":null, "support_assets":[]}
        ]
    });
    let boundary = "glim-boundary";
    let body = multipart_body(
        boundary,
        &manifest,
        &[
            ("first-arrival", b"raw"),
            ("asset-two", b"two"),
            ("asset", b"asset bytes"),
            ("second-arrival", b"entry"),
        ],
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let payload: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["session"]["external_key"], "stream");
    assert_eq!(payload["post"]["commentary"], "line one\n\nline two");
    assert_eq!(payload["post"]["files"][0]["filename"], "entry.md");
    assert_eq!(payload["post"]["files"][0]["caption"], "cap\nline");
    assert_eq!(
        payload["post"]["files"][0]["blob"]["hash"],
        "923fe53966c6cd9343e11af776cd4b05be315ea4b200b02e4d5dfb0f929b73bf"
    );
    assert_eq!(
        payload["post"]["files"][0]["support_assets"][0]["relative_path"],
        "z-last.bin"
    );
    assert_eq!(
        payload["post"]["files"][0]["support_assets"][1]["relative_path"],
        "a-first.bin"
    );
    assert_eq!(payload["post"]["files"][1]["filename"], "raw.bin");
    let (status, page) = request(app, "GET", "/api/v1/posts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["posts"][0], payload["post"]);
}

async fn post_multipart(app: axum::Router, boundary: &str, body: Vec<u8>) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let payload =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    (status, payload)
}

fn minimal_manifest() -> Value {
    json!({"integration_namespace":"pi","external_key":"errors","project_label":"Glim","working_directory":"/tmp/errors","title":"Title","commentary":"Commentary","files":[{"part":"file","filename":"x.bin","support_assets":[]}]})
}

fn assert_clean_publication_store(root: &TempDir) {
    let db = rusqlite::Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    for table in ["projects", "sessions", "posts", "blobs"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("blobs/publication-staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn multipart_manifest_and_part_contract_errors_are_typed_and_leave_no_state() {
    let cases = vec![
        (
            "artifact-first",
            b"--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nx\r\n--b--\r\n".to_vec(),
            "manifest_must_be_first",
            StatusCode::BAD_REQUEST,
        ),
        (
            "invalid-utf8",
            b"--b\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n\xff\r\n--b--\r\n"
                .to_vec(),
            "manifest_not_utf8",
            StatusCode::BAD_REQUEST,
        ),
        (
            "bad-json",
            b"--b\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n{\r\n--b--\r\n"
                .to_vec(),
            "malformed_manifest",
            StatusCode::BAD_REQUEST,
        ),
        (
            "unknown",
            multipart_body(
                "b",
                &json!({"integration_namespace":"pi","external_key":"x","project_label":"G","working_directory":"/x","title":"T","commentary":"C","files":[],"source_path":"/secret"}),
                &[],
            ),
            "malformed_manifest",
            StatusCode::BAD_REQUEST,
        ),
        (
            "declared-empty-name",
            multipart_body(
                "b",
                &json!({"integration_namespace":"pi","external_key":"x","project_label":"G","working_directory":"/x","title":"T","commentary":"C","files":[{"part":"","filename":"f"}]}),
                &[],
            ),
            "invalid_part_name",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "missing",
            multipart_body("b", &minimal_manifest(), &[]),
            "missing_part",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "unexpected",
            multipart_body("b", &minimal_manifest(), &[("other", b"x")]),
            "unexpected_part",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "duplicate",
            multipart_body("b", &minimal_manifest(), &[("file", b"x"), ("file", b"y")]),
            "duplicate_part",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "second-manifest",
            multipart_body("b", &minimal_manifest(), &[("manifest", b"{}")]),
            "duplicate_manifest",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (name, body, code, status) in cases {
        let root = TempDir::new().unwrap();
        let app = glim::app_with_store(Store::open(root.path()).unwrap());
        let (actual_status, payload) = post_multipart(app, "b", body).await;
        assert_eq!(actual_status, status, "{name}: {payload}");
        assert_eq!(payload["error"]["code"], code, "{name}");
        assert_clean_publication_store(&root);
    }
}

#[tokio::test]
async fn http_git_provenance_validation_rejects_unsafe_inert_metadata_before_upload() {
    let invalid = [
        json!({"root":"relative","branch":"main","commit":"a".repeat(40)}),
        json!({"root":"/work\nleak","branch":"main","commit":"a".repeat(40)}),
        json!({"root":"/work","branch":"bad\nbranch","commit":"a".repeat(40)}),
        json!({"root":"/work","branch":"main","commit":"a".repeat(39)}),
    ];
    for git in invalid {
        let root = TempDir::new().unwrap();
        let app = glim::app_with_store(Store::open(root.path()).unwrap());
        let mut manifest = minimal_manifest();
        manifest["git"] = git;
        let (status, payload) = post_multipart(app, "b", multipart_body("b", &manifest, &[])).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(payload["error"]["code"], "validation_failed");
        assert_clean_publication_store(&root);
    }
}

#[tokio::test]
async fn manifest_size_and_declared_part_complexity_are_bounded() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let huge = json!({"integration_namespace":"pi","external_key":"x","project_label":"G","working_directory":"/x","title":"T","commentary":"x".repeat(70_000),"files":[{"part":"f","filename":"f","support_assets":[]}]});
    let (status, payload) = post_multipart(app, "b", multipart_body("b", &huge, &[])).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(payload["error"]["code"], "manifest_too_large");
    assert_clean_publication_store(&root);

    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let files = (0..257)
        .map(|i| json!({"part":format!("p{i}"),"filename":"f","support_assets":[]}))
        .collect::<Vec<_>>();
    let manifest = json!({"integration_namespace":"pi","external_key":"x","project_label":"G","working_directory":"/x","title":"T","commentary":"C","files":files});
    let (status, payload) = post_multipart(app, "b", multipart_body("b", &manifest, &[])).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(payload["error"]["code"], "manifest_complexity_exceeded");
    assert_clean_publication_store(&root);
}

#[tokio::test]
async fn multipart_upload_and_aggregate_limits_cleanup_all_stages_and_identity() {
    for (limits, parts, expected_code) in [
        (
            StoreLimits {
                max_upload_bytes: 3,
                max_finalized_blob_bytes: 100,
            },
            vec![("file", b"abc".as_slice()), ("later", b"four".as_slice())],
            "upload_limit_exceeded",
        ),
        (
            StoreLimits {
                max_upload_bytes: 3,
                max_finalized_blob_bytes: 5,
            },
            vec![("file", b"abc".as_slice()), ("later", b"def".as_slice())],
            "storage_limit_exceeded",
        ),
    ] {
        let root = TempDir::new().unwrap();
        let app = glim::app_with_store(Store::open_with_limits(root.path(), limits).unwrap());
        let manifest = json!({"integration_namespace":"pi","external_key":"limit","project_label":"G","working_directory":"/limit","title":"T","commentary":"C","files":[{"part":"file","filename":"a","support_assets":[]},{"part":"later","filename":"b","support_assets":[]}]});
        let (status, payload) =
            post_multipart(app, "b", multipart_body("b", &manifest, &parts)).await;
        assert!(matches!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE | StatusCode::INSUFFICIENT_STORAGE
        ));
        assert_eq!(payload["error"]["code"], expected_code);
        assert_clean_publication_store(&root);
    }
}

#[tokio::test]
async fn missing_predecessor_and_sql_failure_rollback_resolved_identity_and_blobs() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let mut manifest = minimal_manifest();
    manifest["predecessor_post_id"] = json!(999);
    let (status, payload) = post_multipart(
        app,
        "b",
        multipart_body("b", &manifest, &[("file", b"abc")]),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(payload["error"]["code"], "post_not_found");
    assert_clean_publication_store(&root);

    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    rusqlite::Connection::open(root.path().join("metadata.sqlite3")).unwrap().execute_batch(
        "CREATE TRIGGER reject_http_publication BEFORE INSERT ON posts BEGIN SELECT RAISE(ABORT, 'forced'); END;"
    ).unwrap();
    let app = glim::app_with_store(store);
    let (status, payload) = post_multipart(
        app,
        "b",
        multipart_body("b", &minimal_manifest(), &[("file", b"abc")]),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(payload["error"]["code"], "storage_constraint_conflict");
    assert_clean_publication_store(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_or_cancelled_multipart_does_not_hold_store_mutex_and_cleans_stage() {
    use tokio_stream::wrappers::ReceiverStream;
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let manifest = minimal_manifest();
    let prefix = format!(
        "--b\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n{manifest}\r\n--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\npartial"
    );
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    tx.send(Ok(Bytes::from(prefix))).await.unwrap();
    let request_app = app.clone();
    let task = tokio::spawn(async move {
        request_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/posts")
                    .header("content-type", "multipart/form-data; boundary=b")
                    .body(Body::from_stream(ReceiverStream::new(rx)))
                    .unwrap(),
            )
            .await
    });
    let staging = root.path().join("blobs/publication-staging");
    for _ in 0..100 {
        if std::fs::read_dir(&staging).unwrap().count() > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        std::fs::read_dir(&staging).unwrap().count() > 0,
        "stream never began staging"
    );
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        request(app.clone(), "GET", "/api/v1/posts", None),
    )
    .await;
    assert!(read.is_ok(), "paused multipart held the SQLite mutex");
    task.abort();
    drop(tx);
    for _ in 0..100 {
        if std::fs::read_dir(&staging).unwrap().count() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_clean_publication_store(&root);
}

#[tokio::test]
async fn interrupted_multipart_stream_returns_typed_error_and_cleans_stage() {
    use tokio_stream::wrappers::ReceiverStream;
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(Store::open(root.path()).unwrap());
    let prefix = format!(
        "--b\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n{}\r\n--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\npartial",
        minimal_manifest()
    );
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.send(Ok::<_, std::io::Error>(Bytes::from(prefix)))
        .await
        .unwrap();
    tx.send(Err(std::io::Error::other("disconnected")))
        .await
        .unwrap();
    drop(tx);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts")
                .header("content-type", "multipart/form-data; boundary=b")
                .body(Body::from_stream(ReceiverStream::new(rx)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["error"]["code"], "multipart_stream_error");
    assert_clean_publication_store(&root);
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
