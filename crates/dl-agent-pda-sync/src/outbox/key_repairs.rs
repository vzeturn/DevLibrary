//! Rows `sync.verify` found different between the shadow and the cloud (ADR 0128), kept in
//! `key_repair` until the capture's key repair job sent them again or tombstoned them
//! (port of `Outbox/SqliteOutbox.KeyRepairs.cs`). One row per (table, cloud row key);
//! queueing a key that is already waiting keeps the older entry. Same schema and
//! statements as the C# agent, so its `outbox.db` is continued unchanged.
//!
//! Also [`SqliteOutbox::stream_unsettled`] (`SqliteOutbox.Resnapshots.cs`), which
//! `sync.verify` reads to leave a busy stream out.

use std::collections::HashSet;
use std::sync::Arc;

use dl_sqlite::TransactionBehavior;

use super::batches::SqliteOutbox;
use super::error::Result;
use super::sql;
use crate::capture::key_repair::{KeyRepairList, KeyRepairTarget};
use crate::capture::sinks_bridge::outbox_error;
use crate::contracts::time::DateTimeOffset;
use crate::util::ctx::Ctx;

/// `EnsureKeyRepairSchema`.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS key_repair(
  id INTEGER PRIMARY KEY AUTOINCREMENT, tbl TEXT NOT NULL, row_key TEXT NOT NULL, requested_utc TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS ux_key_repair ON key_repair(tbl, row_key);
";

/// Most keys one capture pass takes (`KeyRepairBatch`).
pub const KEY_REPAIR_BATCH: i64 = 5000;

impl SqliteOutbox {
    /// Queues `row_keys` of `table` (distinct, ordinal); returns how many were not waiting
    /// already.
    pub fn queue_key_repairs<S: AsRef<str>>(
        &self,
        table: &str,
        row_keys: &[S],
        now: DateTimeOffset,
    ) -> Result<usize> {
        let at = now.to_utc().to_round_trip_string();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut added = 0;
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        for key in row_keys.iter().map(AsRef::as_ref) {
            if !seen.insert(key) {
                continue;
            }
            added += sql::exec(
                &tx,
                "INSERT OR IGNORE INTO key_repair(tbl,row_key,requested_utc) VALUES($t,$k,$u)",
                &[("$t", &table), ("$k", &key), ("$u", &at)],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }

    /// The oldest waiting keys, at most [`KEY_REPAIR_BATCH`].
    pub fn pending_key_repairs(&self) -> Result<Vec<KeyRepairTarget>> {
        sql::query(
            &self.lock(),
            "SELECT id, tbl, row_key FROM key_repair ORDER BY id LIMIT $n",
            &[("$n", &KEY_REPAIR_BATCH)],
            |r| {
                Ok(KeyRepairTarget {
                    id: r.get(0)?,
                    table: r.get(1)?,
                    row_key: r.get(2)?,
                })
            },
        )
    }

    /// Removes settled keys.
    pub fn complete_key_repairs(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        for id in ids {
            sql::exec(&tx, "DELETE FROM key_repair WHERE id=$i", &[("$i", id)])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Keys of `table` still waiting for the key repair job.
    pub fn key_repairs_pending(&self, table: &str) -> Result<i64> {
        sql::scalar_i64(
            &self.lock(),
            "SELECT COUNT(*) FROM key_repair WHERE tbl=$t",
            &[("$t", &table)],
        )
    }

    /// True while `stream` has batches waiting to upload or a resnapshot not yet finished:
    /// its shadow is then ahead of the cloud by design, so `sync.verify` (ADR 0126) leaves
    /// it out instead of reporting a mismatch.
    pub fn stream_unsettled(&self, stream: &str) -> Result<bool> {
        Ok(sql::scalar_i64(
            &self.lock(),
            "SELECT (SELECT COUNT(*) FROM outbox WHERE stream=$s AND quarantined=0)
     + (SELECT COUNT(*) FROM stream_resnapshot WHERE stream=$s AND state IN ('pending','running','captured'))",
            &[("$s", &stream)],
        )? > 0)
    }
}

/// The capture library's view of the outbox's `key_repair` table (`OutboxKeyRepairList`).
#[derive(Debug, Clone)]
pub struct OutboxKeyRepairList {
    outbox: Arc<SqliteOutbox>,
}

impl OutboxKeyRepairList {
    /// The list over `outbox`.
    pub fn new(outbox: Arc<SqliteOutbox>) -> OutboxKeyRepairList {
        OutboxKeyRepairList { outbox }
    }
}

impl KeyRepairList for OutboxKeyRepairList {
    fn pending(&self, _ctx: &Ctx) -> crate::capture::Result<Vec<KeyRepairTarget>> {
        self.outbox.pending_key_repairs().map_err(outbox_error)
    }

    fn complete(&self, ids: &[i64], _ctx: &Ctx) -> crate::capture::Result<()> {
        self.outbox.complete_key_repairs(ids).map_err(outbox_error)
    }
}
