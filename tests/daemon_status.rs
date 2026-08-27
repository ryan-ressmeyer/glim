use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use glim::{
    daemon::{DEFAULT_MAX_FINALIZED_BLOB_BYTES, DEFAULT_MAX_UPLOAD_BYTES, DaemonLimits},
    storage::{Store, StoreError, StoreLimits},
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::TempDir;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn sqlite_path(root: &TempDir) -> std::path::PathBuf {
    root.path().join("metadata.sqlite3")
}

#[test]
fn storage_status_is_consistent_and_rejects_negative_metadata() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 512,
            max_finalized_blob_bytes: 2048,
        },
    )
    .unwrap();
    let a = store.resolve_session("test", "active", "P", "/p").unwrap();
    let d = store.resolve_session("test", "due", "P", "/p").unwrap();
    let connection = rusqlite::Connection::open(sqlite_path(&root)).unwrap();
    connection
        .execute(
            "UPDATE sessions SET last_activity_at=101 WHERE public_id=?1",
            [&a.public_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sessions SET last_activity_at=100 WHERE public_id=?1",
            [&d.public_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO blobs(hash, byte_size) VALUES (?1, 17)",
            ["a".repeat(64)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO blob_deletion_queue(blob_hash) VALUES (?1)",
            ["a".repeat(64)],
        )
        .unwrap();
    let snapshot = store.status_snapshot(100).unwrap();
    assert_eq!(snapshot.finalized_unique_blob_bytes, 17);
    assert_eq!(snapshot.active_sessions, 1);
    assert_eq!(snapshot.sessions_due_for_purge, 1);
    assert_eq!(snapshot.queued_blob_deletions, 1);
    assert_eq!(
        snapshot.limits,
        StoreLimits {
            max_upload_bytes: 512,
            max_finalized_blob_bytes: 2048
        }
    );
    connection
        .execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE blobs SET byte_size=-1;")
        .unwrap();
    assert!(matches!(
        store.status_snapshot(100),
        Err(StoreError::InvalidStatusValue { .. })
    ));
}

#[tokio::test]
async fn health_is_minimal_and_status_is_expanded_without_refreshing_activity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 512,
            max_finalized_blob_bytes: 2048,
        },
    )
    .unwrap();
    let session = store
        .resolve_session(
            "secret-namespace",
            "secret-key",
            "secret-title",
            "/secret/path",
        )
        .unwrap();
    let before = store.session(&session.public_id).unwrap().last_activity_at;
    let app = glim::app_with_store(store);
    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let health: Value =
        serde_json::from_slice(&health.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(health.as_object().unwrap().len(), 2);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    for secret in [
        "secret-namespace",
        "secret-key",
        "secret-title",
        "/secret/path",
        &session.public_id,
    ] {
        assert!(!text.contains(secret));
    }
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status["ok"], true);
    assert_eq!(status["max_upload_bytes"], 512);
    assert_eq!(status["max_finalized_blob_bytes"], 2048);
    assert_eq!(status["retention_seconds"], 604800);
    assert_eq!(status["cleanup_interval_seconds"], 3600);
    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(
        reopened
            .session(&session.public_id)
            .unwrap()
            .last_activity_at,
        before
    );
}

#[test]
fn limit_defaults_and_strict_resolution_are_finite() {
    assert_eq!(DEFAULT_MAX_UPLOAD_BYTES, 536_870_912);
    assert_eq!(DEFAULT_MAX_FINALIZED_BLOB_BYTES, 21_474_836_480);
    let legacy = glim::daemon::resolve_daemon_configuration_limit_values(
        Some(br#"{"schema_version":1}"#),
        None,
        None,
    )
    .unwrap();
    assert_eq!(legacy, DaemonLimits::default());
    let file =
        br#"{"schema_version":1,"limits":{"max_upload_bytes":10,"max_finalized_blob_bytes":20}}"#;
    assert_eq!(
        glim::daemon::resolve_daemon_configuration_limit_values(
            Some(file),
            Some(std::ffi::OsStr::new("12")),
            Some(std::ffi::OsStr::new("30"))
        )
        .unwrap(),
        DaemonLimits {
            max_upload_bytes: 12,
            max_finalized_blob_bytes: 30
        }
    );
    for bad in [br#"{"schema_version":1,"limits":{"max_upload_bytes":0,"max_finalized_blob_bytes":20}}"#.as_slice(), br#"{"schema_version":1,"limits":{"max_upload_bytes":21,"max_finalized_blob_bytes":20}}"#.as_slice(), br#"{"schema_version":1,"limits":{"max_upload_bytes":10,"max_finalized_blob_bytes":20,"unknown":1}}"#.as_slice()] { assert!(glim::daemon::resolve_daemon_configuration_limit_values(Some(bad), None, None).is_err()); }
    for bad in ["", "0", "-1", "+1", "1.0", "18446744073709551616"] {
        assert!(
            glim::daemon::resolve_daemon_configuration_limit_values(
                Some(file),
                Some(std::ffi::OsStr::new(bad)),
                None
            )
            .is_err()
        );
    }
}

#[test]
fn production_open_store_applies_the_resolved_upload_boundary() {
    let root = TempDir::new().unwrap();
    let store = glim::daemon::open_store(
        glim::daemon::StoreRoot {
            path: root.path().join("store"),
            explicit_override: true,
        },
        DaemonLimits {
            max_upload_bytes: 512,
            max_finalized_blob_bytes: 2048,
        },
    )
    .unwrap();
    store
        .stage_publication_blob(std::io::Cursor::new(vec![0_u8; 512]))
        .unwrap();
    assert!(matches!(
        store.stage_publication_blob(std::io::Cursor::new(vec![0_u8; 513])),
        Err(StoreError::UploadLimitExceeded {
            limit: 512,
            attempted: 513
        })
    ));
}

#[test]
fn cutoff_arithmetic_is_checked() {
    assert_eq!(
        glim::daemon::cleanup_cutoff(UNIX_EPOCH + std::time::Duration::from_secs(604_801)).unwrap(),
        1
    );
    assert!(glim::daemon::cleanup_cutoff(UNIX_EPOCH - std::time::Duration::from_secs(1)).is_err());
    assert!(glim::daemon::cleanup_cutoff(UNIX_EPOCH + std::time::Duration::from_secs(1)).is_err());
    let _ = SystemTime::now();
}

#[tokio::test(flavor = "multi_thread")]
async fn periodic_cleanup_retries_after_open_failure() {
    let log = SharedLogWriter::default();
    glim::logging::initialize_daemon_with_writer(glim::logging::LogLevel::Info, log.clone());
    let parent = TempDir::new().unwrap();
    let root_path = parent.path().join("store");
    std::fs::write(&root_path, b"temporarily not a directory").unwrap();
    let root = glim::daemon::StoreRoot {
        path: root_path.clone(),
        explicit_override: true,
    };
    let worker = glim::daemon::spawn_periodic_cleanup_with_interval(
        root.clone(),
        DaemonLimits::default(),
        std::time::Duration::from_millis(20),
    );
    tokio::time::sleep(std::time::Duration::from_millis(35)).await;
    std::fs::remove_file(&root_path).unwrap();
    let mut store = Store::open(&root_path).unwrap();
    let session = store.resolve_session("test", "stale", "P", "/p").unwrap();
    rusqlite::Connection::open(root_path.join("metadata.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at=0 WHERE public_id=?1",
            [&session.public_id],
        )
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if store.session(&session.public_id).is_err() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "periodic cleanup did not retry"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    worker.abort();
    let bytes = log.0.lock().unwrap().clone();
    let events = String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let failed = events
        .iter()
        .find(|event| event["event"] == "cleanup_failed")
        .unwrap();
    assert_eq!(failed["trigger"], "periodic");
    assert_eq!(failed["category"], "cleanup_operation_failed");
    assert!(failed.as_object().unwrap().keys().all(|key| {
        [
            "schema_version",
            "timestamp",
            "level",
            "event",
            "trigger",
            "category",
        ]
        .contains(&key.as_str())
    }));
    let completed = events
        .iter()
        .find(|event| event["event"] == "cleanup_completed")
        .unwrap();
    assert_eq!(completed["trigger"], "periodic");
    assert_eq!(completed["sessions_deleted"], 1);
    assert!(
        !String::from_utf8(log.0.lock().unwrap().clone())
            .unwrap()
            .contains(root_path.to_str().unwrap())
    );
}
