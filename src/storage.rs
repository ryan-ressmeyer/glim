use std::{error::Error, fmt, fs, path::Path};

use rusqlite::{Connection, Transaction};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
const DATABASE_FILENAME: &str = "metadata.sqlite3";

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: r#"
        CREATE TABLE projects (
            id                INTEGER PRIMARY KEY,
            label             TEXT NOT NULL,
            working_directory TEXT NOT NULL UNIQUE
        );

        CREATE TABLE sessions (
            id                    INTEGER PRIMARY KEY,
            public_id             TEXT NOT NULL UNIQUE,
            integration_namespace TEXT NOT NULL,
            external_key          TEXT NOT NULL,
            project_id            INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            UNIQUE (integration_namespace, external_key, project_id)
        );

        CREATE TABLE posts (
            id                  INTEGER PRIMARY KEY,
            session_id          INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            title               TEXT NOT NULL,
            commentary          TEXT NOT NULL,
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
            hash      TEXT PRIMARY KEY,
            byte_size INTEGER NOT NULL CHECK (byte_size >= 0)
        );

        CREATE TABLE blob_references (
            id        INTEGER PRIMARY KEY,
            post_id   INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            blob_hash TEXT NOT NULL REFERENCES blobs(hash),
            UNIQUE (id, post_id)
        );

        CREATE TABLE post_files (
            id                INTEGER PRIMARY KEY,
            post_id           INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            blob_reference_id INTEGER NOT NULL UNIQUE,
            position          INTEGER NOT NULL CHECK (position >= 0),
            filename          TEXT NOT NULL,
            caption           TEXT,
            UNIQUE (id, post_id),
            UNIQUE (post_id, position),
            FOREIGN KEY (blob_reference_id, post_id)
                REFERENCES blob_references(id, post_id)
        );

        CREATE TABLE support_assets (
            id                INTEGER PRIMARY KEY,
            post_id           INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
            entry_file_id     INTEGER NOT NULL,
            blob_reference_id INTEGER NOT NULL UNIQUE,
            relative_path     TEXT NOT NULL,
            UNIQUE (entry_file_id, relative_path),
            FOREIGN KEY (entry_file_id, post_id)
                REFERENCES post_files(id, post_id),
            FOREIGN KEY (blob_reference_id, post_id)
                REFERENCES blob_references(id, post_id)
        );
    "#,
}];

#[derive(Clone, Copy)]
struct Migration {
    version: u32,
    sql: &'static str,
}

#[derive(Debug)]
pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        fs::create_dir_all(root.as_ref())?;
        let mut connection = Connection::open(root.as_ref().join(DATABASE_FILENAME))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;

        let version = schema_version(&connection)?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found: version,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }

        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        apply_migrations(&mut connection, version, MIGRATIONS)?;

        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<u32, StoreError> {
        schema_version(&self.connection).map_err(StoreError::from)
    }
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    SchemaTooNew { found: u32, supported: u32 },
    InvalidMigrationPlan { expected: u32, found: u32 },
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "failed to prepare store directory: {error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite store error: {error}"),
            Self::SchemaTooNew { found, supported } => write!(
                formatter,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self::InvalidMigrationPlan { expected, found } => write!(
                formatter,
                "invalid migration plan: expected version {expected}, found {found}"
            ),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::SchemaTooNew { .. } | Self::InvalidMigrationPlan { .. } => None,
        }
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

fn schema_version(connection: &Connection) -> rusqlite::Result<u32> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn apply_migrations(
    connection: &mut Connection,
    current_version: u32,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    validate_migration_plan(migrations)?;

    for migration in migrations
        .iter()
        .filter(|migration| migration.version > current_version)
    {
        apply_migration(connection.transaction()?, *migration)?;
    }
    Ok(())
}

fn validate_migration_plan(migrations: &[Migration]) -> Result<(), StoreError> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = u32::try_from(index).expect("migration count exceeds u32") + 1;
        if migration.version != expected {
            return Err(StoreError::InvalidMigrationPlan {
                expected,
                found: migration.version,
            });
        }
    }

    let final_version = migrations.last().map_or(0, |migration| migration.version);
    if final_version != CURRENT_SCHEMA_VERSION {
        return Err(StoreError::InvalidMigrationPlan {
            expected: CURRENT_SCHEMA_VERSION,
            found: final_version,
        });
    }

    Ok(())
}

fn apply_migration(transaction: Transaction<'_>, migration: Migration) -> rusqlite::Result<()> {
    transaction.execute_batch(migration.sql)?;
    transaction.pragma_update(None, "user_version", migration.version)?;
    transaction.commit()
}

#[cfg(test)]
mod tests {
    use super::{Migration, Store, StoreError, apply_migrations};
    use rusqlite::Connection;

    #[test]
    fn opened_store_connection_enforces_foreign_keys_and_wal() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).unwrap();

        let foreign_keys = store
            .connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap();
        let journal_mode = store
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap();

        assert_eq!(foreign_keys, 1);
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn invalid_migration_plan_must_end_at_current_schema_version() {
        let mut connection = Connection::open_in_memory().unwrap();

        let error = apply_migrations(&mut connection, 0, &[]).unwrap_err();

        assert!(matches!(
            error,
            StoreError::InvalidMigrationPlan {
                expected: 1,
                found: 0
            }
        ));
    }

    #[test]
    fn invalid_migration_plan_rejects_a_version_gap_without_applying_it() {
        let mut connection = Connection::open_in_memory().unwrap();
        let migrations = [Migration {
            version: 2,
            sql: "CREATE TABLE skipped_version (id INTEGER PRIMARY KEY);",
        }];

        let error = apply_migrations(&mut connection, 0, &migrations).unwrap_err();

        assert!(matches!(
            error,
            StoreError::InvalidMigrationPlan {
                expected: 1,
                found: 2
            }
        ));
        let table_count = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'skipped_version'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(table_count, 0);
    }

    #[test]
    fn invalid_migration_plan_rejects_an_adjacent_duplicate() {
        let mut connection = Connection::open_in_memory().unwrap();
        let migrations = [
            Migration {
                version: 1,
                sql: "CREATE TABLE first (id INTEGER PRIMARY KEY);",
            },
            Migration {
                version: 1,
                sql: "CREATE TABLE duplicate (id INTEGER PRIMARY KEY);",
            },
        ];

        let error = apply_migrations(&mut connection, 0, &migrations).unwrap_err();

        assert!(matches!(
            error,
            StoreError::InvalidMigrationPlan {
                expected: 2,
                found: 1
            }
        ));
        let table_count = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('first', 'duplicate')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(table_count, 0);
    }

    #[test]
    fn failed_migration_rolls_back_its_schema_and_version() {
        let mut connection = Connection::open_in_memory().unwrap();
        let migrations = [Migration {
            version: 1,
            sql: "CREATE TABLE rolled_back (id INTEGER PRIMARY KEY); INVALID SQL;",
        }];

        assert!(apply_migrations(&mut connection, 0, &migrations).is_err());
        let table_count = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'rolled_back'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let version = connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap();

        assert_eq!(table_count, 0);
        assert_eq!(version, 0);
    }
}
