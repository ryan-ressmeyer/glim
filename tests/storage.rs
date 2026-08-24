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
