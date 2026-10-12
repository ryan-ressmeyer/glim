use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;

use crate::logging::{LogLevel, daemon as log_daemon};

use super::{Store, StoreError, blob};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LifecycleReport {
    pub sessions_deleted: u64,
    #[serde(default)]
    pub sessions_retained: u64,
    pub projects_deleted: u64,
    pub posts_deleted: u64,
    pub post_files_deleted: u64,
    pub support_assets_deleted: u64,
    pub blob_references_deleted: u64,
    pub blobs_queued: u64,
    pub blobs_deleted: u64,
}

impl Store {
    pub fn set_saved(&mut self, post_id: i64, saved: bool) -> Result<(), StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM posts WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = ?1))",
            [post_id], |row| row.get::<_, bool>(0))?;
        if !exists {
            return Err(StoreError::PostNotFound { post_id });
        }
        if saved {
            tx.execute(
                "INSERT OR IGNORE INTO saved_posts (post_id) VALUES (?1)",
                [post_id],
            )?;
        } else {
            tx.execute("DELETE FROM saved_posts WHERE post_id = ?1", [post_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_post(
        &mut self,
        post_id: i64,
        confirm_saved: bool,
    ) -> Result<LifecycleReport, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found = tx.query_row(
            "SELECT p.session_id, EXISTS(SELECT 1 FROM saved_posts WHERE post_id = p.id)
             FROM posts p WHERE p.id = ?1 AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = p.id)",
            [post_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))).optional()?;
        let (session_id, saved) = found.ok_or(StoreError::PostNotFound { post_id })?;
        if saved && !confirm_saved {
            return Err(StoreError::SavedPostRequiresConfirmation { post_id });
        }
        let mut report = LifecycleReport::default();
        let mut hashes = Vec::new();
        remove_post(&tx, post_id, &mut report, &mut hashes)?;
        // A closed context is needed while any readable post remains.
        let closed = tx.query_row(
            "SELECT closed FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get::<_, bool>(0),
        )?;
        if closed
            && count(
                &tx,
                "SELECT COUNT(*) FROM posts WHERE session_id = ?1 AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = posts.id)",
                session_id,
            )? == 0
        {
            remove_session(&tx, session_id, &mut report, &mut hashes)?;
        }
        queue_unreferenced(&tx, hashes, &mut report)?;
        tx.commit()?;
        report.blobs_deleted = self.drain_committed_blob_cleanup("post_delete");
        Ok(report)
    }

    pub fn reclaim_unused_data(&mut self) -> Result<u64, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO blob_deletion_queue (blob_hash)
            SELECT b.hash FROM blobs b WHERE NOT EXISTS
            (SELECT 1 FROM blob_references r WHERE r.blob_hash = b.hash)",
            [],
        )?;
        tx.commit()?;
        blob::drain_blob_deletion_queue(&self.connection, &self.root)
    }

    pub fn close_session(&mut self, public_id: &str) -> Result<LifecycleReport, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_id = tx
            .query_row(
                "SELECT id FROM sessions WHERE public_id = ?1",
                [public_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let mut report = LifecycleReport::default();
        if let Some(id) = session_id {
            purge_sessions(&tx, &[id], &mut report)?;
        }
        tx.commit()?;
        report.blobs_deleted = self.drain_committed_blob_cleanup("session_close");
        Ok(report)
    }

    pub fn purge_inactive_sessions(&mut self, cutoff: i64) -> Result<LifecycleReport, StoreError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session_ids = {
            let mut statement = tx.prepare("SELECT id FROM sessions WHERE last_activity_at <= ?1 AND (closed = 0 OR NOT EXISTS
                (SELECT 1 FROM saved_posts v JOIN posts p ON p.id = v.post_id WHERE p.session_id = sessions.id)
                OR EXISTS (SELECT 1 FROM posts p WHERE p.session_id = sessions.id
                    AND NOT EXISTS (SELECT 1 FROM saved_posts WHERE post_id = p.id)
                    AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = p.id))) ORDER BY id")?;
            statement
                .query_map([cutoff], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut report = LifecycleReport::default();
        purge_sessions(&tx, &session_ids, &mut report)?;
        tx.commit()?;
        report.blobs_deleted = self.drain_committed_blob_cleanup("retention");
        Ok(report)
    }

    fn drain_committed_blob_cleanup(&self, trigger: &'static str) -> u64 {
        match blob::drain_blob_deletion_queue(&self.connection, &self.root) {
            Ok(deleted) => deleted,
            Err(_) => {
                log_daemon(
                    LogLevel::Warn,
                    "blob_cleanup_deferred",
                    &[("trigger", json!(trigger))],
                );
                0
            }
        }
    }
}

fn purge_sessions(
    tx: &Transaction<'_>,
    ids: &[i64],
    report: &mut LifecycleReport,
) -> Result<(), StoreError> {
    let mut hashes = Vec::new();
    for &id in ids {
        if count(
            tx,
            "SELECT COUNT(*) FROM saved_posts v JOIN posts p ON p.id = v.post_id WHERE p.session_id = ?1",
            id,
        )? > 0
        {
            // A NULL active key frees every original identity, including synthetic-looking keys.
            report.sessions_retained += tx.execute(
                "UPDATE sessions SET retained_external_key = external_key,
                external_key = NULL, closed = 1 WHERE id = ?1 AND closed = 0",
                [id],
            )? as u64;
            let posts = {
                let mut statement = tx.prepare("SELECT id FROM posts WHERE session_id = ?1 AND NOT EXISTS (SELECT 1 FROM saved_posts WHERE post_id = posts.id) AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = posts.id)")?;
                statement
                    .query_map([id], |row| row.get::<_, i64>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for post in posts {
                remove_post(tx, post, report, &mut hashes)?;
            }
        } else {
            remove_session(tx, id, report, &mut hashes)?;
        }
    }
    queue_unreferenced(tx, hashes, report)
}

fn remove_session(
    tx: &Transaction<'_>,
    id: i64,
    report: &mut LifecycleReport,
    hashes: &mut Vec<String>,
) -> Result<(), StoreError> {
    let project_id = tx.query_row(
        "SELECT project_id FROM sessions WHERE id = ?1",
        [id],
        |row| row.get::<_, i64>(0),
    )?;
    report.posts_deleted += count(
        tx,
        "SELECT COUNT(*) FROM posts WHERE session_id = ?1 AND NOT EXISTS (SELECT 1 FROM post_tombstones WHERE post_id = posts.id)",
        id,
    )?;
    report.post_files_deleted += count(
        tx,
        "SELECT COUNT(*) FROM post_files WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)",
        id,
    )?;
    report.support_assets_deleted += count(
        tx,
        "SELECT COUNT(*) FROM support_assets WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)",
        id,
    )?;
    report.blob_references_deleted += count(
        tx,
        "SELECT COUNT(*) FROM blob_references WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)",
        id,
    )?;
    hashes.extend(tx.prepare("SELECT DISTINCT blob_hash FROM blob_references WHERE post_id IN (SELECT id FROM posts WHERE session_id = ?1)")?
        .query_map([id], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?);
    report.sessions_deleted += tx.execute("DELETE FROM sessions WHERE id = ?1", [id])? as u64;
    report.projects_deleted += tx.execute("DELETE FROM projects WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM sessions WHERE project_id = ?1)", [project_id])? as u64;
    Ok(())
}

fn remove_post(
    tx: &Transaction<'_>,
    id: i64,
    report: &mut LifecycleReport,
    hashes: &mut Vec<String>,
) -> Result<(), StoreError> {
    report.posts_deleted += 1;
    report.post_files_deleted +=
        count(tx, "SELECT COUNT(*) FROM post_files WHERE post_id = ?1", id)?;
    report.support_assets_deleted += count(
        tx,
        "SELECT COUNT(*) FROM support_assets WHERE post_id = ?1",
        id,
    )?;
    report.blob_references_deleted += count(
        tx,
        "SELECT COUNT(*) FROM blob_references WHERE post_id = ?1",
        id,
    )?;
    hashes.extend(
        tx.prepare("SELECT DISTINCT blob_hash FROM blob_references WHERE post_id = ?1")?
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?,
    );
    let successor = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM posts WHERE predecessor_post_id = ?1)",
        [id],
        |row| row.get::<_, bool>(0),
    )?;
    if successor {
        tx.execute(
            "INSERT OR IGNORE INTO post_tombstones (post_id) VALUES (?1)",
            [id],
        )?;
        tx.execute("DELETE FROM saved_posts WHERE post_id = ?1", [id])?;
        tx.execute("DELETE FROM support_assets WHERE post_id = ?1", [id])?;
        tx.execute("DELETE FROM post_files WHERE post_id = ?1", [id])?;
        tx.execute("DELETE FROM blob_references WHERE post_id = ?1", [id])?;
    } else {
        tx.execute("DELETE FROM posts WHERE id = ?1", [id])?;
    }
    Ok(())
}

fn queue_unreferenced(
    tx: &Transaction<'_>,
    mut hashes: Vec<String>,
    report: &mut LifecycleReport,
) -> Result<(), StoreError> {
    hashes.sort();
    hashes.dedup();
    for hash in hashes {
        report.blobs_queued += tx.execute(
            "INSERT INTO blob_deletion_queue (blob_hash)
            SELECT ?1 WHERE NOT EXISTS (SELECT 1 FROM blob_references WHERE blob_hash = ?1)
            ON CONFLICT(blob_hash) DO NOTHING",
            [hash],
        )? as u64;
    }
    Ok(())
}

fn count(tx: &Transaction<'_>, sql: &str, id: i64) -> Result<u64, StoreError> {
    Ok(tx.query_row(sql, params![id], |row| {
        row.get::<_, i64>(0).map(|value| value as u64)
    })?)
}
