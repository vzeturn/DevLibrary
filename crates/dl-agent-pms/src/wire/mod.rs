//! The agent <-> cloud protocol v1: frames, their validation and the message codec.
//! Mirrors `internal/wire` of the Go agent and `packages/shared/src/agent-protocol`; the
//! golden fixtures there are the cross-language check (`tests/wire_fixtures.rs`).
//!
//! Decoding is as strict as the Go agent: every top-level key must be known, required
//! keys present, `null` only where the contract allows it, integers only from integer
//! literals, then the frame's own rules. Error messages name fields, never values.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use dl_json::{Map, Value};

use crate::json::{self, Obj, TypeError};
use crate::pat;

/// Bumped only on a breaking change to the frame set or framing.
pub const PROTOCOL_VERSION: i64 = 1;
/// The WebSocket endpoint, dialled outbound from the site.
pub const STREAM_PATH: &str = "/agent/v1/stream";
/// Redeems a one-time enrollment code.
pub const ENROLL_PATH: &str = "/agent/v1/enroll";
/// Exchanges a signed request for a short-lived JWT.
pub const TOKEN_PATH: &str = "/agent/v1/token";
/// Where the agent downloads a bundle: `GET AGENT_BUNDLE_PATH/<id>`.
pub const AGENT_BUNDLE_PATH: &str = "/agent/v1/bundles";

/// Caps a downloaded bundle's JSON (base64 adds a third to the SQL).
pub const MAX_BUNDLE_BYTES: usize = 128 << 20;

/// Caps every frame, compressed or not, and after decompression.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// A frame whose JSON is larger goes as a binary gzip message.
pub const GZIP_THRESHOLD_BYTES: usize = 64 * 1024;
/// How often the cloud pings.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// Unanswered pings after which the cloud closes the session.
pub const MISSED_PONGS_LIMIT: u32 = 3;
/// The most ops in flight per agent.
pub const INFLIGHT_CAP: usize = 8;
/// Caps the reconnect and retry back-off.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Error codes of the channel.
pub const ERR_SITE_OFFLINE: &str = "SITE_OFFLINE";
/// See [`ERR_SITE_OFFLINE`].
pub const ERR_AGENT_SUPERSEDED: &str = "AGENT_SUPERSEDED";
/// The op's deadline ran out here first (informational: the cloud's timeout wins).
pub const ERR_OP_TIMEOUT: &str = "OP_TIMEOUT";
/// See [`ERR_SITE_OFFLINE`].
pub const ERR_OP_REJECTED: &str = "OP_REJECTED";
/// See [`ERR_SITE_OFFLINE`].
pub const ERR_CONTRACT_UNSUPPORTED: &str = "CONTRACT_UNSUPPORTED";
/// See [`ERR_SITE_OFFLINE`].
pub const ERR_LEASE_EXPIRED: &str = "LEASE_EXPIRED";
/// At the in-flight cap or shutting down: the op never started.
pub const ERR_AGENT_BUSY: &str = "AGENT_BUSY";

/// The codes above in the order of `AGENT_ERROR_CODES` (constants.ts).
pub const ERROR_CODES: [&str; 7] = [
    ERR_SITE_OFFLINE,
    ERR_AGENT_SUPERSEDED,
    ERR_OP_TIMEOUT,
    ERR_OP_REJECTED,
    ERR_CONTRACT_UNSUPPORTED,
    ERR_LEASE_EXPIRED,
    ERR_AGENT_BUSY,
];

/// `hello.schema_version` when the MM_SchemaVersion ledger cannot be read.
pub const SCHEMA_VERSION_UNKNOWN: i64 = 0;

/// WebSocket close codes the cloud uses; the reason string is the error code.
pub const CLOSE_BAD_FRAME: u16 = 4400;
/// See [`CLOSE_BAD_FRAME`].
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
/// See [`CLOSE_BAD_FRAME`].
pub const CLOSE_HEARTBEAT_TIMEOUT: u16 = 4408;
/// See [`CLOSE_BAD_FRAME`].
pub const CLOSE_AGENT_SUPERSEDED: u16 = 4409;
/// See [`CLOSE_BAD_FRAME`].
pub const CLOSE_CONTRACT_UNSUPPORTED: u16 = 4426;
/// See [`CLOSE_BAD_FRAME`].
pub const CLOSE_SHUTDOWN: u16 = 4503;

/// Result statuses: applied and rejected are final; failed may be retried.
pub const STATUS_APPLIED: &str = "applied";
/// See [`STATUS_APPLIED`].
pub const STATUS_REJECTED: &str = "rejected";
/// See [`STATUS_APPLIED`].
pub const STATUS_FAILED: &str = "failed";

/// Codes the agent answers a migrate frame with before (or instead of) running.
pub const MIGRATE_BUSY: &str = "MIGRATE_BUSY";
/// See [`MIGRATE_BUSY`].
pub const MIGRATE_UNSUPPORTED: &str = "MIGRATE_UNSUPPORTED";
/// See [`MIGRATE_BUSY`].
pub const BUNDLE_FETCH: &str = "BUNDLE_FETCH";
/// See [`MIGRATE_BUSY`].
pub const TARGET_MISMATCH: &str = "TARGET_MISMATCH";

/// Migrate result statuses.
pub const MIGRATE_APPLIED: &str = "applied";
/// See [`MIGRATE_APPLIED`].
pub const MIGRATE_DRY_RUN: &str = "dry_run";
/// See [`MIGRATE_APPLIED`].
pub const MIGRATE_REFUSED: &str = "refused";
/// See [`MIGRATE_APPLIED`].
pub const MIGRATE_FAILED: &str = "failed";

/// `hello`, agent -> cloud, the first frame of every session.
#[derive(Debug, Clone, PartialEq)]
pub struct Hello {
    /// Protocol version.
    pub protocol: i64,
    /// Agent build.
    pub agent_version: String,
    /// Applied MM_PMS schema (0 = unknown).
    pub schema_version: i64,
    /// Highest op contract version the agent executes.
    pub contract_max: i64,
}

/// `op`, cloud -> agent: run one operation. Args are opaque here and never logged.
#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    /// Op id (UUID).
    pub id: String,
    /// Op name.
    pub op: String,
    /// Contract version.
    pub contract_ver: i64,
    /// The arguments (an object once validated).
    pub args: Value,
    /// Idempotency key (`null` allowed).
    pub request_key: Option<String>,
    /// Optimistic concurrency stamp (optional).
    pub expected_stamp: Option<String>,
    /// Deadline in milliseconds.
    pub deadline_ms: i64,
}

/// Why a result was rejected or failed.
#[derive(Debug, Clone, PartialEq)]
pub struct OpError {
    /// Error code.
    pub code: String,
    /// SQL Server error number, when one applies.
    pub sql_number: Option<i64>,
    /// Message (never data).
    pub message: String,
}

/// One result-set row: column name -> JSON value (keys sorted on output, like Go maps).
pub type Row = BTreeMap<String, Value>;

/// `result`, agent -> cloud: the outcome of one op.
#[derive(Debug, Clone, PartialEq)]
pub struct OpResult {
    /// The op id.
    pub id: String,
    /// applied, rejected or failed.
    pub status: String,
    /// Result sets.
    pub sets: Vec<Vec<Row>>,
    /// Rows affected per statement.
    pub rowcounts: Vec<i64>,
    /// Output parameters.
    pub outputs: BTreeMap<String, Value>,
    /// The error (required unless applied).
    pub error: Option<OpError>,
    /// Set when the result was replayed from the request-key log.
    pub replayed: Option<bool>,
}

/// `ping`, the cloud heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    /// Echoed in the pong.
    pub ts: i64,
}

/// `pong`, answering a ping; the status fields are omitted when zero.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pong {
    /// The ping's ts.
    pub ts: i64,
    /// Agent build ("" omits it).
    pub agent_version: String,
    /// Applied schema (0 omits it).
    pub schema_version: i64,
    /// Process uptime in seconds (0 omits it).
    pub uptime_s: i64,
}

/// `cfg`, cloud -> agent, after an accepted hello and whenever settings change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cfg {
    /// Protocol version (must be 1).
    pub protocol: i64,
    /// `poll.interval_ms`.
    pub poll_interval_ms: i64,
    /// `limits.max_frame_bytes`.
    pub max_frame_bytes: i64,
    /// `limits.gzip_threshold_bytes`.
    pub gzip_threshold_bytes: i64,
    /// `limits.inflight_cap`.
    pub inflight_cap: i64,
    /// `limits.heartbeat_ms`.
    pub heartbeat_ms: i64,
    /// Hash of the op allow-list the cloud expects.
    pub allowlist_hash: Option<String>,
}

/// `update`, cloud -> agent: a signed agent build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// Build version.
    pub version: String,
    /// https download URL.
    pub url: String,
    /// sha256 of the build.
    pub sha256: String,
    /// Base64 Ed25519 signature.
    pub sig: String,
}

/// `migrate`, cloud -> agent: verify and apply (or dry-run) one signed bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrate {
    /// Run id (UUID).
    pub id: String,
    /// Bundle id.
    pub bundle_id: String,
    /// The version the bundle must end at.
    pub target_version: i64,
    /// Plan only.
    pub dry_run: bool,
    /// Operator approval to re-run a half-applied version (0: none).
    pub rerun_partial: i64,
    /// Report drift too.
    pub check_drift: bool,
}

/// `migrate_progress`, agent -> cloud: one runner step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateProgress {
    /// Run id.
    pub id: String,
    /// verified, adopt, backup, start, applied or failed.
    pub phase: String,
    /// Migration version.
    pub seq: i64,
    /// Migration file name.
    pub name: Option<String>,
    /// Step duration.
    pub duration_ms: i64,
}

/// Why a run was refused or failed (codes and numbers only).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrateError {
    /// Error code.
    pub code: String,
    /// Failed version.
    pub seq: Option<i64>,
    /// SQL Server error number.
    pub sql_number: Option<i32>,
    /// Line of the failing statement.
    pub line: Option<i32>,
}

/// One difference between the database and the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftFinding {
    /// ledger, proc or table.
    pub kind: String,
    /// The object.
    pub object: String,
    /// What differs.
    pub code: String,
}

/// `migrate_result`, agent -> cloud: the outcome of one run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrateResult {
    /// Run id.
    pub id: String,
    /// applied, dry_run, refused or failed.
    pub status: String,
    /// Bundle id.
    pub bundle_id: String,
    /// Ledger head.
    pub head: i64,
    /// Files applied.
    pub applied: i64,
    /// Files adopted.
    pub adopted: i64,
    /// Files still pending.
    pub pending: i64,
    /// Run duration.
    pub duration_ms: i64,
    /// Drift findings (omitted when empty).
    pub drift: Vec<DriftFinding>,
    /// The error (required unless applied or dry_run).
    pub error: Option<MigrateError>,
}

/// One decoded, validated frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// `hello`.
    Hello(Hello),
    /// `op`.
    Op(Op),
    /// `result`.
    Result(OpResult),
    /// `ping`.
    Ping(Ping),
    /// `pong`.
    Pong(Pong),
    /// `cfg`.
    Cfg(Cfg),
    /// `update`.
    Update(Update),
    /// `migrate`.
    Migrate(Migrate),
    /// `migrate_progress`.
    MigrateProgress(MigrateProgress),
    /// `migrate_result`.
    MigrateResult(MigrateResult),
}

impl Frame {
    /// The `type` tag.
    pub fn frame_type(&self) -> &'static str {
        match self {
            Frame::Hello(_) => "hello",
            Frame::Op(_) => "op",
            Frame::Result(_) => "result",
            Frame::Ping(_) => "ping",
            Frame::Pong(_) => "pong",
            Frame::Cfg(_) => "cfg",
            Frame::Update(_) => "update",
            Frame::Migrate(_) => "migrate",
            Frame::MigrateProgress(_) => "migrate_progress",
            Frame::MigrateResult(_) => "migrate_result",
        }
    }

    fn sent_by_cloud(&self) -> bool {
        matches!(
            self,
            Frame::Op(_) | Frame::Ping(_) | Frame::Cfg(_) | Frame::Update(_) | Frame::Migrate(_)
        )
    }
}

/// A frame that cannot be decoded or encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Over [`MAX_FRAME_BYTES`] (before or after decompression).
    TooLarge,
    /// Malformed or off-contract; the message names fields, never values.
    Frame(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::TooLarge => f.write_str("frame over the size limit"),
            WireError::Frame(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for WireError {}

fn frame_err(msg: impl Into<String>) -> WireError {
    WireError::Frame(msg.into())
}

mod decode;
mod encode;
mod validate;

pub use decode::*;
pub use encode::*;
use validate::*;

#[cfg(test)]
mod tests;
