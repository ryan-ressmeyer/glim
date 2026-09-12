use axum::{body::Body, http::Request};
use glim::storage::{PublicationFile, PublicationRequest, Store};
use http_body_util::BodyExt;
use std::io::Cursor;
use tempfile::TempDir;
use tower::ServiceExt;

fn seeded_store(root: &TempDir) -> (Store, String, i64) {
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "live", "Glim", "/tmp/live")
        .unwrap();
    let project_id = session.project_id;
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "First".into(),
                commentary: "one".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "one.txt".into(),
                    caption: None,
                    blob: store.stage_publication_blob(Cursor::new(b"one")).unwrap(),
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap();
    (store, session.public_id, project_id)
}

async fn next_chunk(body: &mut Body) -> String {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
        .await
        .expect("SSE frame timeout")
        .expect("SSE ended")
        .expect("SSE body error");
    String::from_utf8(frame.into_data().expect("data frame").to_vec()).unwrap()
}

#[tokio::test]
async fn sse_replays_scoped_posts_and_rejects_malformed_last_event_id() {
    let root = TempDir::new().unwrap();
    let (store, public_id, project_id) = seeded_store(&root);
    let app = glim::app_with_store(store);
    for uri in [
        "/api/v1/posts/events".to_owned(),
        format!("/api/v1/projects/{project_id}/posts/events"),
        format!("/api/v1/sessions/{public_id}/posts/events"),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let mut body = response.into_body();
        let chunk = next_chunk(&mut body).await;
        assert!(chunk.contains("event: post"), "{chunk}");
        assert!(chunk.contains("id: 1"), "{chunk}");
        assert!(chunk.contains("\"title\":\"First\""), "{chunk}");
    }
    for malformed in ["0", "nope"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/posts/events")
                    .header("last-event-id", malformed)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let payload = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&payload).unwrap()["error"]["code"],
            "malformed_last_event_id"
        );
    }
}

fn multipart_publication(index: usize) -> Request<Body> {
    let boundary = format!("concurrent-boundary-{index}");
    let manifest = serde_json::json!({"integration_namespace":"pi","external_key":"live","project_label":"Glim","working_directory":"/tmp/live","title":format!("Concurrent {index}"),"commentary":"ordered","files":[{"part":"file","filename":format!("{index}.txt"),"support_assets":[]}]});
    let multipart = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n{manifest}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\n{index}\r\n--{boundary}--\r\n"
    );
    Request::builder()
        .method("POST")
        .uri("/api/v1/posts")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(multipart))
        .unwrap()
}

fn event_id(chunk: &str) -> i64 {
    chunk
        .lines()
        .find_map(|line| line.strip_prefix("id: "))
        .expect("post event ID")
        .parse()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_publications_are_delivered_in_commit_id_order() {
    const PUBLICATIONS: usize = 96;
    let root = TempDir::new().unwrap();
    let (store, _, _) = seeded_store(&root);
    let app = glim::app_with_store(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/events")
                .header("last-event-id", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    assert!(next_chunk(&mut body).await.contains("event: reset"));

    for start in (0..PUBLICATIONS).step_by(4) {
        let mut publications = Vec::new();
        for index in start..(start + 4).min(PUBLICATIONS) {
            let request_app = app.clone();
            publications.push(tokio::spawn(async move {
                request_app
                    .oneshot(multipart_publication(index))
                    .await
                    .unwrap()
                    .status()
            }));
        }
        for publication in publications {
            assert_eq!(publication.await.unwrap(), 201);
        }
    }
    let reader = tokio::spawn(async move {
        let mut ids = Vec::new();
        for _ in 0..PUBLICATIONS {
            ids.push(event_id(&next_chunk(&mut body).await));
        }
        ids
    });
    let ids = reader.await.unwrap();
    assert_eq!(ids, (2..=PUBLICATIONS as i64 + 1).collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn lagged_stream_resets_past_backlog_before_resuming_live_delivery() {
    const PUBLICATIONS: usize = 300;
    let root = TempDir::new().unwrap();
    let (store, _, _) = seeded_store(&root);
    let app = glim::app_with_store(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts/events")
                .header("last-event-id", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    assert!(next_chunk(&mut body).await.contains("event: reset"));

    for start in (0..PUBLICATIONS).step_by(4) {
        let mut publications = Vec::new();
        for index in start..(start + 4).min(PUBLICATIONS) {
            let request_app = app.clone();
            publications.push(tokio::spawn(async move {
                request_app
                    .oneshot(multipart_publication(index))
                    .await
                    .unwrap()
                    .status()
            }));
        }
        for publication in publications {
            assert_eq!(publication.await.unwrap(), 201);
        }
    }

    let mut reset = false;
    for _ in 0..40 {
        if next_chunk(&mut body).await.contains("event: reset") {
            reset = true;
            break;
        }
    }
    assert!(reset, "lagged stream did not reset");

    let published = app
        .clone()
        .oneshot(multipart_publication(PUBLICATIONS + 1))
        .await
        .unwrap();
    assert_eq!(published.status(), 201);
    let resumed = next_chunk(&mut body).await;
    assert!(resumed.contains("event: post"), "{resumed}");
    assert_eq!(event_id(&resumed), PUBLICATIONS as i64 + 2);
}

#[tokio::test]
async fn committed_publication_and_successful_close_reach_open_streams_with_scope_isolation() {
    let root = TempDir::new().unwrap();
    let (store, public_id, project_id) = seeded_store(&root);
    let app = glim::app_with_store(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/projects/{project_id}/posts/events"))
                .header("last-event-id", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    assert!(next_chunk(&mut body).await.contains("event: reset"));

    let boundary = "live-boundary";
    let manifest = serde_json::json!({"integration_namespace":"pi","external_key":"live","project_label":"Glim","working_directory":"/tmp/live","title":"Second","commentary":"two","files":[{"part":"file","filename":"two.txt","support_assets":[]}]});
    let multipart = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n{manifest}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\ntwo\r\n--{boundary}--\r\n"
    );
    let published = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(multipart))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(published.status(), 201);
    let post_event = next_chunk(&mut body).await;
    assert!(post_event.contains("event: post"), "{post_event}");
    assert!(post_event.contains("\"title\":\"Second\""), "{post_event}");

    let closed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/sessions/{public_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(closed.status(), 200);
    let close_event = next_chunk(&mut body).await;
    assert!(
        close_event.contains("event: session-closed"),
        "{close_event}"
    );
    assert!(close_event.contains(&public_id), "{close_event}");
    let reset = next_chunk(&mut body).await;
    assert!(reset.contains("event: reset"), "{reset}");
    assert!(reset.contains("\"reason\":\"session_deleted\""), "{reset}");
}

#[tokio::test]
async fn reconnect_after_deleted_history_receives_authoritative_reset() {
    let root = TempDir::new().unwrap();
    let (mut store, public_id, _) = seeded_store(&root);
    store.close_session(&public_id).unwrap();
    let app = glim::app_with_store(store);

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/sessions/{public_id}/posts/events"))
                .header("last-event-id", "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let reset = next_chunk(&mut response.into_body()).await;
    assert!(reset.contains("event: reset"), "{reset}");
    assert!(reset.contains("\"reason\":\"reconnect\""), "{reset}");
}

#[tokio::test]
async fn periodic_cleanup_resets_an_open_feed() {
    let root = TempDir::new().unwrap();
    let (mut store, public_id, project_id) = seeded_store(&root);
    let notifier = glim::storage::LifecycleNotifier::default();
    store.configure_lifecycle_notifier(notifier.clone());
    rusqlite::Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at = 0 WHERE public_id = ?1",
            [&public_id],
        )
        .unwrap();
    let app = glim::app_with_store(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/projects/{project_id}/posts/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    assert!(next_chunk(&mut body).await.contains("event: post"));

    let cleanup = glim::daemon::spawn_periodic_cleanup_with_interval_and_notifier(
        glim::daemon::StoreRoot {
            path: root.path().to_owned(),
            explicit_override: true,
        },
        glim::daemon::DaemonLimits::default(),
        std::time::Duration::from_millis(20),
        notifier,
    );
    let reset = next_chunk(&mut body).await;
    cleanup.abort();
    assert!(reset.contains("event: reset"), "{reset}");
    assert!(reset.contains("\"reason\":\"storage_changed\""), "{reset}");
}

#[tokio::test]
async fn close_reports_committed_deletion_when_blob_cleanup_is_deferred() {
    let root = TempDir::new().unwrap();
    let (store, public_id, project_id) = seeded_store(&root);
    let hash = rusqlite::Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .query_row("SELECT hash FROM blobs", [], |row| row.get::<_, String>(0))
        .unwrap();
    let blob_path = root
        .path()
        .join("blobs")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash);
    std::fs::remove_file(&blob_path).unwrap();
    std::fs::create_dir(&blob_path).unwrap();
    let app = glim::app_with_store(store);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/projects/{project_id}/posts/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut events = response.into_body();
    assert!(next_chunk(&mut events).await.contains("event: post"));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/sessions/{public_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let payload: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["sessions_deleted"], 1);
    assert_eq!(payload["blobs_queued"], 1);
    assert_eq!(payload["blobs_deleted"], 0);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status["queued_blob_deletions"], 1);
    let closed = next_chunk(&mut events).await;
    assert!(closed.contains("event: session-closed"), "{closed}");
    assert!(closed.contains(&public_id), "{closed}");
    let reset = next_chunk(&mut events).await;
    assert!(reset.contains("event: reset"), "{reset}");
    assert!(reset.contains("\"reason\":\"session_deleted\""), "{reset}");
}
