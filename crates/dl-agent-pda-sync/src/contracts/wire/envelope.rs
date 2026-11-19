//! Batch, command, command result and heartbeat envelopes (`Wire/Envelope.cs`,
//! `Wire/WireVersion.cs`).

use dl_json::Value;
use dl_uuid::Uuid;

use crate::contracts::json::{self, WireJson, string_enum};
use crate::contracts::map::StrMap;
use crate::contracts::time::DateTimeOffset;
use crate::util::stj::{Obj, ReadError, Writer};

/// Wire-level constants (`WireVersion`).
pub mod wire_version {
    /// Batch/command/heartbeat envelope schema version.
    pub const ENVELOPE: i32 = 1;
    /// Mapper (vendor row to PG row) version, sent as `mapper_ver` in every batch.
    pub const MAPPER: i32 = 1;
    /// Hard cap of one request body (spec section 6).
    pub const MAX_BODY_BYTES: i32 = 8 * 1024 * 1024;
    /// Row-hash algorithm id.
    pub const HASH_ALGORITHM: &str = "sha256-v1";
}

string_enum! {
    /// How the rows in a batch relate to the cloud state.
    SyncMode {
        /// Incremental upserts and tombstones.
        Delta = 0,
        /// Complete state of the table/aggregate; absent keys are implicit tombstones.
        Snapshot = 1,
        /// Auto-repair re-send after a checksum mismatch.
        Repair = 2,
    }
}

string_enum! {
    /// Terminal state of a command as reported by the agent.
    CommandStatus {
        /// Applied.
        Applied = 0,
        /// Refused without side effects (stale hash, check locked, expired).
        Rejected = 1,
        /// Failed.
        Failed = 2,
        /// Outcome not known; never retried blindly, consult the command log first.
        Unknown = 3,
    }
}

/// One row on the wire. All values are canonical invariant strings (`None` = SQL NULL).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncRow {
    /// PG key columns (tenant is implicit, taken from the token).
    pub key: StrMap,
    /// Mapped PG columns, including `src_extra` JSON when present.
    pub columns: StrMap,
    /// Canonical row hash ([`wire_version::HASH_ALGORITHM`]).
    pub hash: String,
    /// Monotonic source version used to guard upserts.
    pub src_ver: i64,
}

impl WireJson for SyncRow {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("key");
        json::write_str_map(w, &self.key);
        w.key("columns");
        json::write_str_map(w, &self.columns);
        w.key("hash").string(&self.hash);
        w.key("src_ver").int(self.src_ver);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncRow {
            key: json::str_map(&o, "key")?,
            columns: json::str_map(&o, "columns")?,
            hash: o.string("hash")?,
            src_ver: o.int("src_ver")?,
        })
    }
}

/// A deleted/vanished row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncTombstone {
    /// Key columns.
    pub key: StrMap,
    /// Source version.
    pub src_ver: i64,
    /// Why it vanished.
    pub reason: Option<String>,
}

impl WireJson for SyncTombstone {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("key");
        json::write_str_map(w, &self.key);
        w.key("src_ver").int(self.src_ver);
        json::opt_str(w, "reason", &self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncTombstone {
            key: json::str_map(&o, "key")?,
            src_ver: o.int("src_ver")?,
            reason: o.opt_string("reason")?,
        })
    }
}

/// Batch envelope (spec section 6). `table` is the vendor table name as
/// `sync-catalog.json` keys it, never its `pg_target`.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncBatch {
    /// Agent id.
    pub agent_id: String,
    /// Stream name.
    pub stream: String,
    /// Per-stream sequence.
    pub seq: i64,
    /// [`wire_version::MAPPER`].
    pub mapper_ver: i32,
    /// Vendor table.
    pub table: String,
    /// Mode.
    pub mode: SyncMode,
    /// Upserts.
    pub rows: Vec<SyncRow>,
    /// Deletions.
    pub tombstones: Vec<SyncTombstone>,
    /// Watermark after the batch.
    pub watermark: Option<String>,
}

impl WireJson for SyncBatch {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("agent_id").string(&self.agent_id);
        w.key("stream").string(&self.stream);
        w.key("seq").int(self.seq);
        w.key("mapper_ver").int(self.mapper_ver.into());
        w.key("table").string(&self.table);
        w.key("mode").string(self.mode.name());
        w.key("rows");
        json::write_items(w, &self.rows);
        w.key("tombstones");
        json::write_items(w, &self.tombstones);
        json::opt_str(w, "watermark", &self.watermark);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncBatch {
            agent_id: o.string("agent_id")?,
            stream: o.string("stream")?,
            seq: o.int("seq")?,
            mapper_ver: o.int("mapper_ver")?,
            table: o.string("table")?,
            mode: json::enum_member(&o, "mode", SyncMode::MEMBERS)?,
            rows: o.list("rows", json::item)?,
            tombstones: o.list("tombstones", json::item)?,
            watermark: o.opt_string("watermark")?,
        })
    }
}

/// Cloud to agent command (spec section 5, ADR 0029, order fields ADR 0100).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncCommand {
    /// Command id.
    pub command_id: Uuid,
    /// Wire type (`kds.line_state`, `order.item_post`, ...).
    pub type_: String,
    /// Aggregate the router orders commands by.
    pub aggregate_key: String,
    /// Fingerprint of the check the user saw (ADR 0100); `None` when not guarded.
    pub expected_hash: Option<String>,
    /// The payload (`JsonElement`).
    pub payload: Value,
    /// Who sent it.
    pub created_by: String,
    /// Expiry.
    pub expires_at_utc: Option<DateTimeOffset>,
    /// Contract version the payload was written for; absent on the wire means 1.
    pub contract_version: i32,
    /// Monotonic per-aggregate sequence; absent from older clouds.
    pub aggregate_seq: Option<i64>,
    /// The client's Idempotency-Key.
    pub idempotency_key: Option<String>,
    /// Tenant the command belongs to.
    pub tenant_id: Option<Uuid>,
}

impl WireJson for SyncCommand {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("command_id").string(&self.command_id.to_string());
        w.key("type").string(&self.type_);
        w.key("aggregate_key").string(&self.aggregate_key);
        json::opt_str(w, "expected_hash", &self.expected_hash);
        w.key("payload").value(&self.payload);
        w.key("created_by").string(&self.created_by);
        json::opt_dto_member(w, "expires_at_utc", self.expires_at_utc);
        w.key("contract_version").int(self.contract_version.into());
        json::opt_int(w, "aggregate_seq", self.aggregate_seq);
        json::opt_str(w, "idempotency_key", &self.idempotency_key);
        if let Some(t) = self.tenant_id {
            w.key("tenant_id").string(&t.to_string());
        }
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncCommand {
            command_id: o.guid("command_id")?,
            type_: o.string("type")?,
            aggregate_key: o.string("aggregate_key")?,
            expected_hash: o.opt_string("expected_hash")?,
            payload: json::element(&o, "payload"),
            created_by: o.string("created_by")?,
            expires_at_utc: json::opt_dto(&o, "expires_at_utc")?,
            contract_version: o.int_or("contract_version", 1)?,
            aggregate_seq: o.opt_int("aggregate_seq")?,
            idempotency_key: o.opt_string("idempotency_key")?,
            tenant_id: o.opt_guid("tenant_id")?,
        })
    }
}

/// Agent reply for one command; replaying the same command returns the stored result.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncCommandResult {
    /// Command id.
    pub command_id: Uuid,
    /// Outcome.
    pub status: CommandStatus,
    /// Rejection/failure reason.
    pub reason: Option<String>,
    /// Result payload (`JsonElement?`).
    pub result: Option<Value>,
    /// When the agent finished.
    pub at_utc: DateTimeOffset,
    /// Milliseconds spent against SMILE_POS (ADR 0100).
    pub exec_ms: Option<i64>,
}

impl WireJson for SyncCommandResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("command_id").string(&self.command_id.to_string());
        w.key("status").string(self.status.name());
        json::opt_str(w, "reason", &self.reason);
        json::opt_value(w, "result", &self.result);
        w.key("at_utc").unescaped(&self.at_utc.to_json_string());
        json::opt_int(w, "exec_ms", self.exec_ms);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncCommandResult {
            command_id: o.guid("command_id")?,
            status: json::enum_member(&o, "status", CommandStatus::MEMBERS)?,
            reason: o.opt_string("reason")?,
            result: json::opt_element(&o, "result"),
            at_utc: json::dto(&o, "at_utc")?,
            exec_ms: o.opt_int("exec_ms")?,
        })
    }
}

/// Replication lag of a single stream.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamLag {
    /// Stream.
    pub stream: String,
    /// Rows behind.
    pub lag_rows: i64,
    /// Seconds behind.
    pub lag_seconds: f64,
}

impl WireJson for StreamLag {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("stream").string(&self.stream);
        w.key("lag_rows").int(self.lag_rows);
        w.key("lag_seconds").f64(self.lag_seconds);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(StreamLag {
            stream: o.string("stream")?,
            lag_rows: o.int("lag_rows")?,
            lag_seconds: o.f64("lag_seconds")?,
        })
    }
}

/// A resnapshot of `stream` that finished cleanly (ADR 0102).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamResnapshot {
    /// Stream.
    pub stream: String,
    /// Last captured seq of the resnapshot.
    pub through_seq: i64,
    /// When it completed.
    pub completed_at_utc: DateTimeOffset,
}

impl WireJson for StreamResnapshot {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("stream").string(&self.stream);
        w.key("through_seq").int(self.through_seq);
        w.key("completed_at_utc")
            .unescaped(&self.completed_at_utc.to_json_string());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(StreamResnapshot {
            stream: o.string("stream")?,
            through_seq: o.int("through_seq")?,
            completed_at_utc: json::dto(&o, "completed_at_utc")?,
        })
    }
}

/// Agent liveness and health report.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncHeartbeat {
    /// Agent id.
    pub agent_id: String,
    /// When.
    pub at_utc: DateTimeOffset,
    /// Agent version.
    pub agent_version: String,
    /// [`wire_version::ENVELOPE`].
    pub envelope_version: i32,
    /// [`wire_version::MAPPER`].
    pub mapper_version: i32,
    /// Catalog version.
    pub catalog_version: i32,
    /// Whether SQL Server answered.
    pub sql_reachable: bool,
    /// Batches waiting.
    pub queue_depth: i64,
    /// Per-stream lag.
    pub streams: Vec<StreamLag>,
    /// Blobs waiting for upload (ADR 0030).
    pub blob_queue_depth: Option<i64>,
    /// Stable code of the failure that stops capture (`SQL_LOGIN_FAILED`, ...).
    pub capture_error: Option<String>,
    /// Clean resnapshots of the last hours (ADR 0102).
    pub resnapshots: Option<Vec<StreamResnapshot>>,
}

impl WireJson for SyncHeartbeat {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("agent_id").string(&self.agent_id);
        w.key("at_utc").unescaped(&self.at_utc.to_json_string());
        w.key("agent_version").string(&self.agent_version);
        w.key("envelope_version").int(self.envelope_version.into());
        w.key("mapper_version").int(self.mapper_version.into());
        w.key("catalog_version").int(self.catalog_version.into());
        w.key("sql_reachable").bool(self.sql_reachable);
        w.key("queue_depth").int(self.queue_depth);
        w.key("streams");
        json::write_items(w, &self.streams);
        json::opt_int(w, "blob_queue_depth", self.blob_queue_depth);
        json::opt_str(w, "capture_error", &self.capture_error);
        if let Some(r) = &self.resnapshots {
            w.key("resnapshots");
            json::write_items(w, r);
        }
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(SyncHeartbeat {
            agent_id: o.string("agent_id")?,
            at_utc: json::dto(&o, "at_utc")?,
            agent_version: o.string("agent_version")?,
            envelope_version: o.int("envelope_version")?,
            mapper_version: o.int("mapper_version")?,
            catalog_version: o.int("catalog_version")?,
            sql_reachable: o.bool("sql_reachable")?,
            queue_depth: o.int("queue_depth")?,
            streams: o.list("streams", json::item)?,
            blob_queue_depth: o.opt_int("blob_queue_depth")?,
            capture_error: o.opt_string("capture_error")?,
            resnapshots: o.opt_list("resnapshots", json::item)?,
        })
    }
}
