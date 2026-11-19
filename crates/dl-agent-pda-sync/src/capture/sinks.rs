//! Where capture output goes (`Output/Sinks.cs`).

use super::error::Result;
use crate::contracts::wire::{SyncBatch, SyncBlobRef};
use crate::util::ctx::Ctx;

/// Receives finished batches. [`BatchSink::send`] must return only once the batch is
/// durably accepted (the agent's SQLite outbox); capture commits its own state right
/// afterwards and never re-reads a batch it handed over.
pub trait BatchSink: Send + Sync {
    /// Accepts one batch durably.
    fn send(&self, batch: &SyncBatch, ctx: &Ctx) -> Result<()>;
}

/// An image/blob column travelling separately from its row, addressed by content hash.
/// [`BlobItem::ref_`] is the cloud address (blob contract, ADR 0030): vendor table and
/// column plus the row's wire key, never the `pg_target`.
#[derive(Debug, Clone, PartialEq)]
pub struct BlobItem {
    /// Cloud address.
    pub ref_: SyncBlobRef,
    /// Content bytes.
    pub content: Vec<u8>,
}

impl BlobItem {
    /// Lowercase hex SHA-256 of the content (`Ref.Sha256`).
    pub fn sha256(&self) -> &str {
        &self.ref_.sha256
    }
}

/// Receives blobs. [`BlobSink::send`] must return only once the blob is durably queued on
/// the agent; capture records the blob hash in its shadow right afterwards, so an error
/// keeps the blob pending for the next run.
pub trait BlobSink: Send + Sync {
    /// Queues one blob durably.
    fn send(&self, blob: &BlobItem, ctx: &Ctx) -> Result<()>;
}

/// Echo suppression (spec section 4): changes the agent itself applied from a cloud
/// command must not travel back up as new events. `aggregate_key` is the check number
/// for check-scoped data.
pub trait EchoSuppressor: Send + Sync {
    /// True when this aggregate state is the result of an applied command; the capture
    /// then only records it.
    fn is_echo(&self, aggregate_key: &str, state_hash: &str) -> bool;
}

/// Never suppresses; used until the command log exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEchoSuppressor;

impl EchoSuppressor for NoEchoSuppressor {
    fn is_echo(&self, _aggregate_key: &str, _state_hash: &str) -> bool {
        false
    }
}
