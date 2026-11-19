//! The `sync.verify` contract (`Wire/SiteCommands.cs`: `SyncVerifyPayload`,
//! `SyncVerifyRepair`, `SyncVerifyTableResult`, `SyncVerifySkippedTable`,
//! `SyncVerifyCommandResult`; ADR 0126 and 0128). Members are written in the record
//! parameter order, snake_case, as `SyncJsonContext` writes them.

use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Payload of [`site_command_types::SYNC_VERIFY`](super::site_command_types::SYNC_VERIFY)
/// (ADR 0126): the vendor tables to compare (empty = every mirrored table), the bucket
/// count of the per-bucket digest (1..256, as the cloud manifest takes it) and why.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncVerifyPayload {
    /// Vendor tables; empty means every mirrored table.
    pub tables: Vec<String>,
    /// Bucket count (1..256). A missing member reads as 0 and fails validation.
    pub buckets: i32,
    /// Why.
    pub reason: String,
}

impl SyncVerifyPayload {
    /// Most tables per command.
    pub const MAX_TABLES: usize = 64;
    /// Most buckets.
    pub const MAX_BUCKETS: i32 = 256;
    /// The cloud's default bucket count.
    pub const DEFAULT_BUCKETS: i32 = 256;
    /// Longest reason.
    pub const MAX_REASON: usize = 300;
    /// The one aggregate of every verify command: the queue hands them out one at a time.
    pub const AGGREGATE_KEY: &'static str = "sync:verify";

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.tables.len() > Self::MAX_TABLES {
            return Some("tables must hold at most 64 names");
        }
        for table in &self.tables {
            if table.trim().is_empty() || utf16_len(table) > super::sync_streams::MAX_STREAM_LENGTH
            {
                return Some("each table must be 1..64 characters");
            }
        }
        if !(1..=Self::MAX_BUCKETS).contains(&self.buckets) {
            return Some("buckets must be 1..256");
        }
        if self.reason.trim().is_empty() {
            return Some("reason is required");
        }
        (utf16_len(&self.reason) > Self::MAX_REASON)
            .then_some("reason must be at most 300 characters")
    }
}

impl WireJson for SyncVerifyPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("tables");
        json::write_strings(w, &self.tables);
        w.key("buckets").int(self.buckets.into());
        w.key("reason").string(&self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncVerifyPayload {
            tables: o.list("tables", json::string_item)?,
            buckets: o.int("buckets")?,
            reason: o.string("reason")?,
        })
    }
}

/// What the agent did about a mismatch, in [`SyncVerifyCommandResult::repair`].
pub mod sync_verify_repair {
    /// Every compared table matched; nothing to repair.
    pub const NONE: &str = "none";
    /// A resnapshot of the mismatched streams was scheduled for the next capture pass.
    pub const SCHEDULED: &str = "scheduled";
    /// The same streams were already waiting for a resnapshot.
    pub const ALREADY_PENDING: &str = "already_pending";
    /// The recovery-resnapshot cooldown held the repair back; the next run tries again.
    pub const COOLDOWN: &str = "cooldown";
    /// ADR 0128: every mismatch was narrowed to row keys, queued for capture to send again
    /// or tombstone; no resnapshot.
    pub const KEYS_QUEUED: &str = "keys_queued";
}

/// One compared table: rows on each side and the bucket numbers whose count or digest
/// differ. `repair_keys` is how many differing row keys were queued for a resend by key
/// (ADR 0128); 0 when the table matched or fell back to a resnapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncVerifyTableResult {
    /// Vendor table (catalog spelling).
    pub table: String,
    /// Rows in the agent's shadow.
    pub store_rows: i32,
    /// Rows in the cloud manifest.
    pub cloud_rows: i32,
    /// Buckets whose count or digest differ, ascending.
    pub mismatched_buckets: Vec<i32>,
    /// A repair (key resend or resnapshot) was scheduled.
    pub repair_scheduled: bool,
    /// Keys queued for a resend by key.
    pub repair_keys: i32,
}

impl WireJson for SyncVerifyTableResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("table").string(&self.table);
        w.key("store_rows").int(self.store_rows.into());
        w.key("cloud_rows").int(self.cloud_rows.into());
        w.key("mismatched_buckets");
        json::write_ints(w, &self.mismatched_buckets);
        w.key("repair_scheduled").bool(self.repair_scheduled);
        w.key("repair_keys").int(self.repair_keys.into());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncVerifyTableResult {
            table: o.string("table")?,
            store_rows: o.int("store_rows")?,
            cloud_rows: o.int("cloud_rows")?,
            mismatched_buckets: o.list("mismatched_buckets", json::int_item)?,
            repair_scheduled: o.bool("repair_scheduled")?,
            repair_keys: o.int("repair_keys")?,
        })
    }
}

/// A table the agent did not compare, and why (e.g. `not_mirrored`, `open_check`,
/// `stream_busy`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncVerifySkippedTable {
    /// Vendor table (catalog spelling when known, else as requested).
    pub table: String,
    /// Why it was left out.
    pub reason: String,
}

impl SyncVerifySkippedTable {
    /// A skipped table.
    pub fn new(table: &str, reason: &str) -> SyncVerifySkippedTable {
        SyncVerifySkippedTable {
            table: table.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

impl WireJson for SyncVerifySkippedTable {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("table").string(&self.table);
        w.key("reason").string(&self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncVerifySkippedTable {
            table: o.string("table")?,
            reason: o.string("reason")?,
        })
    }
}

/// Result of an applied `sync.verify`. Named apart from the request-signature
/// `SyncVerifyResult` of the cloud. Totals cover the compared tables only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncVerifyCommandResult {
    /// Bucket count used.
    pub buckets: i32,
    /// Compared tables.
    pub tables: Vec<SyncVerifyTableResult>,
    /// Tables left out, with the reason.
    pub skipped: Vec<SyncVerifySkippedTable>,
    /// Sum of the compared tables' shadow rows.
    pub store_rows: i32,
    /// Sum of the compared tables' cloud rows.
    pub cloud_rows: i32,
    /// Tables with at least one mismatched bucket.
    pub mismatched_tables: i32,
    /// Mismatched buckets over every table.
    pub mismatched_buckets: i32,
    /// One of [`sync_verify_repair`].
    pub repair: String,
}

impl WireJson for SyncVerifyCommandResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("buckets").int(self.buckets.into());
        w.key("tables");
        json::write_items(w, &self.tables);
        w.key("skipped");
        json::write_items(w, &self.skipped);
        w.key("store_rows").int(self.store_rows.into());
        w.key("cloud_rows").int(self.cloud_rows.into());
        w.key("mismatched_tables")
            .int(self.mismatched_tables.into());
        w.key("mismatched_buckets")
            .int(self.mismatched_buckets.into());
        w.key("repair").string(&self.repair);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncVerifyCommandResult {
            buckets: o.int("buckets")?,
            tables: o.list("tables", json::item)?,
            skipped: o.list("skipped", json::item)?,
            store_rows: o.int("store_rows")?,
            cloud_rows: o.int("cloud_rows")?,
            mismatched_tables: o.int("mismatched_tables")?,
            mismatched_buckets: o.int("mismatched_buckets")?,
            repair: o.string("repair")?,
        })
    }
}
