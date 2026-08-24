use std::{error::Error, fmt, fs, path::Path, time::Duration};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
const DATABASE_FILENAME: &str = "metadata.sqlite3";
const PUBLIC_ID_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const INITIAL_PUBLIC_ID_LENGTH: usize = 6;
const PUBLIC_ID_ACCEPTANCE_BOUND: usize = (u8::MAX as usize + 1) / 58 * 58;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: i64,
    pub public_id: String,
    pub project_id: i64,
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
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;

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

    /// Resolves one session identity and allocates new public IDs from the
    /// Bitcoin Base58 alphabet
    /// (`123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz`), beginning at six characters.
    ///
    /// The working directory is the project identity. A later label for that
    /// directory replaces the stored label without replacing its project or sessions.
    pub fn resolve_session(
        &mut self,
        integration_namespace: &str,
        external_key: &str,
        project_label: &str,
        working_directory: &str,
    ) -> Result<SessionRecord, StoreError> {
        self.resolve_session_with_candidates(
            integration_namespace,
            external_key,
            project_label,
            working_directory,
            generate_public_id,
        )
    }

    fn resolve_session_with_candidates<F>(
        &mut self,
        integration_namespace: &str,
        external_key: &str,
        project_label: &str,
        working_directory: &str,
        mut candidate: F,
    ) -> Result<SessionRecord, StoreError>
    where
        F: FnMut(usize) -> Result<String, StoreError>,
    {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO projects (label, working_directory) VALUES (?1, ?2)
             ON CONFLICT(working_directory) DO UPDATE SET label = excluded.label",
            params![project_label, working_directory],
        )?;
        let project_id = transaction.query_row(
            "SELECT id FROM projects WHERE working_directory = ?1",
            [working_directory],
            |row| row.get::<_, i64>(0),
        )?;

        if let Some(session) = find_session(
            &transaction,
            integration_namespace,
            external_key,
            project_id,
        )? {
            transaction.commit()?;
            return Ok(session);
        }

        let mut length = INITIAL_PUBLIC_ID_LENGTH;
        let session = loop {
            let public_id = candidate(length)?;
            let inserted = transaction.execute(
                "INSERT INTO sessions
                 (public_id, integration_namespace, external_key, project_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(public_id) DO NOTHING",
                params![public_id, integration_namespace, external_key, project_id],
            )?;
            if inserted == 1 {
                break SessionRecord {
                    id: transaction.last_insert_rowid(),
                    public_id,
                    project_id,
                };
            }
            length = length.checked_add(1).ok_or(StoreError::PublicIdExhausted)?;
        };
        transaction.commit()?;
        Ok(session)
    }
}

fn find_session(
    transaction: &Transaction<'_>,
    integration_namespace: &str,
    external_key: &str,
    project_id: i64,
) -> rusqlite::Result<Option<SessionRecord>> {
    transaction
        .query_row(
            "SELECT id, public_id, project_id FROM sessions
             WHERE integration_namespace = ?1 AND external_key = ?2 AND project_id = ?3",
            params![integration_namespace, external_key, project_id],
            |row| {
                Ok(SessionRecord {
                    id: row.get(0)?,
                    public_id: row.get(1)?,
                    project_id: row.get(2)?,
                })
            },
        )
        .optional()
}

fn generate_public_id(length: usize) -> Result<String, StoreError> {
    generate_public_id_with_fill(length, |random_bytes| {
        getrandom::fill(random_bytes).map_err(StoreError::Random)
    })
}

fn generate_public_id_with_fill<F>(length: usize, mut fill: F) -> Result<String, StoreError>
where
    F: FnMut(&mut [u8]) -> Result<(), StoreError>,
{
    let mut public_id = String::with_capacity(length);
    let mut random_bytes = vec![0; length];
    while public_id.len() < length {
        random_bytes.resize(length - public_id.len(), 0);
        fill(&mut random_bytes)?;
        for byte in random_bytes.iter().copied() {
            if usize::from(byte) < PUBLIC_ID_ACCEPTANCE_BOUND {
                public_id
                    .push(PUBLIC_ID_ALPHABET[usize::from(byte) % PUBLIC_ID_ALPHABET.len()] as char);
            }
        }
    }
    Ok(public_id)
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    SchemaTooNew { found: u32, supported: u32 },
    InvalidMigrationPlan { expected: u32, found: u32 },
    Random(getrandom::Error),
    PublicIdExhausted,
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
            Self::Random(error) => {
                write!(formatter, "failed to allocate public session ID: {error}")
            }
            Self::PublicIdExhausted => formatter.write_str("public session ID length exhausted"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::SchemaTooNew { .. }
            | Self::InvalidMigrationPlan { .. }
            | Self::Random(_)
            | Self::PublicIdExhausted => None,
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
    use super::{Migration, Store, StoreError, apply_migrations, generate_public_id_with_fill};
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
    fn public_id_generation_rejects_biased_bytes_and_refills_to_requested_length() {
        let mut bytes = [232, 255, 0, 57, 58, 231].into_iter();
        let mut fill_count = 0;

        let public_id = generate_public_id_with_fill(4, |buffer| {
            fill_count += 1;
            for byte in buffer {
                *byte = bytes.next().unwrap();
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(public_id, "1z1z");
        assert_eq!(public_id.len(), 4);
        assert_eq!(fill_count, 2);
        assert_eq!(bytes.next(), None);
    }

    #[test]
    fn public_id_collision_increases_the_next_candidate_length() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path()).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO projects (id, label, working_directory)
                 VALUES (1, 'Existing', '/tmp/existing')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO sessions
                 (public_id, integration_namespace, external_key, project_id)
                 VALUES ('111111', 'pi', 'existing', 1)",
                [],
            )
            .unwrap();
        let mut requested_lengths = Vec::new();

        let session = store
            .resolve_session_with_candidates("pi", "new", "Glimse", "/tmp/glim", |length| {
                requested_lengths.push(length);
                Ok(if length == 6 {
                    "111111".to_owned()
                } else {
                    "2222222".to_owned()
                })
            })
            .unwrap();

        assert_eq!(requested_lengths, [6, 7]);
        assert_eq!(session.public_id, "2222222");
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
