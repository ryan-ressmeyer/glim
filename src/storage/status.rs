use rusqlite::TransactionBehavior;

use super::{Store, StoreError, StoreLimits};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreStatusSnapshot {
    pub finalized_unique_blob_bytes: u64,
    pub active_sessions: u64,
    pub sessions_due_for_purge: u64,
    pub queued_blob_deletions: u64,
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
            "SELECT COUNT(*) FROM sessions WHERE last_activity_at > ?1",
            [cutoff],
            |row| row.get::<_, i64>(0),
        )?;
        let due = transaction.query_row(
            "SELECT COUNT(*) FROM sessions WHERE last_activity_at <= ?1",
            [cutoff],
            |row| row.get::<_, i64>(0),
        )?;
        let queued =
            transaction.query_row("SELECT COUNT(*) FROM blob_deletion_queue", [], |row| {
                row.get::<_, i64>(0)
            })?;
        transaction.commit()?;
        Ok(StoreStatusSnapshot {
            finalized_unique_blob_bytes: nonnegative("finalized_unique_blob_bytes", finalized)?,
            active_sessions: nonnegative("active_sessions", active)?,
            sessions_due_for_purge: nonnegative("sessions_due_for_purge", due)?,
            queued_blob_deletions: nonnegative("queued_blob_deletions", queued)?,
            limits: self.limits,
        })
    }
}

fn nonnegative(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStatusValue { field, value })
}
