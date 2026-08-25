use std::{
    io::Cursor,
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use glim::storage::{CURRENT_SCHEMA_VERSION, Store, StoreError, StoreLimits};
use rusqlite::Connection;
use tempfile::TempDir;

fn database(root: &TempDir) -> Connection {
    Connection::open(root.path().join("metadata.sqlite3")).unwrap()
}

#[test]
fn version_one_sessions_migrate_with_fresh_equal_activity_timestamps() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path()).unwrap();
    let connection = database(&root);
    connection
        .execute_batch(
            "CREATE TABLE projects (
                 id INTEGER PRIMARY KEY,
                 label TEXT NOT NULL,
                 working_directory TEXT NOT NULL UNIQUE
             );
             CREATE TABLE sessions (
                 id INTEGER PRIMARY KEY,
                 public_id TEXT NOT NULL UNIQUE,
                 integration_namespace TEXT NOT NULL,
                 external_key TEXT NOT NULL,
                 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                 UNIQUE (integration_namespace, external_key, project_id)
             );
             CREATE TABLE posts (
                 id INTEGER PRIMARY KEY,
                 session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 title TEXT NOT NULL,
                 commentary TEXT NOT NULL,
                 predecessor_post_id INTEGER,
                 UNIQUE (id, session_id),
                 FOREIGN KEY (predecessor_post_id, session_id)
                     REFERENCES posts(id, session_id),
                 CHECK (predecessor_post_id IS NULL OR predecessor_post_id <> id)
             );
             CREATE TRIGGER posts_are_immutable
             BEFORE UPDATE ON posts
             BEGIN
                 SELECT RAISE(ABORT, 'posts are immutable');
             END;
             CREATE TABLE blobs (
                 hash TEXT PRIMARY KEY,
                 byte_size INTEGER NOT NULL CHECK (byte_size >= 0)
             );
             CREATE TABLE blob_references (
                 id INTEGER PRIMARY KEY,
                 post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                 blob_hash TEXT NOT NULL REFERENCES blobs(hash),
                 UNIQUE (id, post_id)
             );
             CREATE TABLE post_files (
                 id INTEGER PRIMARY KEY,
                 post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                 blob_reference_id INTEGER NOT NULL UNIQUE,
                 position INTEGER NOT NULL CHECK (position >= 0),
                 filename TEXT NOT NULL,
                 caption TEXT,
                 UNIQUE (id, post_id), UNIQUE (post_id, position),
                 FOREIGN KEY (blob_reference_id, post_id) REFERENCES blob_references(id, post_id)
             );
             CREATE TABLE support_assets (
                 id INTEGER PRIMARY KEY,
                 post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                 entry_file_id INTEGER NOT NULL,
                 blob_reference_id INTEGER NOT NULL UNIQUE,
                 relative_path TEXT NOT NULL,
                 UNIQUE (entry_file_id, relative_path),
                 FOREIGN KEY (entry_file_id, post_id) REFERENCES post_files(id, post_id),
                 FOREIGN KEY (blob_reference_id, post_id) REFERENCES blob_references(id, post_id)
             );
             INSERT INTO projects (id, label, working_directory)
             VALUES (1, 'Glim', '/tmp/glim');
             INSERT INTO sessions (id, public_id, integration_namespace, external_key, project_id)
             VALUES (1, 'legacy', 'pi', 'old', 1);
             INSERT INTO posts (id, session_id, title, commentary)
             VALUES (1, 1, 'Legacy', 'Result');
             PRAGMA user_version = 1;",
        )
        .unwrap();
    drop(connection);

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(CURRENT_SCHEMA_VERSION, 4);
    assert_eq!(reopened.schema_version().unwrap(), 4);
    drop(reopened);

    let connection = database(&root);
    let (created_at, last_activity_at) = connection
        .query_row(
            "SELECT created_at, last_activity_at FROM sessions WHERE public_id = 'legacy'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(created_at, last_activity_at);
    assert!(created_at > 0);
    assert!(
        connection
            .query_row("SELECT published_at FROM posts WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
            > 0
    );
    let immutable = connection
        .execute("UPDATE posts SET published_at = 0 WHERE id = 1", [])
        .unwrap_err();
    assert!(matches!(
        immutable,
        rusqlite::Error::SqliteFailure(_, Some(ref message))
            if message == "posts are immutable"
    ));
}

#[test]
fn new_session_timestamps_are_created_once_and_resolution_is_not_activity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "new", "Glim", "/tmp/glim")
        .unwrap();
    let connection = database(&root);
    connection
        .execute(
            "UPDATE sessions SET last_activity_at = created_at - 10 WHERE public_id = ?1",
            [&session.public_id],
        )
        .unwrap();
    let before = connection
        .query_row(
            "SELECT created_at, last_activity_at FROM sessions WHERE public_id = ?1",
            [&session.public_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    drop(connection);
    thread::sleep(Duration::from_millis(10));

    store
        .resolve_session("pi", "new", "Renamed", "/tmp/glim")
        .unwrap();
    drop(store);
    let after = database(&root)
        .query_row(
            "SELECT created_at, last_activity_at FROM sessions WHERE public_id = ?1",
            [&session.public_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(after, before);
}

#[test]
fn publication_and_visible_heartbeat_advance_activity_monotonically() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "activity", "Glim", "/tmp/glim")
        .unwrap();
    database(&root)
        .execute(
            "UPDATE sessions SET last_activity_at = 100 WHERE public_id = ?1",
            [&session.public_id],
        )
        .unwrap();

    let publication = store
        .record_publication_activity(&session.public_id, 200)
        .unwrap();
    assert!(publication.updated);
    assert_eq!(publication.last_activity_at, 200);

    let stale_heartbeat = store
        .record_visible_viewer_heartbeat(&session.public_id, 150)
        .unwrap();
    assert!(!stale_heartbeat.updated);
    assert_eq!(stale_heartbeat.last_activity_at, 200);

    let heartbeat = store
        .record_visible_viewer_heartbeat(&session.public_id, 250)
        .unwrap();
    assert!(heartbeat.updated);
    assert_eq!(heartbeat.last_activity_at, 250);
}

#[test]
fn activity_for_an_unknown_public_id_is_typed_not_found() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();

    let publication = store
        .record_publication_activity("missing", 100)
        .unwrap_err();
    let heartbeat = store
        .record_visible_viewer_heartbeat("missing", 100)
        .unwrap_err();

    assert!(matches!(
        publication,
        StoreError::SessionNotFound { ref public_id } if public_id == "missing"
    ));
    assert!(matches!(
        heartbeat,
        StoreError::SessionNotFound { ref public_id } if public_id == "missing"
    ));
}

fn attach_blob(connection: &Connection, session_id: i64, post_id: i64, hash: &str) {
    connection
        .execute(
            "INSERT INTO posts (id, session_id, title, commentary) VALUES (?1, ?2, 'Plot', '')",
            rusqlite::params![post_id, session_id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO blob_references (id, post_id, blob_hash) VALUES (?1, ?1, ?2)",
            rusqlite::params![post_id, hash],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO post_files
             (id, post_id, blob_reference_id, position, filename)
             VALUES (?1, ?1, ?1, 0, 'plot.png')",
            [post_id],
        )
        .unwrap();
}

#[test]
fn inclusive_inactivity_purge_selects_only_stale_sessions_and_is_idempotent() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let stale = store
        .resolve_session("pi", "stale", "Stale", "/tmp/stale")
        .unwrap();
    let boundary = store
        .resolve_session("pi", "boundary", "Boundary", "/tmp/boundary")
        .unwrap();
    let active = store
        .resolve_session("pi", "active", "Active", "/tmp/active")
        .unwrap();
    database(&root)
        .execute_batch(&format!(
            "UPDATE sessions SET last_activity_at = 9 WHERE public_id = '{}';
             UPDATE sessions SET last_activity_at = 10 WHERE public_id = '{}';
             UPDATE sessions SET last_activity_at = 11 WHERE public_id = '{}';",
            stale.public_id, boundary.public_id, active.public_id
        ))
        .unwrap();

    let report = store.purge_inactive_sessions(10).unwrap();
    assert_eq!(report.sessions_deleted, 2);
    assert_eq!(report.projects_deleted, 2);
    assert_eq!(report.posts_deleted, 0);
    assert_eq!(report.blob_references_deleted, 0);
    assert_eq!(report.blobs_queued, 0);
    assert_eq!(report.blobs_deleted, 0);
    assert_eq!(
        store.purge_inactive_sessions(10).unwrap().sessions_deleted,
        0
    );

    let remaining = database(&root)
        .query_row("SELECT public_id FROM sessions", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap();
    assert_eq!(remaining, active.public_id);
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn closing_a_projects_final_session_removes_the_project() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "only", "Temporary", "/tmp/temporary")
        .unwrap();

    let report = store.close_session(&session.public_id).unwrap();

    assert_eq!(report.sessions_deleted, 1);
    assert_eq!(report.projects_deleted, 1);
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn closing_one_session_preserves_its_project_while_another_session_remains() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first = store
        .resolve_session("pi", "first", "Shared", "/tmp/shared")
        .unwrap();
    store
        .resolve_session("pi", "second", "Shared", "/tmp/shared")
        .unwrap();

    let report = store.close_session(&first.public_id).unwrap();

    assert_eq!(report.sessions_deleted, 1);
    assert_eq!(report.projects_deleted, 0);
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn close_cascades_metadata_but_preserves_a_blob_shared_by_another_session() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let blob = store.store_blob(Cursor::new(b"shared")).unwrap();
    let first = store
        .resolve_session("pi", "first", "Glim", "/tmp/glim")
        .unwrap();
    let second = store
        .resolve_session("pi", "second", "Glim", "/tmp/glim")
        .unwrap();
    let connection = database(&root);
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    attach_blob(&connection, first.id, 1, blob.hash().as_str());
    attach_blob(&connection, second.id, 2, blob.hash().as_str());
    drop(connection);

    let report = store.close_session(&first.public_id).unwrap();
    assert_eq!(report.sessions_deleted, 1);
    assert_eq!(report.posts_deleted, 1);
    assert_eq!(report.post_files_deleted, 1);
    assert_eq!(report.support_assets_deleted, 0);
    assert_eq!(report.blob_references_deleted, 1);
    assert_eq!(report.blobs_queued, 0);
    assert_eq!(report.blobs_deleted, 0);
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM post_files", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(store.blob_record(blob.hash()).unwrap().is_some());
    assert!(store.open_blob(&blob).is_ok());
}

#[test]
fn closing_the_final_reference_deletes_blob_metadata_and_file_and_repeats_cleanly() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let blob = store.store_blob(Cursor::new(b"final")).unwrap();
    let session = store
        .resolve_session("pi", "only", "Glim", "/tmp/glim")
        .unwrap();
    let connection = database(&root);
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    attach_blob(&connection, session.id, 1, blob.hash().as_str());
    drop(connection);
    let path = root
        .path()
        .join("blobs")
        .join(&blob.hash().as_str()[..2])
        .join(&blob.hash().as_str()[2..4])
        .join(blob.hash().as_str());

    let report = store.close_session(&session.public_id).unwrap();
    assert_eq!(report.blobs_queued, 1);
    assert_eq!(report.blobs_deleted, 1);
    assert!(store.blob_record(blob.hash()).unwrap().is_none());
    assert!(!path.exists());

    let repeated = store.close_session(&session.public_id).unwrap();
    assert_eq!(repeated.sessions_deleted, 0);
    assert_eq!(repeated.blobs_deleted, 0);
}

#[test]
fn deleting_final_reference_releases_budget_for_later_unique_blob() {
    let root = TempDir::new().unwrap();
    let mut original = Store::open(root.path()).unwrap();
    let first = original.store_blob(Cursor::new(b"longer")).unwrap();
    let session = original
        .resolve_session("pi", "release", "Glim", "/tmp/glim")
        .unwrap();
    let connection = database(&root);
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    attach_blob(&connection, session.id, 1, first.hash().as_str());
    drop(connection);
    drop(original);
    let mut store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 6,
            max_finalized_blob_bytes: 5,
        },
    )
    .unwrap();
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        6
    );

    store.close_session(&session.public_id).unwrap();

    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        0
    );
    let later = store.store_blob(Cursor::new(b"later")).unwrap();
    assert_eq!(later.byte_size(), 5);
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        5
    );
}

#[test]
fn blob_reads_do_not_refresh_session_activity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let blob = store.store_blob(Cursor::new(b"background")).unwrap();
    let session = store
        .resolve_session("pi", "background", "Glim", "/tmp/glim")
        .unwrap();
    database(&root)
        .execute(
            "UPDATE sessions SET last_activity_at = 123 WHERE public_id = ?1",
            [&session.public_id],
        )
        .unwrap();

    assert!(store.blob_record(blob.hash()).unwrap().is_some());
    assert!(store.open_blob(&blob).is_ok());

    let activity = database(&root)
        .query_row(
            "SELECT last_activity_at FROM sessions WHERE public_id = ?1",
            [&session.public_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(activity, 123);
}

#[test]
fn startup_retries_queued_deletion_without_scanning_unrelated_final_files() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let blob = store.store_blob(Cursor::new(b"queued")).unwrap();
    let path = root
        .path()
        .join("blobs")
        .join(&blob.hash().as_str()[..2])
        .join(&blob.hash().as_str()[2..4])
        .join(blob.hash().as_str());
    drop(store);
    database(&root)
        .execute(
            "INSERT INTO blob_deletion_queue (blob_hash) VALUES (?1)",
            [blob.hash().as_str()],
        )
        .unwrap();
    let unrelated = root.path().join("blobs/ff/ff/not-a-hash");
    std::fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    std::fs::write(&unrelated, b"untouched").unwrap();

    Store::open(root.path()).unwrap();

    assert!(!path.exists());
    assert_eq!(std::fs::read(unrelated).unwrap(), b"untouched");
    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blob_deletion_queue", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn file_deletion_failure_retains_queue_and_metadata_for_startup_retry() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let blob = store.store_blob(Cursor::new(b"retry")).unwrap();
    let session = store
        .resolve_session("pi", "retry", "Glim", "/tmp/glim")
        .unwrap();
    let connection = database(&root);
    connection
        .execute_batch("PRAGMA foreign_keys = ON")
        .unwrap();
    attach_blob(&connection, session.id, 1, blob.hash().as_str());
    drop(connection);
    let path = root
        .path()
        .join("blobs")
        .join(&blob.hash().as_str()[..2])
        .join(&blob.hash().as_str()[2..4])
        .join(blob.hash().as_str());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();

    assert!(matches!(
        store.close_session(&session.public_id),
        Err(StoreError::Io(_))
    ));
    drop(store);
    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blob_deletion_queue", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(connection);

    std::fs::remove_dir(&path).unwrap();
    Store::open(root.path()).unwrap();
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn concurrent_ingestion_and_final_reference_close_leave_accepted_blob_available() {
    let root = TempDir::new().unwrap();
    for round in 0..12 {
        let bytes = format!("concurrent-{round}").into_bytes();
        let mut lifecycle_store = Store::open(root.path()).unwrap();
        let blob = lifecycle_store
            .store_blob(Cursor::new(bytes.clone()))
            .unwrap();
        let session = lifecycle_store
            .resolve_session("pi", &format!("race-{round}"), "Glim", "/tmp/glim")
            .unwrap();
        let connection = database(&root);
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .unwrap();
        attach_blob(
            &connection,
            session.id,
            i64::from(round) + 1,
            blob.hash().as_str(),
        );
        drop(connection);
        let barrier = Arc::new(Barrier::new(2));
        let writer_barrier = barrier.clone();
        let path = root.path().to_owned();
        let writer = thread::spawn(move || {
            let mut store = Store::open(path).unwrap();
            writer_barrier.wait();
            store.store_blob(Cursor::new(bytes))
        });

        barrier.wait();
        lifecycle_store.close_session(&session.public_id).unwrap();
        let accepted = writer.join().unwrap().unwrap();
        assert!(lifecycle_store.open_blob(&accepted).is_ok());
    }
}
