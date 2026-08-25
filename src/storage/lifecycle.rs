use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{Store, StoreError, blob};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LifecycleReport {
    pub sessions_deleted: u64,
    pub projects_deleted: u64,
    pub posts_deleted: u64,
    pub post_files_deleted: u64,
    pub support_assets_deleted: u64,
    pub blob_references_deleted: u64,
    pub blobs_queued: u64,
    pub blobs_deleted: u64,
}

impl Store {
    pub fn close_session(&mut self, public_id: &str) -> Result<LifecycleReport, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_id = transaction
            .query_row(
                "SELECT id FROM sessions WHERE public_id = ?1",
                [public_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let mut report = match session_id {
            Some(session_id) => purge_sessions(&transaction, &[session_id])?,
            None => LifecycleReport::default(),
        };
        transaction.commit()?;
        report.blobs_deleted = blob::drain_blob_deletion_queue(&self.connection, &self.root)?;
        Ok(report)
    }

    pub fn purge_inactive_sessions(&mut self, cutoff: i64) -> Result<LifecycleReport, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_ids = {
            let mut statement = transaction
                .prepare("SELECT id FROM sessions WHERE last_activity_at <= ?1 ORDER BY id")?;
            statement
                .query_map([cutoff], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut report = purge_sessions(&transaction, &session_ids)?;
        transaction.commit()?;
        report.blobs_deleted = blob::drain_blob_deletion_queue(&self.connection, &self.root)?;
        Ok(report)
    }
}

fn purge_sessions(
    transaction: &Transaction<'_>,
    session_ids: &[i64],
) -> Result<LifecycleReport, StoreError> {
    let mut report = LifecycleReport::default();
    let mut candidate_hashes = Vec::new();
    let mut candidate_project_ids = Vec::new();

    for session_id in session_ids {
        candidate_project_ids.push(transaction.query_row(
            "SELECT project_id FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get::<_, i64>(0),
        )?);
        report.posts_deleted += count(
            transaction,
            "SELECT COUNT(*) FROM posts WHERE session_id = ?1",
            *session_id,
        )?;
        report.post_files_deleted += count(
            transaction,
            "SELECT COUNT(*) FROM post_files
             WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)",
            *session_id,
        )?;
        report.support_assets_deleted += count(
            transaction,
            "SELECT COUNT(*) FROM support_assets
             WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)",
            *session_id,
        )?;
        report.blob_references_deleted += count(
            transaction,
            "SELECT COUNT(*) FROM blob_references
             WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)",
            *session_id,
        )?;
        let mut statement = transaction.prepare(
            "SELECT DISTINCT blob_hash FROM blob_references
             WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)
             ORDER BY blob_hash",
        )?;
        candidate_hashes.extend(
            statement
                .query_map([session_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    for session_id in session_ids {
        report.sessions_deleted +=
            u64::try_from(transaction.execute("DELETE FROM sessions WHERE id = ?1", [session_id])?)
                .expect("SQLite affected-row count exceeds u64");
    }

    candidate_project_ids.sort_unstable();
    candidate_project_ids.dedup();
    for project_id in candidate_project_ids {
        report.projects_deleted += u64::try_from(transaction.execute(
            "DELETE FROM projects
             WHERE id = ?1
               AND NOT EXISTS (SELECT 1 FROM sessions WHERE project_id = ?1)",
            [project_id],
        )?)
        .expect("SQLite affected-row count exceeds u64");
    }

    candidate_hashes.sort();
    candidate_hashes.dedup();
    for hash in candidate_hashes {
        report.blobs_queued += u64::try_from(transaction.execute(
            "INSERT INTO blob_deletion_queue (blob_hash)
             SELECT ?1 WHERE NOT EXISTS (
                 SELECT 1 FROM blob_references WHERE blob_hash = ?1
             )
             ON CONFLICT(blob_hash) DO NOTHING",
            [hash],
        )?)
        .expect("SQLite affected-row count exceeds u64");
    }

    Ok(report)
}

fn count(transaction: &Transaction<'_>, sql: &str, session_id: i64) -> Result<u64, StoreError> {
    let value = transaction.query_row(sql, params![session_id], |row| row.get::<_, i64>(0))?;
    Ok(u64::try_from(value).expect("SQLite COUNT returned a negative value"))
}
