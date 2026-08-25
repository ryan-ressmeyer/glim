use std::{
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

mod blob;
mod classification;
mod lifecycle;
mod publication;
mod read;

pub use blob::{BlobHash, BlobIntegrityError, BlobRecord, InvalidBlobHash};
pub use classification::ArtifactRenderer;
pub use lifecycle::LifecycleReport;
pub(crate) use publication::PublicationStagingWriter;
pub use publication::{
    PostRecord, PublicationFile, PublicationIdentity, PublicationRequest, PublicationSupportAsset,
    PublishedPublication, StagedPublicationBlob,
};
pub(crate) use read::AssociatedArtifact;
pub use read::{
    BlobRead, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageRequest, PostFileRead, PostPage, PostRead,
    ProjectRead, SessionRead, SupportAssetRead,
};

pub const CURRENT_SCHEMA_VERSION: u32 = 5;
const DATABASE_FILENAME: &str = "metadata.sqlite3";
const PUBLIC_ID_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const INITIAL_PUBLIC_ID_LENGTH: usize = 6;
const PUBLIC_ID_ACCEPTANCE_BOUND: usize = (u8::MAX as usize + 1) / 58 * 58;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const MIGRATIONS: &[Migration] = &[
    Migration {
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
    },
    Migration {
        version: 2,
        sql: r#"
            ALTER TABLE sessions ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE sessions ADD COLUMN last_activity_at INTEGER NOT NULL DEFAULT 0;
            UPDATE sessions
            SET created_at = CAST(strftime('%s', 'now') AS INTEGER),
                last_activity_at = CAST(strftime('%s', 'now') AS INTEGER);

            CREATE TABLE blob_deletion_queue (
                blob_hash TEXT PRIMARY KEY REFERENCES blobs(hash) ON DELETE CASCADE
            );
        "#,
    },
    Migration {
        version: 3,
        sql: r#"
            DROP TRIGGER posts_are_immutable;

            ALTER TABLE posts ADD COLUMN published_at INTEGER NOT NULL DEFAULT 0;
            UPDATE posts
            SET published_at = CAST(strftime('%s', 'now') AS INTEGER);

            CREATE TRIGGER posts_are_immutable
            BEFORE UPDATE ON posts
            BEGIN
                SELECT RAISE(ABORT, 'posts are immutable');
            END;
        "#,
    },
    Migration {
        version: 4,
        sql: r#"
            ALTER TABLE support_assets ADD COLUMN position INTEGER NOT NULL DEFAULT 0 CHECK (position >= 0);
            UPDATE support_assets AS current
            SET position = (
                SELECT COUNT(*) FROM support_assets AS prior
                WHERE prior.entry_file_id = current.entry_file_id
                  AND (prior.relative_path < current.relative_path
                       OR (prior.relative_path = current.relative_path AND prior.id < current.id))
            );
            CREATE UNIQUE INDEX support_assets_entry_position
            ON support_assets(entry_file_id, position);
        "#,
    },
    Migration {
        version: 5,
        sql: r#"
            ALTER TABLE post_files ADD COLUMN media_type TEXT NOT NULL DEFAULT 'application/octet-stream';
            ALTER TABLE post_files ADD COLUMN renderer TEXT NOT NULL DEFAULT 'download'
                CHECK (renderer IN ('image','svg','pdf','video','audio','markdown','text','json','csv','html','download'));
        "#,
    },
];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalUsage {
    pub finalized_unique_blob_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActivityReport {
    pub updated: bool,
    pub last_activity_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimits {
    pub max_upload_bytes: u64,
    pub max_finalized_blob_bytes: u64,
}

impl StoreLimits {
    pub const fn unlimited() -> Self {
        Self {
            max_upload_bytes: u64::MAX,
            max_finalized_blob_bytes: u64::MAX,
        }
    }
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[derive(Debug)]
pub struct Store {
    connection: Connection,
    root: PathBuf,
    limits: StoreLimits,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_limits(root, StoreLimits::unlimited())
    }

    pub fn open_with_limits(
        root: impl AsRef<Path>,
        limits: StoreLimits,
    ) -> Result<Self, StoreError> {
        let root = root.as_ref().to_owned();
        fs::create_dir_all(&root)?;
        let mut connection = Connection::open(root.join(DATABASE_FILENAME))?;
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
        publication::recover_publication_staging(&connection, &root)?;
        blob::recover_blob_store(&connection, &root, limits.max_finalized_blob_bytes)?;
        blob::drain_blob_deletion_queue(&connection, &root)?;

        Ok(Self {
            connection,
            root,
            limits,
        })
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
        let created_at = unix_seconds_now()?;
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
                 (public_id, integration_namespace, external_key, project_id, created_at, last_activity_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(public_id) DO NOTHING",
                params![
                    public_id,
                    integration_namespace,
                    external_key,
                    project_id,
                    created_at
                ],
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

    pub fn record_publication_activity(
        &mut self,
        public_id: &str,
        occurred_at: i64,
    ) -> Result<ActivityReport, StoreError> {
        self.record_activity(public_id, occurred_at)
    }

    pub fn record_visible_viewer_heartbeat(
        &mut self,
        public_id: &str,
        occurred_at: i64,
    ) -> Result<ActivityReport, StoreError> {
        self.record_activity(public_id, occurred_at)
    }

    pub fn record_visible_viewer_heartbeat_now(
        &mut self,
        public_id: &str,
    ) -> Result<ActivityReport, StoreError> {
        self.record_activity(public_id, unix_seconds_now()?)
    }

    fn record_activity(
        &mut self,
        public_id: &str,
        occurred_at: i64,
    ) -> Result<ActivityReport, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous = transaction
            .query_row(
                "SELECT last_activity_at FROM sessions WHERE public_id = ?1",
                [public_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::SessionNotFound {
                public_id: public_id.to_owned(),
            })?;
        let last_activity_at = previous.max(occurred_at);
        if last_activity_at != previous {
            transaction.execute(
                "UPDATE sessions SET last_activity_at = ?2 WHERE public_id = ?1",
                params![public_id, last_activity_at],
            )?;
        }
        transaction.commit()?;
        Ok(ActivityReport {
            updated: last_activity_at != previous,
            last_activity_at,
        })
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

fn unix_seconds_now() -> Result<i64, StoreError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| StoreError::Io(std::io::Error::other(error)))?;
    i64::try_from(duration.as_secs()).map_err(|_| {
        StoreError::Io(std::io::Error::other(
            "system time exceeds signed Unix seconds",
        ))
    })
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
    Integrity(BlobIntegrityError),
    SchemaTooNew {
        found: u32,
        supported: u32,
    },
    InvalidMigrationPlan {
        expected: u32,
        found: u32,
    },
    Random(getrandom::Error),
    PublicIdExhausted,
    SessionNotFound {
        public_id: String,
    },
    ProjectNotFound {
        project_id: i64,
    },
    PostNotFound {
        post_id: i64,
    },
    InvalidPageLimit {
        limit: u32,
        maximum: u32,
    },
    InvalidPageCursor,
    UploadLimitExceeded {
        limit: u64,
        attempted: u64,
    },
    GlobalBlobBudgetExceeded {
        limit: u64,
        current: u64,
        additional: u64,
    },
    BlankPublicationTitle,
    BlankPublicationCommentary,
    PublicationRequiresFile,
    PredecessorNotFound {
        post_id: i64,
    },
    CrossSessionPredecessor {
        post_id: i64,
    },
    DuplicateSupportPath {
        relative_path: String,
    },
    InvalidSupportPath {
        relative_path: String,
    },
    PublicationStagingStoreMismatch,
    ArtifactClassificationFailed,
    ArtifactNotFound,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "storage I/O error: {error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite store error: {error}"),
            Self::Integrity(error) => write!(formatter, "blob integrity error: {error}"),
            Self::SchemaTooNew { found, supported } => write!(
                formatter,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self::InvalidMigrationPlan { expected, found } => write!(
                formatter,
                "invalid migration plan: expected version {expected}, found {found}"
            ),
            Self::Random(error) => {
                write!(formatter, "operating-system randomness error: {error}")
            }
            Self::PublicIdExhausted => formatter.write_str("public session ID length exhausted"),
            Self::SessionNotFound { public_id } => {
                write!(formatter, "session {public_id} was not found")
            }
            Self::ProjectNotFound { project_id } => {
                write!(formatter, "project {project_id} was not found")
            }
            Self::PostNotFound { post_id } => write!(formatter, "post {post_id} was not found"),
            Self::InvalidPageLimit { limit, maximum } => {
                write!(formatter, "page limit {limit} is outside 1..={maximum}")
            }
            Self::InvalidPageCursor => formatter.write_str("invalid page cursor"),
            Self::UploadLimitExceeded { limit, attempted } => write!(
                formatter,
                "upload byte limit exceeded: limit {limit}, attempted {attempted}"
            ),
            Self::GlobalBlobBudgetExceeded {
                limit,
                current,
                additional,
            } => write!(
                formatter,
                "global finalized blob budget exceeded: limit {limit}, current usage {current}, additional unique bytes {additional}"
            ),
            Self::BlankPublicationTitle => {
                formatter.write_str("publication title must not be blank")
            }
            Self::BlankPublicationCommentary => {
                formatter.write_str("publication commentary must not be blank")
            }
            Self::PublicationRequiresFile => {
                formatter.write_str("publication requires at least one visible file")
            }
            Self::PredecessorNotFound { post_id } => {
                write!(formatter, "predecessor post {post_id} was not found")
            }
            Self::CrossSessionPredecessor { post_id } => {
                write!(
                    formatter,
                    "predecessor post {post_id} belongs to another session"
                )
            }
            Self::DuplicateSupportPath { relative_path } => {
                write!(
                    formatter,
                    "duplicate support path under one entry: {relative_path}"
                )
            }
            Self::InvalidSupportPath { relative_path } => {
                write!(formatter, "invalid relative support path: {relative_path}")
            }
            Self::PublicationStagingStoreMismatch => {
                formatter.write_str("staged publication blob belongs to another store")
            }
            Self::ArtifactClassificationFailed => {
                formatter.write_str("artifact media declaration contradicts its filename or bytes")
            }
            Self::ArtifactNotFound => formatter.write_str("associated artifact was not found"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Integrity(error) => Some(error),
            Self::SchemaTooNew { .. }
            | Self::InvalidMigrationPlan { .. }
            | Self::Random(_)
            | Self::PublicIdExhausted
            | Self::SessionNotFound { .. }
            | Self::ProjectNotFound { .. }
            | Self::PostNotFound { .. }
            | Self::InvalidPageLimit { .. }
            | Self::InvalidPageCursor
            | Self::UploadLimitExceeded { .. }
            | Self::GlobalBlobBudgetExceeded { .. }
            | Self::BlankPublicationTitle
            | Self::BlankPublicationCommentary
            | Self::PublicationRequiresFile
            | Self::PredecessorNotFound { .. }
            | Self::CrossSessionPredecessor { .. }
            | Self::DuplicateSupportPath { .. }
            | Self::InvalidSupportPath { .. }
            | Self::PublicationStagingStoreMismatch
            | Self::ArtifactClassificationFailed
            | Self::ArtifactNotFound => None,
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
    use super::{
        MIGRATIONS, Migration, Store, StoreError, apply_migration, apply_migrations,
        generate_public_id_with_fill,
    };
    use rusqlite::Connection;

    #[test]
    fn io_error_message_is_context_neutral() {
        let error = StoreError::Io(std::io::Error::other("disk unavailable"));

        assert_eq!(error.to_string(), "storage I/O error: disk unavailable");
    }

    #[test]
    fn randomness_error_message_is_context_neutral() {
        let source = getrandom::Error::UNSUPPORTED;
        let error = StoreError::Random(source);

        assert_eq!(
            error.to_string(),
            format!("operating-system randomness error: {source}")
        );
    }

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
                expected: 5,
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
    fn failed_publication_timestamp_migration_restores_the_v2_trigger_and_version() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE posts (
                     id INTEGER PRIMARY KEY,
                     title TEXT NOT NULL,
                     published_at INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TRIGGER posts_are_immutable
                 BEFORE UPDATE ON posts
                 BEGIN
                     SELECT RAISE(ABORT, 'posts are immutable');
                 END;
                 INSERT INTO posts (id, title) VALUES (1, 'Legacy');
                 PRAGMA user_version = 2;",
            )
            .unwrap();

        let error = apply_migration(connection.transaction().unwrap(), MIGRATIONS[2]).unwrap_err();

        assert!(error.to_string().contains("duplicate column name"));
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            2
        );
        let immutable = connection
            .execute("UPDATE posts SET title = 'Changed' WHERE id = 1", [])
            .unwrap_err();
        assert!(matches!(
            immutable,
            rusqlite::Error::SqliteFailure(_, Some(ref message))
                if message == "posts are immutable"
        ));
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
