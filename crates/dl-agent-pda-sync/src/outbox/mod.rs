//! SQLite outboxes and uploaders (port of `Outbox/`).
//!
//! - [`SqliteOutbox`]: `outbox.db`, the durable batch queue with per-stream seq,
//!   watermarks, quarantine, gap renumbering and targeted stream resnapshots.
//! - [`SqliteBlobOutbox`]: `blobs.db`, blob cell values waiting for upload (ADR 0030).
//! - [`ParentPendingTracker`]: the uploader's HTTP 424 parking (ADR 0102).
//! - [`BatchUploader`], [`BlobUploader`]: drain the two queues to the cloud.
//! - [`HeartbeatSender`]: the heartbeat, its health verdict and the cloud config.

mod batch_uploader;
mod batches;
mod blob_uploader;
mod blobs;
mod error;
mod heartbeat;
mod identity;
mod key_repairs;
mod parent_pending;
mod resnapshots;
mod sql;

pub use batch_uploader::{
    BATCH_CATEGORY, BATCHES_PATH, BatchUploader, MAX_REASON, PARENT_GONE_CODE,
    RECOVERY_RESNAPSHOT_COOLDOWN, UploadCycleResult, parent_stream_of, rejection_of, table_of,
};
pub use batches::{CATEGORY, EnqueueResult, OutboxItem, ResequenceResult, SCHEMA, SqliteOutbox};
pub use blob_uploader::{BLOB_UPLOADER_CATEGORY, BlobUploader};
pub use blobs::{BLOB_CATEGORY, BLOB_SCHEMA, BlobEnqueueResult, BlobOutboxItem, SqliteBlobOutbox};
pub use error::{OutboxError, Result, UploadError};
pub use heartbeat::{
    CaptureStatus, HEARTBEAT_CATEGORY, HEARTBEAT_PATH, HeartbeatParts, HeartbeatSender,
};
pub use identity::AdoptResult;
pub use key_repairs::{KEY_REPAIR_BATCH, OutboxKeyRepairList, SCHEMA as KEY_REPAIR_SCHEMA};
pub use parent_pending::{ParentPendingEpisode, ParentPendingTracker};
pub use resnapshots::{
    MAX_REPORTED_RESNAPSHOTS, RESNAPSHOT_HISTORY, SCHEMA as RESNAPSHOT_SCHEMA,
    StreamResnapshotRequest,
};
