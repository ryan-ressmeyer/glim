use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::{
    BlobHash, INITIAL_PUBLIC_ID_LENGTH, PostRead, SessionRead, Store, StoreError, blob,
    generate_public_id, unix_seconds_now,
};

const PUBLICATION_STAGING_DIRECTORY: &str = "publication-staging";
const STREAM_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct PublicationStagingWriter {
    root: PathBuf,
    staging_directory: PathBuf,
    lock_path: PathBuf,
    data_path: PathBuf,
    journal_path: PathBuf,
    lock_file: Option<File>,
    data_file: Option<File>,
    hasher: Option<Sha256>,
    byte_size: u64,
    max_upload_bytes: u64,
    cleaned: bool,
}

#[derive(Debug)]
pub struct StagedPublicationBlob {
    root: PathBuf,
    staging_directory: PathBuf,
    lock_path: PathBuf,
    pub(super) data_path: PathBuf,
    journal_path: PathBuf,
    lock_file: File,
    hash: BlobHash,
    byte_size: u64,
    cleaned: bool,
    retain_for_recovery: Cell<bool>,
}

#[derive(Debug)]
pub struct PublicationSupportAsset {
    pub relative_path: String,
    pub blob: StagedPublicationBlob,
}

#[derive(Debug)]
pub struct PublicationFile {
    pub filename: String,
    pub caption: Option<String>,
    pub blob: StagedPublicationBlob,
    pub support_assets: Vec<PublicationSupportAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationIdentity {
    pub integration_namespace: String,
    pub external_key: String,
    pub project_label: String,
    pub working_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitProvenance {
    pub root: String,
    pub branch: Option<String>,
    pub commit: Option<String>,
}

impl GitProvenance {
    pub(crate) fn is_valid(&self) -> bool {
        let commit_valid = self.commit.as_ref().is_none_or(|value| {
            matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        std::path::Path::new(&self.root).is_absolute()
            && !self.root.is_empty()
            && self.root.len() <= 4096
            && !self.root.chars().any(char::is_control)
            && self.branch.as_ref().is_none_or(|value| {
                !value.trim().is_empty()
                    && value.len() <= 1024
                    && !value.chars().any(char::is_control)
            })
            && commit_valid
    }
}

#[derive(Debug)]
pub struct PublicationRequest {
    pub session_public_id: String,
    pub title: String,
    pub commentary: String,
    pub predecessor_post_id: Option<i64>,
    pub git: Option<GitProvenance>,
    pub files: Vec<PublicationFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedPublication {
    pub session: SessionRead,
    pub post: PostRead,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostRecord {
    pub id: i64,
    pub session_id: i64,
    pub session_public_id: String,
    pub predecessor_post_id: Option<i64>,
    pub published_at: i64,
}

impl Store {
    /// Stages one publication source without making it a standalone stored blob.
    /// The returned handle owns a durable, lock-protected journal and removes it
    /// when dropped unless publication has already consumed it.
    pub fn stage_publication_blob(
        &self,
        mut source: impl Read,
    ) -> Result<StagedPublicationBlob, StoreError> {
        let mut writer = self.publication_staging_writer()?;
        let mut buffer = [0_u8; STREAM_BUFFER_SIZE];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_chunk(&buffer[..read])?;
        }
        writer.finish()
    }

    pub(crate) fn publication_staging_writer(
        &self,
    ) -> Result<PublicationStagingWriter, StoreError> {
        PublicationStagingWriter::create(&self.root, self.limits.max_upload_bytes)
    }

    pub fn publish(&mut self, request: PublicationRequest) -> Result<PostRecord, StoreError> {
        self.publish_at(request, unix_seconds_now()?)
    }

    pub fn publish_at(
        &mut self,
        request: PublicationRequest,
        published_at: i64,
    ) -> Result<PostRecord, StoreError> {
        self.publish_internal(None, request, published_at, None)
    }

    pub fn publish_resolving_at(
        &mut self,
        identity: PublicationIdentity,
        request: PublicationRequest,
        published_at: i64,
    ) -> Result<PublishedPublication, StoreError> {
        self.publish_resolving_classified_at(identity, request, published_at, None)
    }

    pub(crate) fn publish_resolving_classified_at(
        &mut self,
        identity: PublicationIdentity,
        request: PublicationRequest,
        published_at: i64,
        declared_media_types: Option<Vec<Option<String>>>,
    ) -> Result<PublishedPublication, StoreError> {
        let record =
            self.publish_internal(Some(identity), request, published_at, declared_media_types)?;
        Ok(PublishedPublication {
            session: self.session(&record.session_public_id)?,
            post: self.post(record.id)?,
        })
    }

    pub fn publish_resolving(
        &mut self,
        identity: PublicationIdentity,
        request: PublicationRequest,
    ) -> Result<PublishedPublication, StoreError> {
        self.publish_resolving_at(identity, request, unix_seconds_now()?)
    }

    fn publish_internal(
        &mut self,
        identity: Option<PublicationIdentity>,
        mut request: PublicationRequest,
        published_at: i64,
        declared_media_types: Option<Vec<Option<String>>>,
    ) -> Result<PostRecord, StoreError> {
        validate_request(&request, &self.root, self.limits.max_upload_bytes)?;
        let classifications = request
            .files
            .iter()
            .enumerate()
            .map(|(position, file)| {
                let declared = declared_media_types
                    .as_ref()
                    .and_then(|values| values.get(position))
                    .and_then(Option::as_deref);
                super::classification::classify_staged(&file.blob, &file.filename, declared)
            })
            .collect::<Result<Vec<_>, _>>()?;

        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        let mut installed = Vec::new();
        let result = (|| {
            let session_id = if let Some(identity) = identity.as_ref() {
                let (session_id, public_id) =
                    resolve_publication_identity(&self.connection, identity, published_at)?;
                request.session_public_id = public_id;
                session_id
            } else {
                self.connection
                    .query_row(
                        "SELECT id FROM sessions WHERE public_id = ?1",
                        [&request.session_public_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::SessionNotFound {
                        public_id: request.session_public_id.clone(),
                    })?
            };

            validate_predecessor(&self.connection, request.predecessor_post_id, session_id)?;

            let occurrences = occurrences(&request);
            let unique = unique_blobs(&occurrences)?;
            let current = blob::finalized_unique_blob_bytes(&self.connection)?;
            let mut additional = 0_u64;
            for staged in unique.values() {
                let recorded = self
                    .connection
                    .query_row(
                        "SELECT byte_size FROM blobs WHERE hash = ?1",
                        [staged.hash.as_str()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                match recorded {
                    Some(recorded) => {
                        let recorded = u64::try_from(recorded).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "negative blob byte count")
                        })?;
                        if recorded != staged.byte_size {
                            return Err(StoreError::Integrity(
                                super::BlobIntegrityError::SizeMismatch {
                                    hash: staged.hash.clone(),
                                    recorded,
                                    actual: staged.byte_size,
                                },
                            ));
                        }
                    }
                    None => {
                        additional = additional.checked_add(staged.byte_size).ok_or_else(|| {
                            io::Error::other("publication blob byte count exceeds u64")
                        })?;
                    }
                }
            }
            if additional > 0
                && current
                    .checked_add(additional)
                    .is_none_or(|total| total > self.limits.max_finalized_blob_bytes)
            {
                return Err(StoreError::GlobalBlobBudgetExceeded {
                    limit: self.limits.max_finalized_blob_bytes,
                    current,
                    additional,
                });
            }

            for staged in unique.values() {
                let existed = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)",
                    [staged.hash.as_str()],
                    |row| row.get::<_, bool>(0),
                )?;
                if !existed {
                    staged.mark_finalizing()?;
                    self.connection.execute(
                        "INSERT INTO blobs (hash, byte_size) VALUES (?1, ?2)",
                        params![
                            staged.hash.as_str(),
                            i64::try_from(staged.byte_size).map_err(|_| {
                                io::Error::other("blob exceeds SQLite's maximum byte count")
                            })?
                        ],
                    )?;
                }
                self.connection.execute(
                    "DELETE FROM blob_deletion_queue WHERE blob_hash = ?1",
                    [staged.hash.as_str()],
                )?;

                let final_path = blob::blob_path(&self.root, &staged.hash);
                let final_parent = final_path
                    .parent()
                    .expect("blob path has a parent")
                    .to_owned();
                fs::create_dir_all(&final_parent)?;
                match fs::hard_link(&staged.data_path, &final_path) {
                    Ok(()) => {
                        installed.push((
                            staged.hash.clone(),
                            final_path.clone(),
                            final_parent.clone(),
                        ));
                        blob::sync_directory(&final_parent)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        blob::verify_file_size(&final_path, &staged.hash, staged.byte_size)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }

            self.connection.execute(
                "INSERT INTO posts
                 (session_id, title, commentary, predecessor_post_id, published_at,
                  git_root, git_branch, git_commit)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    request.title,
                    request.commentary,
                    request.predecessor_post_id,
                    published_at,
                    request.git.as_ref().map(|git| git.root.as_str()),
                    request.git.as_ref().and_then(|git| git.branch.as_deref()),
                    request.git.as_ref().and_then(|git| git.commit.as_deref()),
                ],
            )?;
            let post_id = self.connection.last_insert_rowid();

            for (position, file) in request.files.iter().enumerate() {
                let reference_id = insert_reference(&self.connection, post_id, &file.blob.hash)?;
                let classification = &classifications[position];
                let renderer = serde_json::to_value(classification.renderer)
                    .expect("renderer serializes")
                    .as_str()
                    .expect("renderer is a string")
                    .to_owned();
                self.connection.execute(
                    "INSERT INTO post_files
                     (post_id, blob_reference_id, position, filename, caption, media_type, renderer)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        post_id,
                        reference_id,
                        i64::try_from(position).expect("publication file count exceeds i64"),
                        file.filename,
                        file.caption,
                        classification.media_type,
                        renderer
                    ],
                )?;
                let entry_file_id = self.connection.last_insert_rowid();
                for (position, asset) in file.support_assets.iter().enumerate() {
                    let reference_id =
                        insert_reference(&self.connection, post_id, &asset.blob.hash)?;
                    self.connection.execute(
                        "INSERT INTO support_assets
                         (post_id, entry_file_id, blob_reference_id, relative_path, position)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            post_id,
                            entry_file_id,
                            reference_id,
                            asset.relative_path,
                            i64::try_from(position).expect("support asset count exceeds i64")
                        ],
                    )?;
                }
            }

            self.connection.execute(
                "UPDATE sessions
                 SET last_activity_at = MAX(last_activity_at, ?2)
                 WHERE id = ?1",
                params![session_id, published_at],
            )?;
            self.connection.execute_batch("COMMIT")?;
            Ok(PostRecord {
                id: post_id,
                session_id,
                session_public_id: request.session_public_id.clone(),
                predecessor_post_id: request.predecessor_post_id,
                published_at,
            })
        })();

        let original_error = match result {
            Ok(record) => return Ok(record),
            Err(error) => error,
        };
        if self.connection.is_autocommit() {
            return Err(original_error);
        }

        // Keep BEGIN IMMEDIATE held across unlink and directory sync so no
        // competing blob writer can accept one of these paths during cleanup.
        // Only links installed by this call are candidates for removal.
        let mut cleanup_error = None;
        for (hash, path, parent) in &installed {
            let cleanup = blob::remove_if_exists(path).and_then(|()| blob::sync_directory(parent));
            if let Err(error) = cleanup {
                retain_staged_hash_for_recovery(&request, hash);
                cleanup_error.get_or_insert(error);
            }
        }
        let rollback = self.connection.execute_batch("ROLLBACK");
        if let Err(error) = rollback {
            for (hash, _, _) in &installed {
                retain_staged_hash_for_recovery(&request, hash);
            }
            return Err(error.into());
        }
        match cleanup_error {
            Some(error) => Err(error.into()),
            None => Err(original_error),
        }
    }
}

fn resolve_publication_identity(
    connection: &rusqlite::Connection,
    identity: &PublicationIdentity,
    created_at: i64,
) -> Result<(i64, String), StoreError> {
    connection.execute(
        "INSERT INTO projects (label, working_directory) VALUES (?1, ?2)
         ON CONFLICT(working_directory) DO UPDATE SET label = excluded.label",
        params![identity.project_label, identity.working_directory],
    )?;
    let project_id = connection.query_row(
        "SELECT id FROM projects WHERE working_directory = ?1",
        [&identity.working_directory],
        |row| row.get::<_, i64>(0),
    )?;
    if let Some(found) = connection
        .query_row(
            "SELECT id, public_id FROM sessions
             WHERE integration_namespace = ?1 AND external_key = ?2 AND project_id = ?3",
            params![
                identity.integration_namespace,
                identity.external_key,
                project_id
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        return Ok(found);
    }

    let mut length = INITIAL_PUBLIC_ID_LENGTH;
    loop {
        let public_id = generate_public_id(length)?;
        let inserted = connection.execute(
            "INSERT INTO sessions
             (public_id, integration_namespace, external_key, project_id, created_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(public_id) DO NOTHING",
            params![
                public_id,
                identity.integration_namespace,
                identity.external_key,
                project_id,
                created_at
            ],
        )?;
        if inserted == 1 {
            return Ok((connection.last_insert_rowid(), public_id));
        }
        length = length.checked_add(1).ok_or(StoreError::PublicIdExhausted)?;
    }
}

fn retain_staged_hash_for_recovery(request: &PublicationRequest, hash: &BlobHash) {
    for staged in occurrences(request) {
        if &staged.hash == hash {
            staged.retain_for_recovery.set(true);
        }
    }
}

fn validate_request(
    request: &PublicationRequest,
    root: &Path,
    max_upload_bytes: u64,
) -> Result<(), StoreError> {
    if request.title.trim().is_empty() {
        return Err(StoreError::BlankPublicationTitle);
    }
    if request.commentary.trim().is_empty() {
        return Err(StoreError::BlankPublicationCommentary);
    }
    if request.files.is_empty() {
        return Err(StoreError::PublicationRequiresFile);
    }
    if request.git.as_ref().is_some_and(|git| !git.is_valid()) {
        return Err(StoreError::InvalidGitProvenance);
    }
    for file in &request.files {
        validate_staged_blob(&file.blob, root, max_upload_bytes)?;
        let mut paths = HashSet::new();
        for asset in &file.support_assets {
            validate_staged_blob(&asset.blob, root, max_upload_bytes)?;
            if asset.relative_path.starts_with('/')
                || asset.relative_path.contains('\\')
                || asset.relative_path.chars().any(char::is_control)
                || asset
                    .relative_path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(StoreError::InvalidSupportPath {
                    relative_path: asset.relative_path.clone(),
                });
            }
            if !paths.insert(asset.relative_path.as_str()) {
                return Err(StoreError::DuplicateSupportPath {
                    relative_path: asset.relative_path.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_staged_blob(
    staged: &StagedPublicationBlob,
    root: &Path,
    max_upload_bytes: u64,
) -> Result<(), StoreError> {
    if staged.root != root {
        return Err(StoreError::PublicationStagingStoreMismatch);
    }
    if staged.byte_size > max_upload_bytes {
        return Err(StoreError::UploadLimitExceeded {
            limit: max_upload_bytes,
            attempted: staged.byte_size,
        });
    }
    Ok(())
}

fn validate_predecessor(
    connection: &rusqlite::Connection,
    predecessor: Option<i64>,
    session_id: i64,
) -> Result<(), StoreError> {
    let Some(predecessor) = predecessor else {
        return Ok(());
    };
    let predecessor_session = connection
        .query_row(
            "SELECT session_id FROM posts WHERE id = ?1",
            [predecessor],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    match predecessor_session {
        None => Err(StoreError::PredecessorNotFound {
            post_id: predecessor,
        }),
        Some(found) if found != session_id => Err(StoreError::CrossSessionPredecessor {
            post_id: predecessor,
        }),
        Some(_) => Ok(()),
    }
}

fn occurrences(request: &PublicationRequest) -> Vec<&StagedPublicationBlob> {
    let mut values = Vec::new();
    for file in &request.files {
        values.push(&file.blob);
        values.extend(file.support_assets.iter().map(|asset| &asset.blob));
    }
    values
}

fn unique_blobs<'a>(
    occurrences: &[&'a StagedPublicationBlob],
) -> Result<HashMap<BlobHash, &'a StagedPublicationBlob>, StoreError> {
    let mut unique = HashMap::new();
    for staged in occurrences {
        if let Some(previous) = unique.insert(staged.hash.clone(), *staged)
            && previous.byte_size != staged.byte_size
        {
            return Err(StoreError::Integrity(
                super::BlobIntegrityError::SizeMismatch {
                    hash: staged.hash.clone(),
                    recorded: previous.byte_size,
                    actual: staged.byte_size,
                },
            ));
        }
    }
    Ok(unique)
}

fn insert_reference(
    connection: &rusqlite::Connection,
    post_id: i64,
    hash: &BlobHash,
) -> Result<i64, StoreError> {
    connection.execute(
        "INSERT INTO blob_references (post_id, blob_hash) VALUES (?1, ?2)",
        params![post_id, hash.as_str()],
    )?;
    Ok(connection.last_insert_rowid())
}

impl PublicationStagingWriter {
    fn create(root: &Path, max_upload_bytes: u64) -> Result<Self, StoreError> {
        let staging_directory = root.join("blobs").join(PUBLICATION_STAGING_DIRECTORY);
        fs::create_dir_all(&staging_directory)?;
        loop {
            let token = blob::random_hex_token()?;
            let lock_path = staging_directory.join(format!("{token}.lock"));
            let data_path = staging_directory.join(format!("{token}.part"));
            let journal_path = staging_directory.join(format!("{token}.journal"));
            let lock_file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            };
            lock_file.try_lock().map_err(io::Error::from)?;
            let data_file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&data_path)
            {
                Ok(file) => file,
                Err(error) => {
                    let _ = fs::remove_file(&lock_path);
                    return Err(error.into());
                }
            };
            return Ok(Self {
                root: root.to_owned(),
                staging_directory,
                lock_path,
                data_path,
                journal_path,
                lock_file: Some(lock_file),
                data_file: Some(data_file),
                hasher: Some(Sha256::new()),
                byte_size: 0,
                max_upload_bytes,
                cleaned: false,
            });
        }
    }

    pub(crate) fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        let attempted = self
            .byte_size
            .checked_add(u64::try_from(bytes.len()).expect("chunk size exceeds u64"))
            .ok_or_else(|| io::Error::other("blob byte count overflow"))?;
        if attempted > self.max_upload_bytes {
            return Err(StoreError::UploadLimitExceeded {
                limit: self.max_upload_bytes,
                attempted,
            });
        }
        self.data_file
            .as_mut()
            .expect("writer is active")
            .write_all(bytes)?;
        self.hasher
            .as_mut()
            .expect("writer is active")
            .update(bytes);
        self.byte_size = attempted;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<StagedPublicationBlob, StoreError> {
        self.data_file
            .as_mut()
            .expect("writer is active")
            .sync_all()?;
        let hash = BlobHash::parse(&format!(
            "{:x}",
            self.hasher.take().expect("writer is active").finalize()
        ))
        .expect("SHA-256 formatting is canonical");
        write_journal_state(
            &self.staging_directory,
            &self.journal_path,
            "staged",
            &hash,
            self.byte_size,
        )?;
        self.data_file.take();
        let staged = StagedPublicationBlob {
            root: self.root.clone(),
            staging_directory: self.staging_directory.clone(),
            lock_path: self.lock_path.clone(),
            data_path: self.data_path.clone(),
            journal_path: self.journal_path.clone(),
            lock_file: self.lock_file.take().expect("writer is active"),
            hash,
            byte_size: self.byte_size,
            cleaned: false,
            retain_for_recovery: Cell::new(false),
        };
        self.cleaned = true;
        Ok(staged)
    }

    fn cleanup(&mut self) {
        self.data_file.take();
        let _ = blob::remove_if_exists(&self.data_path);
        let _ = blob::remove_if_exists(&self.journal_path.with_extension("next"));
        let _ = blob::remove_if_exists(&self.journal_path);
        let _ = blob::remove_if_exists(&self.lock_path);
        let _ = blob::sync_directory(&self.staging_directory);
        self.cleaned = true;
    }
}

impl Drop for PublicationStagingWriter {
    fn drop(&mut self) {
        if !self.cleaned {
            self.cleanup();
        }
        let _ = &self.lock_file;
    }
}

impl StagedPublicationBlob {
    fn mark_finalizing(&self) -> Result<(), StoreError> {
        write_journal_state(
            &self.staging_directory,
            &self.journal_path,
            "finalizing",
            &self.hash,
            self.byte_size,
        )
    }

    fn cleanup(&mut self) {
        let _ = blob::remove_if_exists(&self.data_path);
        let _ = blob::remove_if_exists(&self.journal_path.with_extension("next"));
        let _ = blob::remove_if_exists(&self.journal_path);
        let _ = blob::remove_if_exists(&self.lock_path);
        let _ = blob::sync_directory(&self.staging_directory);
        self.cleaned = true;
    }
}

impl Drop for StagedPublicationBlob {
    fn drop(&mut self) {
        if !self.cleaned && !self.retain_for_recovery.get() {
            self.cleanup();
        }
        let _ = &self.lock_file;
    }
}

fn write_journal_state(
    staging_directory: &Path,
    journal_path: &Path,
    state: &str,
    hash: &BlobHash,
    byte_size: u64,
) -> Result<(), StoreError> {
    let next_path = journal_path.with_extension("next");
    let mut next = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&next_path)?;
    writeln!(next, "{state} {} {byte_size}", hash.as_str())?;
    next.sync_all()?;
    fs::rename(&next_path, journal_path)?;
    blob::sync_directory(staging_directory)?;
    Ok(())
}

/// Publication journals describe only paths that a publication may have linked.
/// Before commit, blob metadata is absent after SQLite recovery, so an unlocked
/// `finalizing` journal removes that final path. After commit, metadata remains
/// and recovery retains the blob, then removes the stale stage. No finalized-tree
/// scan is needed, and locked journals protect active writers.
pub(super) fn recover_publication_staging(
    connection: &rusqlite::Connection,
    root: &Path,
) -> Result<(), StoreError> {
    let staging = root.join("blobs").join(PUBLICATION_STAGING_DIRECTORY);
    fs::create_dir_all(&staging)?;
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        for entry in fs::read_dir(&staging)? {
            let journal_path = entry?.path();
            if journal_path.extension().and_then(|value| value.to_str()) != Some("journal") {
                continue;
            }
            let lock_path = journal_path.with_extension("lock");
            let lock_file = match OpenOptions::new().write(true).open(&lock_path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    cleanup_paths(&staging, &journal_path, &lock_path)?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match lock_file.try_lock() {
                Ok(()) => {
                    let journal = fs::read_to_string(&journal_path)?;
                    let mut fields = journal.split_whitespace();
                    let state = fields.next().unwrap_or("");
                    let hash = fields
                        .next()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "invalid publication journal",
                            )
                        })
                        .and_then(|value| {
                            BlobHash::parse(value)
                                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
                        })?;
                    if state == "finalizing" {
                        let tracked = connection.query_row(
                            "SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)",
                            [hash.as_str()],
                            |row| row.get::<_, bool>(0),
                        )?;
                        if !tracked {
                            let final_path = blob::blob_path(root, &hash);
                            blob::remove_if_exists(&final_path)?;
                            if let Some(parent) = final_path.parent() {
                                blob::sync_directory(parent)?;
                            }
                        }
                    }
                    cleanup_paths(&staging, &journal_path, &lock_path)?;
                }
                Err(fs::TryLockError::WouldBlock) => {}
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }

        // A process can stop while streaming, before it has enough information
        // to write a journal. The lock still distinguishes abandoned stages from
        // active writers, so only unlocked pre-journal pairs are removed.
        for entry in fs::read_dir(&staging)? {
            let data_path = entry?.path();
            if data_path.extension().and_then(|value| value.to_str()) != Some("part")
                || data_path.with_extension("journal").exists()
            {
                continue;
            }
            let lock_path = data_path.with_extension("lock");
            let lock_file = match OpenOptions::new().write(true).open(&lock_path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    blob::remove_if_exists(&data_path)?;
                    blob::sync_directory(&staging)?;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match lock_file.try_lock() {
                Ok(()) => {
                    blob::remove_if_exists(&data_path)?;
                    blob::remove_if_exists(&data_path.with_extension("next"))?;
                    blob::remove_if_exists(&lock_path)?;
                    blob::sync_directory(&staging)?;
                }
                Err(fs::TryLockError::WouldBlock) => {}
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        for entry in fs::read_dir(&staging)? {
            let lock_path = entry?.path();
            if lock_path.extension().and_then(|value| value.to_str()) != Some("lock")
                || lock_path.with_extension("part").exists()
                || lock_path.with_extension("journal").exists()
            {
                continue;
            }
            let lock_file = OpenOptions::new().write(true).open(&lock_path)?;
            match lock_file.try_lock() {
                Ok(()) => {
                    blob::remove_if_exists(&lock_path)?;
                    blob::sync_directory(&staging)?;
                }
                Err(fs::TryLockError::WouldBlock) => {}
                Err(fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        connection.execute_batch("COMMIT")?;
        Ok(())
    })();
    if result.is_err() && !connection.is_autocommit() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    result
}

fn cleanup_paths(staging: &Path, journal_path: &Path, lock_path: &Path) -> Result<(), StoreError> {
    blob::remove_if_exists(&journal_path.with_extension("part"))?;
    blob::remove_if_exists(&journal_path.with_extension("next"))?;
    blob::remove_if_exists(journal_path)?;
    blob::remove_if_exists(lock_path)?;
    blob::sync_directory(staging)?;
    Ok(())
}
