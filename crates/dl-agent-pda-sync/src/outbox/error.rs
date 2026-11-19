//! The error of the outbox stores.

use crate::contracts::wire::wire_version;

/// Why an outbox operation failed.
#[derive(Debug)]
pub enum OutboxError {
    /// The SQLite library refused (busy past the timeout, disk full, corrupt file, ...).
    Sqlite(dl_sqlite::Error),
    /// A file operation failed (creating the data directory, writing the runtime config).
    Io(std::io::Error),
    /// A batch serialises to more than [`wire_version::MAX_BODY_BYTES`] (C#
    /// `ArgumentException`): the producer must split it.
    BatchTooLarge {
        /// UTF-8 length of the batch JSON.
        bytes: usize,
    },
    /// An argument out of range (C# `ArgumentOutOfRangeException`).
    InvalidArgument(String),
    /// Stored data the store cannot read back (C# `InvalidOperationException`,
    /// `InvalidDataException`, `FormatException`, `JsonException`).
    Corrupt(String),
}

impl std::fmt::Display for OutboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutboxError::Sqlite(e) => e.fmt(f),
            OutboxError::Io(e) => e.fmt(f),
            OutboxError::BatchTooLarge { bytes } => write!(
                f,
                "Batch of {bytes} bytes exceeds the {} byte wire cap; split it.",
                wire_version::MAX_BODY_BYTES
            ),
            OutboxError::InvalidArgument(m) | OutboxError::Corrupt(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for OutboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OutboxError::Sqlite(e) => Some(e),
            OutboxError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<dl_sqlite::Error> for OutboxError {
    fn from(e: dl_sqlite::Error) -> Self {
        OutboxError::Sqlite(e)
    }
}

impl From<std::io::Error> for OutboxError {
    fn from(e: std::io::Error) -> Self {
        OutboxError::Io(e)
    }
}

/// Result of the outbox stores.
pub type Result<T> = std::result::Result<T, OutboxError>;

/// Why an upload cycle stopped with an error (what the C# `RunCycleAsync` lets escape):
/// a cloud call that got no answer (cancelled, unreachable, token refused) or a store
/// failure.
#[derive(Debug)]
pub enum UploadError {
    /// The cloud call failed (`AgentException`, cancellation, unexpected JSON).
    Transport(crate::transport::Error),
    /// The local store failed.
    Outbox(OutboxError),
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadError::Transport(e) => e.fmt(f),
            UploadError::Outbox(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for UploadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UploadError::Transport(e) => Some(e),
            UploadError::Outbox(e) => Some(e),
        }
    }
}

impl From<crate::transport::Error> for UploadError {
    fn from(e: crate::transport::Error) -> Self {
        UploadError::Transport(e)
    }
}

impl From<OutboxError> for UploadError {
    fn from(e: OutboxError) -> Self {
        UploadError::Outbox(e)
    }
}
