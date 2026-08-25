use glim::storage::{CURRENT_SCHEMA_VERSION, Store, StoreError};
use rusqlite::{Connection, ErrorCode, params};
use tempfile::TempDir;

fn sqlite_constraint(error: rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(ref failure, _)
            if failure.code == ErrorCode::ConstraintViolation
    )
}

#[test]
fn repeated_resolution_returns_one_stable_session() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();

    let first = store
        .resolve_session("pi", "session-1", "Glimse", "/tmp/glim")
        .unwrap();
    let second = store
        .resolve_session("pi", "session-1", "Glimse", "/tmp/glim")
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(first.public_id.len(), 6);
    assert!(first.public_id.chars().all(|character| {
        "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz".contains(character)
    }));

    drop(store);
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn concurrent_independent_connections_resolve_one_session_without_lock_errors() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));

    let handles = (0..16)
        .map(|_| {
            let database_root = root.path().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(database_root).unwrap();
                (0..10)
                    .map(|round| {
                        barrier.wait();
                        store.resolve_session(
                            "pi",
                            &format!("shared-{round}"),
                            "Glimse",
                            "/tmp/glim",
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let sessions_by_connection = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap()
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect::<Vec<_>>();

    for round in 0..10 {
        assert!(
            sessions_by_connection
                .iter()
                .all(|sessions| sessions[round] == sessions_by_connection[0][round])
        );
    }
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        10
    );
}

#[test]
fn external_keys_are_isolated_by_integration_namespace_and_project() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();

    let pi_glim = store
        .resolve_session("pi", "shared", "Glimse", "/tmp/glim")
        .unwrap();
    let claude_glim = store
        .resolve_session("claude", "shared", "Glimse", "/tmp/glim")
        .unwrap();
    let pi_other = store
        .resolve_session("pi", "shared", "Other", "/tmp/other")
        .unwrap();

    assert_ne!(pi_glim.id, claude_glim.id);
    assert_ne!(pi_glim.id, pi_other.id);
    assert_ne!(pi_glim.project_id, pi_other.project_id);
    assert_ne!(pi_glim.public_id, claude_glim.public_id);
    assert_ne!(pi_glim.public_id, pi_other.public_id);
}

#[test]
fn resolving_existing_working_directory_updates_label_without_changing_identity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first = store
        .resolve_session("pi", "session-1", "Old label", "/tmp/glim")
        .unwrap();

    let resolved = store
        .resolve_session("pi", "session-1", "Current label", "/tmp/glim")
        .unwrap();

    assert_eq!(resolved, first);
    drop(store);
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    let project = connection
        .query_row(
            "SELECT id, label FROM projects WHERE working_directory = '/tmp/glim'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .unwrap();
    assert_eq!(project, (first.project_id, "Current label".to_owned()));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

fn insert_project_and_session(connection: &Connection) {
    connection
        .execute(
            "INSERT INTO projects (id, label, working_directory) VALUES (1, 'Glimse', '/tmp/glim')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions (id, public_id, integration_namespace, external_key, project_id)
             VALUES (1, 'abc', 'pi', 'session-1', 1)",
            [],
        )
        .unwrap();
}

#[test]
fn fresh_store_creates_current_schema_and_enables_required_sqlite_modes() {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(store);

    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );

    let mut tables = connection
        .prepare(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap();
    let names = tables
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        names,
        [
            "blob_deletion_queue",
            "blob_references",
            "blobs",
            "post_files",
            "posts",
            "projects",
            "sessions",
            "support_assets",
        ]
    );
}

#[test]
fn reopening_current_store_preserves_representative_metadata() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    {
        let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        insert_project_and_session(&connection);
        connection
            .execute(
                "INSERT INTO posts (id, session_id, title, commentary) VALUES (1, 1, 'Plot', 'Result')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO blobs (hash, byte_size) VALUES ('digest', 42)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO blob_references (id, post_id, blob_hash) VALUES (1, 1, 'digest')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO post_files
                 (id, post_id, blob_reference_id, position, filename)
                 VALUES (1, 1, 1, 0, 'plot.png')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO blob_references (id, post_id, blob_hash) VALUES (2, 1, 'digest')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO support_assets
                 (id, post_id, entry_file_id, blob_reference_id, relative_path)
                 VALUES (1, 1, 1, 2, 'images/plot.png')",
                [],
            )
            .unwrap();
    }

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(reopened);
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    let count = connection
        .query_row("SELECT COUNT(*) FROM support_assets", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn version_two_posts_migrate_with_a_signed_publication_timestamp() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path()).unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE projects (
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
                 created_at INTEGER NOT NULL DEFAULT 0,
                 last_activity_at INTEGER NOT NULL DEFAULT 0,
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
                 UNIQUE (id, post_id),
                 UNIQUE (post_id, position),
                 FOREIGN KEY (blob_reference_id, post_id)
                     REFERENCES blob_references(id, post_id)
             );
             CREATE TABLE support_assets (
                 id INTEGER PRIMARY KEY,
                 post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                 entry_file_id INTEGER NOT NULL,
                 blob_reference_id INTEGER NOT NULL UNIQUE,
                 relative_path TEXT NOT NULL,
                 UNIQUE (entry_file_id, relative_path),
                 FOREIGN KEY (entry_file_id, post_id)
                     REFERENCES post_files(id, post_id),
                 FOREIGN KEY (blob_reference_id, post_id)
                     REFERENCES blob_references(id, post_id)
             );
             CREATE TABLE blob_deletion_queue (
                 blob_hash TEXT PRIMARY KEY REFERENCES blobs(hash) ON DELETE CASCADE
             );
             INSERT INTO projects (id, label, working_directory)
             VALUES (1, 'Legacy', '/tmp/legacy');
             INSERT INTO sessions
                 (id, public_id, integration_namespace, external_key, project_id,
                  created_at, last_activity_at)
             VALUES (1, 'legacy', 'pi', 'old', 1, 10, 10);
             INSERT INTO posts (id, session_id, title, commentary)
             VALUES (1, 1, 'Legacy', 'Result');
             PRAGMA user_version = 2;",
        )
        .unwrap();
    drop(connection);

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    drop(reopened);
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    let published_at = connection
        .query_row("SELECT published_at FROM posts WHERE id = 1", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert!(published_at > 0);

    for update in [
        "UPDATE posts SET title = 'Changed' WHERE id = 1",
        "UPDATE posts SET published_at = 0 WHERE id = 1",
    ] {
        let error = connection.execute(update, []).unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(_, Some(ref message))
                if message == "posts are immutable"
        ));
    }
}

#[test]
fn populated_version_three_store_backfills_deterministic_support_asset_positions() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path()).unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection
        .execute_batch(include_str!("fixtures/legacy-v3-populated.sql"))
        .unwrap();
    drop(connection);

    let reopened = Store::open(root.path()).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), 4);
    let post = reopened.post(1).unwrap();
    let read_paths = post
        .files
        .iter()
        .map(|file| {
            file.support_assets
                .iter()
                .map(|asset| asset.relative_path.as_str())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        read_paths,
        [
            vec!["a-first.js", "m-middle.js", "z-last.css"],
            vec!["images/a.png", "images/z.png"],
        ]
    );
    drop(reopened);

    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    let migrated_assets = connection
        .prepare(
            "SELECT entry_file_id, id, relative_path, position
             FROM support_assets
             ORDER BY entry_file_id, position",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        migrated_assets,
        [
            (10, 101, "a-first.js".to_owned(), 0),
            (10, 102, "m-middle.js".to_owned(), 1),
            (10, 103, "z-last.css".to_owned(), 2),
            (20, 201, "images/a.png".to_owned(), 0),
            (20, 202, "images/z.png".to_owned(), 1),
        ]
    );
}

#[test]
fn newer_schema_version_returns_typed_incompatibility_error() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path()).unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection
        .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
        .unwrap();
    drop(connection);

    let error = Store::open(root.path()).unwrap_err();
    assert!(matches!(
        error,
        StoreError::SchemaTooNew { found, supported }
            if found == CURRENT_SCHEMA_VERSION + 1 && supported == CURRENT_SCHEMA_VERSION
    ));
}

#[test]
fn deleting_session_cascades_through_revision_linked_posts() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    insert_project_and_session(&connection);
    connection
        .execute(
            "INSERT INTO posts (id, session_id, title, commentary) VALUES (1, 1, 'First', '')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO posts (id, session_id, title, commentary, predecessor_post_id)
             VALUES (2, 1, 'Revision', '', 1)",
            [],
        )
        .unwrap();

    connection
        .execute("DELETE FROM sessions WHERE id = 1", [])
        .unwrap();

    let post_count = connection
        .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(post_count, 0);
}

#[test]
fn rejecting_newer_schema_preserves_journal_mode() {
    let root = TempDir::new().unwrap();
    let database_path = root.path().join("metadata.sqlite3");
    let connection = Connection::open(&database_path).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode = DELETE;")
        .unwrap();
    connection
        .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
        .unwrap();
    drop(connection);

    assert!(matches!(
        Store::open(root.path()),
        Err(StoreError::SchemaTooNew { .. })
    ));

    let connection = Connection::open(database_path).unwrap();
    let journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .unwrap();
    assert_eq!(journal_mode, "delete");
}

#[test]
fn schema_constraints_reject_invalid_identities_and_relationships() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    insert_project_and_session(&connection);

    let duplicate_project = connection
        .execute(
            "INSERT INTO projects (id, label, working_directory)
             VALUES (2, 'Renamed Glimse', '/tmp/glim')",
            [],
        )
        .unwrap_err();
    assert!(sqlite_constraint(duplicate_project));

    let bypassed_session_identity = connection
        .execute(
            "INSERT INTO sessions
             (public_id, integration_namespace, external_key, project_id)
             VALUES ('bypass', 'pi', 'session-1', 2)",
            [],
        )
        .unwrap_err();
    assert!(sqlite_constraint(bypassed_session_identity));

    let duplicate_identity = connection
        .execute(
            "INSERT INTO sessions
             (public_id, integration_namespace, external_key, project_id)
             VALUES ('def', 'pi', 'session-1', 1)",
            [],
        )
        .unwrap_err();
    assert!(sqlite_constraint(duplicate_identity));

    let missing_session = connection
        .execute(
            "INSERT INTO posts (session_id, title, commentary) VALUES (999, 'Plot', 'Result')",
            [],
        )
        .unwrap_err();
    assert!(sqlite_constraint(missing_session));

    connection
        .execute(
            "INSERT INTO sessions (id, public_id, integration_namespace, external_key, project_id)
             VALUES (2, 'def', 'pi', 'session-2', 1)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO posts (id, session_id, title, commentary) VALUES (1, 1, 'First', '')",
            [],
        )
        .unwrap();
    let cross_session_revision = connection
        .execute(
            "INSERT INTO posts (id, session_id, title, commentary, predecessor_post_id)
             VALUES (2, 2, 'Revision', '', 1)",
            [],
        )
        .unwrap_err();
    assert!(sqlite_constraint(cross_session_revision));

    connection
        .execute(
            "INSERT INTO posts (id, session_id, title, commentary, predecessor_post_id)
             VALUES (2, 1, 'Revision', '', 1)",
            [],
        )
        .unwrap();
    let mutation = connection
        .execute(
            "UPDATE posts SET predecessor_post_id = NULL WHERE id = ?1",
            params![2],
        )
        .unwrap_err();
    assert!(sqlite_constraint(mutation));
}
