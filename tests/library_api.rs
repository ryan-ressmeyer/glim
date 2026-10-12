use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use glim::{
    daemon::AccessToken,
    storage::{PublicationFile, PublicationRequest, Store},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::io::Cursor;
use tower::ServiceExt;

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn stale_confirmed_delete_cannot_delete_a_new_api_publication() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let blob = store.stage_publication_blob(Cursor::new(b"old")).unwrap();
    let old = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Old".into(),
                commentary: "Old".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "old.txt".into(),
                    caption: None,
                    blob,
                    support_assets: vec![],
                }],
            },
            100,
        )
        .unwrap()
        .id;
    store.set_saved(old, true).unwrap();
    let auth = [(
        "authorization",
        "Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )];
    let app = glim::app_with_store_and_token_auth(
        store,
        AccessToken::parse(&"a".repeat(64)).unwrap(),
        "http://localhost:3030".into(),
        false,
    );
    let uri = format!("/api/v1/posts/{old}?confirm_saved=true");
    assert_eq!(
        call(&app, "DELETE", &uri, json!({}), &auth).await.0,
        StatusCode::OK
    );

    // The daemon store is shared with the API; a fresh direct publication uses the same allocator.
    let mut reopened = Store::open(root.path()).unwrap();
    let blob = reopened
        .stage_publication_blob(Cursor::new(b"new"))
        .unwrap();
    let next = reopened
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "New".into(),
                commentary: "New".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "new.txt".into(),
                    caption: None,
                    blob,
                    support_assets: vec![],
                }],
            },
            101,
        )
        .unwrap()
        .id;
    assert!(next > old);
    assert_eq!(
        call(&app, "DELETE", &uri, json!({}), &auth).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/v1/posts/{next}"),
            json!({}),
            &auth
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn management_routes_save_list_delete_and_reclaim_with_token_auth() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "external", "Project", "/project")
        .unwrap();
    let blob = store.stage_publication_blob(Cursor::new(b"saved")).unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Saved".into(),
                commentary: "Result".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "saved.txt".into(),
                    caption: None,
                    blob,
                    support_assets: vec![],
                }],
            },
            100,
        )
        .unwrap();
    let app = glim::app_with_store_and_token_auth(
        store,
        AccessToken::parse(&"a".repeat(64)).unwrap(),
        "http://localhost:3030".into(),
        false,
    );
    let uri = format!("/api/v1/posts/{}/saved", post.id);
    assert_eq!(
        call(&app, "PUT", &uri, json!({}), &[]).await.0,
        StatusCode::UNAUTHORIZED
    );
    let auth = [(
        "authorization",
        "Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )];
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/session")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token":"a".repeat(64)}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(&app, "PUT", &uri, json!({}), &[("cookie", &cookie)])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            &uri,
            json!({}),
            &[("cookie", &cookie), ("origin", "http://localhost:3030")]
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "PUT", &uri, json!({}), &auth).await.0,
        StatusCode::OK
    );
    let (status, saved) = call(&app, "GET", "/api/v1/saved/posts?limit=1", json!({}), &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["posts"][0]["id"], post.id);
    let (status, sessions) = call(&app, "GET", "/api/v1/sessions?limit=1", json!({}), &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions["sessions"][0]["saved_count"], 1);
    let delete = format!("/api/v1/posts/{}", post.id);
    assert_eq!(
        call(&app, "DELETE", &delete, json!({}), &auth).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("{delete}?confirm_saved=true"),
            json!({}),
            &auth
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", "/api/v1/storage/reclaim", json!({}), &auth)
            .await
            .0,
        StatusCode::OK
    );
}
