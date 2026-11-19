//! The bridge from capture's sinks to the agent's durable queues (port of the agent's
//! `Capture/CaptureSinks.cs` and the store interfaces of `Capture/ICaptureRunner.cs`).

use std::sync::Arc;

use super::error::{CaptureError, ErrorKind, Result};
use super::sinks::{BatchSink, BlobItem, BlobSink};
use crate::config::time::SharedClock;
use crate::config::{RuntimeConfig, RuntimeConfigStore, SystemClock, codes};
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::{SyncBatch, SyncBlobRef};
use crate::outbox::{
    BlobEnqueueResult, EnqueueResult, OutboxError, SqliteBlobOutbox, SqliteOutbox,
};
use crate::util::ctx::Ctx;

/// An outbox failure as a capture failure: SQLite errors are `SqliteException`
/// (`LOCAL_STORE_FAILED`), the rest keep their C# exception kind.
pub fn outbox_error(e: OutboxError) -> CaptureError {
    match e {
        OutboxError::Sqlite(e) => CaptureError::local_store(e.to_string()),
        OutboxError::Io(e) => CaptureError::new(ErrorKind::Io, e.to_string()),
        e @ (OutboxError::BatchTooLarge { .. } | OutboxError::InvalidArgument(_)) => {
            CaptureError::argument(e.to_string())
        }
        OutboxError::Corrupt(m) => CaptureError::invalid_operation(m),
    }
}

/// Sequence-assigning sink in front of the durable outbox (`IBatchStore`).
pub trait BatchStore: Send + Sync {
    /// The outbox asks for a full resnapshot.
    fn needs_resnapshot(&self) -> Result<bool>;
    /// The last watermark stored for `stream`.
    fn watermark(&self, stream: &str) -> Result<Option<String>>;
    /// Assigns the next seq of `stream` and stores `build(seq)`.
    fn enqueue(
        &self,
        stream: &str,
        build: &mut dyn FnMut(i64) -> SyncBatch,
    ) -> Result<EnqueueResult>;
    /// Clears the full-resnapshot flag.
    fn clear_needs_resnapshot(&self) -> Result<()>;
    /// Streams waiting for a targeted resnapshot (ADR 0102), parents first.
    fn pending_stream_resnapshot(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Capture forgot what it sent for these streams; the next initial load re-sends them.
    fn begin_stream_resnapshot(&self, _streams: &[String]) -> Result<()> {
        Ok(())
    }
    /// The initial load finished: the running stream resnapshots are captured.
    fn stream_resnapshot_captured(&self) -> Result<()> {
        Ok(())
    }
    /// Rows `sync.verify` queued for a resend by key (ADR 0128); `None` when the store
    /// keeps none.
    fn key_repairs(&self) -> Option<Arc<dyn super::key_repair::KeyRepairList>> {
        None
    }
}

/// Durable sink for blob column values (ADR 0030), `IBlobStore`. `CapReached` means "not
/// yet": the adapter keeps the value pending and retries. `Refused` means the cloud
/// contract can never accept it (already logged without content).
pub trait BlobStore: Send + Sync {
    /// Queues one blob.
    fn enqueue(&self, address: &SyncBlobRef, content: &[u8]) -> Result<BlobEnqueueResult>;
}

/// The cloud-pushed tunables in force (`IRuntimeConfigSource`).
pub trait RuntimeConfigSource: Send + Sync {
    /// The current config.
    fn current(&self) -> Arc<RuntimeConfig>;
}

impl RuntimeConfigSource for RuntimeConfigStore {
    fn current(&self) -> Arc<RuntimeConfig> {
        RuntimeConfigStore::current(self)
    }
}

impl BlobStore for SqliteBlobOutbox {
    fn enqueue(&self, address: &SyncBlobRef, content: &[u8]) -> Result<BlobEnqueueResult> {
        SqliteBlobOutbox::enqueue(self, address, content).map_err(outbox_error)
    }
}

/// [`BatchStore`] over the SQLite outbox (`OutboxBatchStore`).
pub struct OutboxBatchStore {
    outbox: Arc<SqliteOutbox>,
    clock: SharedClock,
}

impl std::fmt::Debug for OutboxBatchStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxBatchStore").finish_non_exhaustive()
    }
}

impl OutboxBatchStore {
    /// A store over `outbox`; `clock: None` is the system clock.
    pub fn new(outbox: Arc<SqliteOutbox>, clock: Option<SharedClock>) -> OutboxBatchStore {
        OutboxBatchStore {
            outbox,
            clock: clock.unwrap_or_else(SystemClock::shared),
        }
    }
}

impl BatchStore for OutboxBatchStore {
    fn needs_resnapshot(&self) -> Result<bool> {
        self.outbox.needs_resnapshot().map_err(outbox_error)
    }

    fn watermark(&self, stream: &str) -> Result<Option<String>> {
        self.outbox.watermark(stream).map_err(outbox_error)
    }

    /// A full outbox returns `CapReached` without flagging a resnapshot: capture keeps the
    /// unsent changes (its state is committed only after a successful enqueue) and retries
    /// once uploads drain the queue.
    fn enqueue(
        &self,
        stream: &str,
        build: &mut dyn FnMut(i64) -> SyncBatch,
    ) -> Result<EnqueueResult> {
        self.outbox
            .enqueue(stream, build, false)
            .map_err(outbox_error)
    }

    fn clear_needs_resnapshot(&self) -> Result<()> {
        self.outbox.clear_needs_resnapshot().map_err(outbox_error)
    }

    fn pending_stream_resnapshot(&self) -> Result<Vec<String>> {
        self.outbox
            .pending_stream_resnapshot()
            .map_err(outbox_error)
    }

    fn begin_stream_resnapshot(&self, streams: &[String]) -> Result<()> {
        let now = DateTimeOffset::from_unix_nanos(self.clock.now().unix_nanos(), 0)
            .unwrap_or(DateTimeOffset::UNIX_EPOCH);
        self.outbox
            .begin_stream_resnapshot(streams, now)
            .map_err(outbox_error)
    }

    fn stream_resnapshot_captured(&self) -> Result<()> {
        self.outbox
            .stream_resnapshot_captured()
            .map_err(outbox_error)
    }

    fn key_repairs(&self) -> Option<Arc<dyn super::key_repair::KeyRepairList>> {
        Some(Arc::new(crate::outbox::OutboxKeyRepairList::new(
            self.outbox.clone(),
        )))
    }
}

/// Capture's [`BatchSink`] over the durable outbox (`OutboxBatchSink`). The outbox, not
/// the capture state, numbers each stream (the cloud requires `seq = last applied + 1`),
/// so the batch is re-stamped with the outbox's seq inside the transaction that stores it.
pub struct OutboxBatchSink {
    store: Arc<dyn BatchStore>,
}

impl OutboxBatchSink {
    /// A sink over `store`.
    pub fn new(store: Arc<dyn BatchStore>) -> OutboxBatchSink {
        OutboxBatchSink { store }
    }
}

impl BatchSink for OutboxBatchSink {
    fn send(&self, batch: &SyncBatch, ctx: &Ctx) -> Result<()> {
        if ctx.is_done() {
            return Err(CaptureError::cancelled());
        }
        let mut build = |seq| SyncBatch {
            seq,
            ..batch.clone()
        };
        if self.store.enqueue(&batch.stream, &mut build)? == EnqueueResult::CapReached {
            return Err(CaptureError::backpressure(
                codes::OUTBOX_FULL,
                format!(
                    "The outbox is full; capture of stream {} waits until uploads drain it.",
                    batch.stream
                ),
            ));
        }
        Ok(())
    }
}

/// Capture's [`BlobSink`] over the blob queue (`QueueBlobSink`, ADR 0030). A full queue
/// stops the pass (the value stays pending at the source); a refused ref is treated as
/// handled so it is not retried forever.
pub struct QueueBlobSink {
    store: Arc<dyn BlobStore>,
}

impl QueueBlobSink {
    /// A sink over `store`.
    pub fn new(store: Arc<dyn BlobStore>) -> QueueBlobSink {
        QueueBlobSink { store }
    }
}

impl BlobSink for QueueBlobSink {
    fn send(&self, blob: &BlobItem, ctx: &Ctx) -> Result<()> {
        if ctx.is_done() {
            return Err(CaptureError::cancelled());
        }
        if self.store.enqueue(&blob.ref_, &blob.content)? == BlobEnqueueResult::CapReached {
            return Err(CaptureError::backpressure(
                codes::BLOB_QUEUE_FULL,
                format!(
                    "The blob queue is full; blobs of {} wait until uploads drain it.",
                    blob.ref_.table
                ),
            ));
        }
        Ok(())
    }
}
