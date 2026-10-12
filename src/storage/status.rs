use std::{fs, io, path::Path};

use rusqlite::TransactionBehavior;

use super::{Store, StoreError, StoreLimits};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreStatusSnapshot {
    pub staging_bytes_in_use: u64,
    pub max_staging_bytes: u64,
    pub max_concurrent_publications: u32,
    pub finalized_unique_blob_bytes: u64,
    pub active_sessions: u64,
    pub sessions_due_for_purge: u64,
    pub queued_blob_deletions: u64,
    pub metadata_disk_bytes: u64,
    pub saved_posts: u64,
    pub total_posts: u64,
    pub limits: StoreLimits,
}

impl Store {
    pub fn status_snapshot(&mut self, cutoff: i64) -> Result<StoreStatusSnapshot, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let finalized =
            transaction.query_row("SELECT COALESCE(SUM(byte_size), 0) FROM blobs", [], |row| {
                row.get::<_, i64>(0)
            })?;
        let active = transaction.query_row(
            "SELECT COUNT(*) FROM sessions WHERE closed = 0 AND last_activity_at > ?1",
            [cutoff],
            |row| row.get::<_, i64>(0),
        )?;
        let due = transaction.query_row(
            "SELECT COUNT(*) FROM sessions WHERE last_activity_at <= ?1 AND (closed = 0 OR NOT EXISTS
             (SELECT 1 FROM saved_posts v JOIN posts p ON p.id = v.post_id WHERE p.session_id = sessions.id)
             OR EXISTS (SELECT 1 FROM posts p WHERE p.session_id = sessions.id
                 AND NOT EXISTS (SELECT 1 FROM saved_posts WHERE post_id = p.id)
                 AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = p.id)))",
            [cutoff],
            |row| row.get::<_, i64>(0),
        )?;
        let queued =
            transaction.query_row("SELECT COUNT(*) FROM blob_deletion_queue", [], |row| {
                row.get::<_, i64>(0)
            })?;
        let saved = transaction.query_row("SELECT COUNT(*) FROM saved_posts", [], |row| {
            row.get::<_, i64>(0)
        })?;
        let posts = transaction.query_row("SELECT COUNT(*) FROM posts WHERE NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = posts.id)", [], |row| row.get::<_, i64>(0))?;
        transaction.commit()?;
        let mut metadata_disk_bytes = fs::metadata(self.root.join("metadata.sqlite3"))?.len();
        for sidecar in ["metadata.sqlite3-wal", "metadata.sqlite3-shm"] {
            metadata_disk_bytes = metadata_disk_bytes
                .checked_add(optional_file_len(&self.root.join(sidecar))?)
                .ok_or_else(|| io::Error::other("metadata disk size exceeds u64"))?;
        }
        Ok(StoreStatusSnapshot {
            staging_bytes_in_use: self.staging_bytes_in_use(),
            max_staging_bytes: self.max_staging_bytes(),
            max_concurrent_publications: self.max_concurrent_publications(),
            finalized_unique_blob_bytes: nonnegative("finalized_unique_blob_bytes", finalized)?,
            active_sessions: nonnegative("active_sessions", active)?,
            sessions_due_for_purge: nonnegative("sessions_due_for_purge", due)?,
            queued_blob_deletions: nonnegative("queued_blob_deletions", queued)?,
            metadata_disk_bytes,
            saved_posts: nonnegative("saved_posts", saved)?,
            total_posts: nonnegative("total_posts", posts)?,
            limits: self.limits,
        })
    }
}

fn optional_file_len(path: &Path) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn nonnegative(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStatusValue { field, value })
}

#[cfg(all(test, unix))]
mod tests {
    use super::optional_file_len;

    #[test]
    fn missing_sidecar_is_zero_but_other_metadata_errors_propagate() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("metadata.sqlite3-wal");
        assert_eq!(optional_file_len(&path).unwrap(), 0);
        std::os::unix::fs::symlink("metadata.sqlite3-wal", &path).unwrap();
        assert!(optional_file_len(&path).is_err());
    }
}
