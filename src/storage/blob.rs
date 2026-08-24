use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::{Store, StoreError};

const BLOB_DIRECTORY: &str = "blobs";
const STAGING_DIRECTORY: &str = "staging";
const STREAM_BUFFER_SIZE: usize = 64 * 1024;
const SHA256_HEX_LENGTH: usize = 64;

/// A validated, canonical lowercase SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobHash(String);

impl BlobHash {
    pub fn parse(value: &str) -> Result<Self, InvalidBlobHash> {
        if value.len() == SHA256_HEX_LENGTH
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidBlobHash)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBlobHash;

impl fmt::Display for InvalidBlobHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("blob hash must be 64 lowercase hexadecimal SHA-256 characters")
    }
}

impl Error for InvalidBlobHash {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRecord {
    hash: BlobHash,
    byte_size: u64,
}

impl BlobRecord {
    pub fn hash(&self) -> &BlobHash {
        &self.hash
    }

    pub fn byte_size(&self) -> u64 {
        self.byte_size
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobIntegrityError {
    Missing {
        hash: BlobHash,
    },
    MissingMetadata {
        hash: BlobHash,
    },
    SizeMismatch {
        hash: BlobHash,
        recorded: u64,
        actual: u64,
    },
    HashMismatch {
        expected: BlobHash,
        actual: BlobHash,
    },
    NotAFile {
        hash: BlobHash,
    },
}

impl fmt::Display for BlobIntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { hash } => write!(formatter, "recorded blob {hash} is missing"),
            Self::MissingMetadata { hash } => {
                write!(formatter, "blob {hash} has no metadata record")
            }
            Self::SizeMismatch {
                hash,
                recorded,
                actual,
            } => write!(
                formatter,
                "blob {hash} size disagrees with metadata: recorded {recorded}, actual {actual}"
            ),
            Self::HashMismatch { expected, actual } => write!(
                formatter,
                "blob content hash disagrees with its path: expected {expected}, actual {actual}"
            ),
            Self::NotAFile { hash } => write!(formatter, "blob {hash} is not a regular file"),
        }
    }
}

impl Error for BlobIntegrityError {}

impl Store {
    /// Streams a source through a fixed 64 KiB buffer while computing its
    /// SHA-256 digest and exact byte count. Digests use canonical lowercase hex.
    /// Completed files use `blobs/HH/HH/<sha256>` beneath the store root.
    ///
    /// The staging file and directory are synced first. An immediate SQLite
    /// transaction prepares metadata before an atomic create-if-absent hard link
    /// installs the final file, and commits before the staging journal is removed.
    /// Startup recovery examines only unlocked staging journals.
    pub fn store_blob(&mut self, mut source: impl Read) -> Result<BlobRecord, StoreError> {
        let mut staged = StagedBlob::create(&self.root)?;
        let mut hasher = Sha256::new();
        let mut byte_size = 0_u64;
        let mut buffer = [0_u8; STREAM_BUFFER_SIZE];

        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            staged.file_mut().write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            byte_size = byte_size
                .checked_add(u64::try_from(read).expect("usize read count exceeds u64"))
                .ok_or_else(|| io::Error::other("blob byte count overflow"))?;
        }
        staged.sync_journal()?;

        let hash = BlobHash(format!("{:x}", hasher.finalize()));
        let record = finalize_blob(
            &self.connection,
            &self.root,
            &hash,
            byte_size,
            staged.data_path(),
        )?;
        staged.cleanup()?;
        Ok(record)
    }

    pub fn blob_record(&self, hash: &BlobHash) -> Result<Option<BlobRecord>, StoreError> {
        self.connection
            .query_row(
                "SELECT byte_size FROM blobs WHERE hash = ?1",
                [hash.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|byte_size| {
                u64::try_from(byte_size)
                    .map(|byte_size| BlobRecord {
                        hash: hash.clone(),
                        byte_size,
                    })
                    .map_err(|_| {
                        StoreError::Integrity(BlobIntegrityError::SizeMismatch {
                            hash: hash.clone(),
                            recorded: 0,
                            actual: 0,
                        })
                    })
            })
            .transpose()
    }

    pub fn open_blob(&self, blob: &BlobRecord) -> Result<File, StoreError> {
        let recorded = self.blob_record(&blob.hash)?.ok_or_else(|| {
            StoreError::Integrity(BlobIntegrityError::MissingMetadata {
                hash: blob.hash.clone(),
            })
        })?;
        if recorded.byte_size != blob.byte_size {
            return Err(StoreError::Integrity(BlobIntegrityError::SizeMismatch {
                hash: blob.hash.clone(),
                recorded: recorded.byte_size,
                actual: blob.byte_size,
            }));
        }

        let path = blob_path(&self.root, &blob.hash);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::Integrity(BlobIntegrityError::Missing {
                    hash: blob.hash.clone(),
                }));
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(StoreError::Integrity(BlobIntegrityError::NotAFile {
                hash: blob.hash.clone(),
            }));
        }
        if metadata.len() != recorded.byte_size {
            return Err(StoreError::Integrity(BlobIntegrityError::SizeMismatch {
                hash: blob.hash.clone(),
                recorded: recorded.byte_size,
                actual: metadata.len(),
            }));
        }
        Ok(file)
    }
}

struct StagedBlob {
    staging_directory: PathBuf,
    lock_path: PathBuf,
    data_path: PathBuf,
    lock_file: File,
    data_file: Option<File>,
    cleaned: bool,
}

impl StagedBlob {
    fn create(root: &Path) -> Result<Self, StoreError> {
        let staging = root.join(BLOB_DIRECTORY).join(STAGING_DIRECTORY);
        fs::create_dir_all(&staging)?;
        for _ in 0..16 {
            let token = random_hex_token()?;
            let lock_path = staging.join(format!("{token}.lock"));
            let data_path = staging.join(format!("{token}.part"));
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
                staging_directory: staging,
                lock_path,
                data_path,
                lock_file,
                data_file: Some(data_file),
                cleaned: false,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate unique blob staging file",
        )
        .into())
    }

    fn file_mut(&mut self) -> &mut File {
        self.data_file
            .as_mut()
            .expect("staged file is still active")
    }

    fn data_path(&self) -> &Path {
        &self.data_path
    }

    fn sync_journal(&mut self) -> Result<(), StoreError> {
        self.file_mut().sync_all()?;
        sync_directory(&self.staging_directory)?;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), StoreError> {
        self.data_file.take();
        remove_if_exists(&self.data_path)?;
        remove_if_exists(&self.lock_path)?;
        sync_directory(&self.staging_directory)?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for StagedBlob {
    fn drop(&mut self) {
        if !self.cleaned {
            self.data_file.take();
            let _ = fs::remove_file(&self.data_path);
            let _ = fs::remove_file(&self.lock_path);
            let _ = sync_directory(&self.staging_directory);
        }
        let _ = &self.lock_file;
    }
}

fn random_hex_token() -> Result<String, StoreError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(StoreError::Random)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        write!(token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

fn blob_path(root: &Path, hash: &BlobHash) -> PathBuf {
    root.join(BLOB_DIRECTORY)
        .join(&hash.as_str()[..2])
        .join(&hash.as_str()[2..4])
        .join(hash.as_str())
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn verify_file_size(path: &Path, hash: &BlobHash, expected: u64) -> Result<(), StoreError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(StoreError::Integrity(BlobIntegrityError::NotAFile {
            hash: hash.clone(),
        }));
    }
    if metadata.len() != expected {
        return Err(StoreError::Integrity(BlobIntegrityError::SizeMismatch {
            hash: hash.clone(),
            recorded: expected,
            actual: metadata.len(),
        }));
    }
    let actual_hash = hash_file(path)?;
    if &actual_hash != hash {
        return Err(StoreError::Integrity(BlobIntegrityError::HashMismatch {
            expected: hash.clone(),
            actual: actual_hash,
        }));
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<BlobHash, StoreError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; STREAM_BUFFER_SIZE];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(BlobHash(format!("{:x}", hasher.finalize())))
}

fn finalize_blob(
    connection: &Connection,
    root: &Path,
    hash: &BlobHash,
    byte_size: u64,
    staged_path: &Path,
) -> Result<BlobRecord, StoreError> {
    connection.execute_batch("BEGIN IMMEDIATE")?;
    let final_path = blob_path(root, hash);
    let final_parent = final_path.parent().expect("blob path always has a parent");
    let mut installed_new = false;

    let result = (|| {
        let record = insert_and_verify_blob_metadata(connection, hash, byte_size)?;
        fs::create_dir_all(final_parent)?;
        match fs::hard_link(staged_path, &final_path) {
            Ok(()) => {
                installed_new = true;
                sync_directory(final_parent)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                verify_file_size(&final_path, hash, byte_size)?;
            }
            Err(error) => return Err(error.into()),
        }
        connection.execute_batch("COMMIT")?;
        Ok(record)
    })();

    match result {
        Ok(record) => Ok(record),
        Err(error) => {
            abort_finalization(
                connection,
                hash,
                byte_size,
                installed_new.then_some((&final_path, final_parent)),
            )?;
            Err(error)
        }
    }
}

fn abort_finalization(
    connection: &Connection,
    hash: &BlobHash,
    byte_size: u64,
    installed: Option<(&Path, &Path)>,
) -> Result<(), StoreError> {
    let transaction_was_active = !connection.is_autocommit();
    if !transaction_was_active {
        connection.execute_batch("BEGIN IMMEDIATE")?;
    }

    let cleanup: Result<(), StoreError> = (|| {
        let accepted_after_commit =
            !transaction_was_active && blob_metadata_matches(connection, hash, byte_size)?;
        if let Some((final_path, final_parent)) = installed
            && !accepted_after_commit
        {
            remove_if_exists(final_path)?;
            sync_directory(final_parent)?;
        }
        Ok(())
    })();
    let rollback = connection
        .execute_batch("ROLLBACK")
        .map_err(StoreError::from);
    cleanup?;
    rollback
}

fn insert_and_verify_blob_metadata(
    connection: &Connection,
    hash: &BlobHash,
    byte_size: u64,
) -> Result<BlobRecord, StoreError> {
    connection.execute(
        "INSERT INTO blobs (hash, byte_size) VALUES (?1, ?2)
         ON CONFLICT(hash) DO NOTHING",
        params![hash.as_str(), sqlite_byte_size(byte_size)?],
    )?;
    let recorded = connection.query_row(
        "SELECT byte_size FROM blobs WHERE hash = ?1",
        [hash.as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    let recorded = u64::try_from(recorded).map_err(|_| {
        StoreError::Integrity(BlobIntegrityError::SizeMismatch {
            hash: hash.clone(),
            recorded: 0,
            actual: byte_size,
        })
    })?;
    if recorded != byte_size {
        return Err(StoreError::Integrity(BlobIntegrityError::SizeMismatch {
            hash: hash.clone(),
            recorded,
            actual: byte_size,
        }));
    }
    Ok(BlobRecord {
        hash: hash.clone(),
        byte_size,
    })
}

fn blob_metadata_matches(
    connection: &Connection,
    hash: &BlobHash,
    byte_size: u64,
) -> Result<bool, StoreError> {
    let recorded = connection
        .query_row(
            "SELECT byte_size FROM blobs WHERE hash = ?1",
            [hash.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    Ok(recorded == i64::try_from(byte_size).ok())
}

pub(super) fn recover_blob_store(connection: &Connection, root: &Path) -> Result<(), StoreError> {
    let staging = root.join(BLOB_DIRECTORY).join(STAGING_DIRECTORY);
    fs::create_dir_all(&staging)?;
    recover_staging(connection, root, &staging)
}

fn recover_staging(connection: &Connection, root: &Path, staging: &Path) -> Result<(), StoreError> {
    for entry in fs::read_dir(staging)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("part") {
            continue;
        }
        let lock_path = path.with_extension("lock");
        let lock_file = match OpenOptions::new().write(true).open(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                remove_if_exists(&path)?;
                sync_directory(staging)?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        match lock_file.try_lock() {
            Ok(()) => recover_staged_blob(connection, root, staging, &path, &lock_path)?,
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }

    for entry in fs::read_dir(staging)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("lock") {
            continue;
        }
        let part_path = path.with_extension("part");
        if part_path.exists() {
            continue;
        }
        let lock_file = OpenOptions::new().write(true).open(&path)?;
        match lock_file.try_lock() {
            Ok(()) => {
                remove_if_exists(&path)?;
                sync_directory(staging)?;
            }
            Err(fs::TryLockError::WouldBlock) => {}
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Ok(())
}

fn recover_staged_blob(
    connection: &Connection,
    root: &Path,
    staging: &Path,
    staged_path: &Path,
    lock_path: &Path,
) -> Result<(), StoreError> {
    let hash = hash_file(staged_path)?;
    let byte_size = fs::metadata(staged_path)?.len();
    let final_path = blob_path(root, &hash);
    if final_path.exists() {
        verify_file_size(&final_path, &hash, byte_size)?;
        connection.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            insert_and_verify_blob_metadata(connection, &hash, byte_size)?;
            connection.execute_batch("COMMIT")?;
            Ok(())
        })();
        if let Err(error) = result {
            if !connection.is_autocommit() {
                let _ = connection.execute_batch("ROLLBACK");
            }
            return Err(error);
        }
    }
    remove_if_exists(staged_path)?;
    remove_if_exists(lock_path)?;
    sync_directory(staging)?;
    Ok(())
}

fn sqlite_byte_size(byte_size: u64) -> Result<i64, StoreError> {
    i64::try_from(byte_size)
        .map_err(|_| io::Error::other("blob exceeds SQLite's maximum byte count").into())
}
