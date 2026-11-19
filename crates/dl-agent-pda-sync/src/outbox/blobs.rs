//! The blob queue `blobs.db` (port of `Outbox/SqliteBlobOutbox.cs`): blob column values
//! waiting for the cloud (blob contract v1, ADR 0030), kept apart from the batch outbox so
//! images never sit inside a row batch. Same schema and statements as the C# agent.
//!
//! ```text
//! blob_queue(id INTEGER PRIMARY KEY AUTOINCREMENT, ref_key TEXT, ref_json TEXT,
//!            sha256 TEXT, content BLOB, bytes INTEGER, quarantined INTEGER DEFAULT 0,
//!            attempts INTEGER DEFAULT 0, reason TEXT NULL, created_utc TEXT)
//!            -- ux_blob_queue_ref(ref_key)
//! ```

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use dl_sqlite::{Connection, TransactionBehavior};

use super::error::{OutboxError, Result};
use super::sql::{self, Param};
use crate::contracts::WireJson;
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::hashing::row_key;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::{SyncBlobRef, blob_contract};
use crate::reporting::{Logger, Value};
use crate::util::stj::eq_ignore_case;

/// `MM.PdaOrder.SyncAgent.Outbox.SqliteBlobOutbox`: the log category.
pub const BLOB_CATEGORY: &str = "MM.PdaOrder.SyncAgent.Outbox.SqliteBlobOutbox";

/// The C# constructor's schema script.
pub const BLOB_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS blob_queue(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ref_key TEXT NOT NULL, ref_json TEXT NOT NULL, sha256 TEXT NOT NULL, content BLOB NOT NULL,
  bytes INTEGER NOT NULL, quarantined INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0,
  reason TEXT NULL, created_utc TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS ux_blob_queue_ref ON blob_queue(ref_key);
";

/// What [`SqliteBlobOutbox::enqueue`] did (`BlobEnqueueResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobEnqueueResult {
    /// Stored (or replaced the pending value of the same cell).
    Queued,
    /// The size cap is reached: nothing stored, nothing dropped; retry later.
    CapReached,
    /// The cloud would refuse the ref or the content: not queued.
    Refused,
}

/// One queued blob (`BlobOutboxItem`).
#[derive(Debug, Clone, PartialEq)]
pub struct BlobOutboxItem {
    /// Row id (queue order).
    pub id: i64,
    /// The cloud address.
    pub ref_: SyncBlobRef,
    /// The content.
    pub content: Vec<u8>,
}

/// Durable queue of blob cell values. One entry per (table, row, column): a newer value of
/// the same cell replaces the pending one, so only the latest image is uploaded. Content
/// is never logged (member photos are PII).
pub struct SqliteBlobOutbox {
    db: Mutex<Connection>,
    cap_bytes: i64,
    catalog: &'static SyncCatalog,
    log: Logger,
}

impl std::fmt::Debug for SqliteBlobOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteBlobOutbox")
            .field("cap_bytes", &self.cap_bytes)
            .finish_non_exhaustive()
    }
}

impl SqliteBlobOutbox {
    /// Opens (creating when absent, with its directory) the queue at `path`; `cap_bytes`
    /// bounds the stored content bytes.
    pub fn open(
        path: impl AsRef<Path>,
        cap_bytes: i64,
        catalog: &'static SyncCatalog,
        log: &Logger,
    ) -> Result<SqliteBlobOutbox> {
        let db = sql::open_store(path.as_ref(), &[BLOB_SCHEMA])?;
        Ok(SqliteBlobOutbox {
            db: Mutex::new(db),
            cap_bytes,
            catalog,
            log: log.for_category(BLOB_CATEGORY),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn scalar(&self, sql_text: &str, params: &[Param<'_>]) -> Result<i64> {
        sql::scalar_i64(&self.lock(), sql_text, params)
    }

    fn exec(&self, sql_text: &str, params: &[Param<'_>]) -> Result<usize> {
        sql::exec(&self.lock(), sql_text, params)
    }

    /// Blobs still to upload (quarantined ones excluded); the heartbeat's
    /// `blob_queue_depth`.
    pub fn depth(&self) -> Result<i64> {
        self.scalar("SELECT COUNT(*) FROM blob_queue WHERE quarantined=0", &[])
    }

    /// Content bytes stored, quarantined entries included.
    pub fn bytes(&self) -> Result<i64> {
        self.scalar("SELECT COALESCE(SUM(bytes),0) FROM blob_queue", &[])
    }

    /// Entries parked by [`SqliteBlobOutbox::quarantine`].
    pub fn quarantined_count(&self) -> Result<i64> {
        self.scalar("SELECT COUNT(*) FROM blob_queue WHERE quarantined=1", &[])
    }

    /// Queues the value of one blob cell durably. A ref the cloud would refuse (not a
    /// catalog blob column, bad hash, content over [`blob_contract::MAX_BYTES`]) is not
    /// queued; at the size cap nothing is dropped and the caller retries later.
    pub fn enqueue(&self, address: &SyncBlobRef, content: &[u8]) -> Result<BlobEnqueueResult> {
        if let Some(reason) = blob_contract::check(Some(address), self.catalog) {
            self.log.warn(
                "Blob for {Table}.{Column} not queued: {Reason}",
                &[
                    Value::from(&address.table),
                    Value::from(&address.column),
                    Value::from(reason),
                ],
            );
            return Ok(BlobEnqueueResult::Refused);
        }
        let len = i64::try_from(content.len()).unwrap_or(i64::MAX);
        if len == 0 || len > blob_contract::MAX_BYTES {
            self.log.warn(
                "Blob for {Table}.{Column} not queued: {Bytes} bytes is outside 1..{Max}",
                &[
                    Value::from(&address.table),
                    Value::from(&address.column),
                    Value::from(len),
                    Value::from(blob_contract::MAX_BYTES),
                ],
            );
            return Ok(BlobEnqueueResult::Refused);
        }
        let Some(key) = self.ref_key(address) else {
            // check() accepted the ref, so its table and column are in the catalog.
            return Ok(BlobEnqueueResult::Refused);
        };
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let pending = sql::scalar_i64(
            &tx,
            "SELECT COALESCE(SUM(bytes),0) FROM blob_queue WHERE ref_key<>$k",
            &[("$k", &key)],
        )?;
        if pending.saturating_add(len) > self.cap_bytes {
            tx.rollback()?;
            self.log.warn(
                "Blob queue cap of {Cap} bytes reached; {Table}.{Column} stays pending at the source",
                &[
                    Value::from(self.cap_bytes),
                    Value::from(&address.table),
                    Value::from(&address.column),
                ],
            );
            return Ok(BlobEnqueueResult::CapReached);
        }
        let json = address.to_json();
        let created = DateTimeOffset::now_utc().to_round_trip_string();
        sql::exec(
            &tx,
            "INSERT INTO blob_queue(ref_key,ref_json,sha256,content,bytes,created_utc) VALUES($k,$j,$s,$c,$b,$t)
ON CONFLICT(ref_key) DO UPDATE SET ref_json=excluded.ref_json, sha256=excluded.sha256,
  content=excluded.content, bytes=excluded.bytes, quarantined=0, attempts=0, reason=NULL,
  created_utc=excluded.created_utc",
            &[
                ("$k", &key),
                ("$j", &json),
                ("$s", &address.sha256),
                ("$c", &content),
                ("$b", &len),
                ("$t", &created),
            ],
        )?;
        tx.commit()?;
        Ok(BlobEnqueueResult::Queued)
    }

    /// Oldest first; quarantined entries are skipped.
    pub fn peek(&self, max: i32) -> Result<Vec<BlobOutboxItem>> {
        let rows = sql::query(
            &self.lock(),
            "SELECT id,ref_json,content FROM blob_queue WHERE quarantined=0 ORDER BY id LIMIT $n",
            &[("$n", &max)],
            |r| Ok((r.get::<i64>(0)?, r.get::<String>(1)?, r.get::<Vec<u8>>(2)?)),
        )?;
        rows.into_iter()
            .map(|(id, json, content)| {
                let ref_ = SyncBlobRef::from_json(&json).map_err(|_| {
                    OutboxError::Corrupt(format!("Blob queue entry {id} has an unreadable ref."))
                })?;
                Ok(BlobOutboxItem { id, ref_, content })
            })
            .collect()
    }

    /// Removes an uploaded entry, but only if it still holds that content: a newer value
    /// of the same cell queued while the upload ran keeps the entry and is uploaded next.
    pub fn ack(&self, id: i64, sha256: &str) -> Result<bool> {
        Ok(self.exec(
            "DELETE FROM blob_queue WHERE id=$i AND sha256=$s",
            &[("$i", &id), ("$s", &sha256)],
        )? > 0)
    }

    /// Counts one more upload attempt.
    pub fn mark_attempt(&self, id: i64) -> Result<()> {
        self.exec(
            "UPDATE blob_queue SET attempts=attempts+1 WHERE id=$i",
            &[("$i", &id)],
        )?;
        Ok(())
    }

    /// Permanently refused by the cloud: parked for diagnostics until a newer value of the
    /// cell replaces it.
    pub fn quarantine(&self, id: i64, reason: &str) -> Result<()> {
        self.exec(
            "UPDATE blob_queue SET quarantined=1, reason=$r WHERE id=$i",
            &[("$i", &id), ("$r", &reason)],
        )?;
        Ok(())
    }

    /// Same identity the cloud keys refs by: catalog table, [`row_key::of`] the wire key,
    /// catalog spelling of the column, joined with U+001F. `None` for a ref
    /// [`blob_contract::check`] refuses.
    pub fn ref_key(&self, r: &SyncBlobRef) -> Option<String> {
        let t = self.catalog.find(&r.table)?;
        let column = t
            .hash_exclude
            .as_ref()?
            .iter()
            .find(|c| eq_ignore_case(c, &r.column))?;
        Some(format!(
            "{}\u{1f}{}\u{1f}{}",
            t.table,
            row_key::of(&r.key),
            column
        ))
    }
}
