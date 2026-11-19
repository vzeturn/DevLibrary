//! Stream names (`Wire/SyncStreams.cs`, ADR 0102).

/// Wire stream names of site sync and the cross-stream answers that name them.
pub mod sync_streams {
    use crate::contracts::catalog::CatalogTable;

    /// Stream shared by every open-check table.
    pub const OPEN: &str = "open";
    /// Capture strategy of the tables on [`OPEN`].
    pub const OPEN_CAPTURE: &str = "snapshot-diff";
    /// HTTP 424 code: a parent row of another stream is not delivered yet (retryable).
    pub const PARENT_PENDING_CODE: &str = "SYNC_PARENT_PENDING";
    /// Query of `POST /sync/v1/batches` that skips rows whose parent is missing (ADR 0109).
    pub const SKIP_ORPHANS_QUERY: &str = "orphans=skip";
    /// Site event the cloud writes for rows left out under [`SKIP_ORPHANS_QUERY`].
    pub const ORPHANS_SKIPPED_CODE: &str = "SYNC_ORPHANS_SKIPPED";
    /// Longest stream name a resnapshot request accepts.
    pub const MAX_STREAM_LENGTH: usize = 64;

    /// The stream that carries `table`.
    pub fn of(table: &CatalogTable) -> &str {
        if table.capture.as_deref() == Some(OPEN_CAPTURE) {
            OPEN
        } else {
            &table.table
        }
    }
}
