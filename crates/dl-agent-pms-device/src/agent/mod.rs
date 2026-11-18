//! The agent core (`Agent/`): [`TokenFile`], [`ResultOutbox`], [`ResultReporter`],
//! [`CommandProcessor`] and [`DeviceAgentWorker`], plus what replaces the .NET runtime
//! services: [`StopToken`] (the stopping CancellationToken), [`Clock`] (TimeProvider) and
//! [`log`] (ILogger). The options live in [`crate::config`]. An [`Observer`] sees each
//! claim, outcome and poll (the dl-agent module's counters and health).

mod clock;
pub mod log;
mod outbox;
mod processor;
mod reporter;
mod stop;
mod token;
mod worker;

use std::fmt;

pub use clock::{Clock, SystemClock, add_duration};
pub use outbox::ResultOutbox;
pub use processor::CommandProcessor;
pub use reporter::ResultReporter;
pub use stop::StopToken;
pub use token::{TokenFile, TokenFileError};
pub use worker::{DeviceAgentWorker, MAX_BACKOFF_SECS, WorkerExit};

use crate::config::DotnetError;
use crate::contracts::{CommandOutcome, DeviceCommand};
use crate::queue::QueueError;

/// Sees what the worker does, for counters and health. Calls come from the worker thread
/// and must not block.
pub trait Observer: Send + Sync {
    /// A command was claimed.
    fn claimed(&self, command: &DeviceCommand);

    /// A claimed command produced its outcome (before the result is sent).
    fn finished(&self, command: &DeviceCommand, outcome: &CommandOutcome);

    /// One poll ended: `Ok(worked)`, or the failure the worker backs off from. A stop is
    /// not reported.
    fn polled(&self, result: Result<bool, &AgentError>);
}

/// `PendingResultsException`: results are still owed to the API, so the agent does not
/// claim more work yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingResultsError {
    /// Results left in the outbox.
    pub pending: usize,
}

impl fmt::Display for PendingResultsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} device command result(s) could not be delivered yet; not claiming until they are",
            self.pending
        )
    }
}

impl std::error::Error for PendingResultsError {}

/// A failed poll (what escapes `RunOnceAsync` in C#).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// The service is stopping (`OperationCanceledException` on the stop token).
    Stopped,
    /// Earlier results are still owed.
    PendingResults(PendingResultsError),
    /// A queue call failed in a way the reporter does not retry, or the claim failed.
    Queue(QueueError),
    /// The outbox could not be read (`IOException`).
    Io {
        /// The OS error text.
        message: String,
        /// `UnauthorizedAccessException` instead of `IOException`.
        denied: bool,
    },
}

impl AgentError {
    /// An outbox I/O failure.
    pub fn io(e: std::io::Error) -> AgentError {
        AgentError::Io {
            message: e.to_string(),
            denied: e.kind() == std::io::ErrorKind::PermissionDenied,
        }
    }

    /// The exception the C# agent would log for this failure.
    pub fn to_dotnet(&self) -> DotnetError {
        match self {
            AgentError::Stopped => DotnetError::new(
                "System.OperationCanceledException",
                "The operation was canceled.",
            ),
            AgentError::PendingResults(p) => DotnetError::new(
                "SmilePms.DeviceAgent.Agent.PendingResultsException",
                p.to_string(),
            ),
            AgentError::Queue(q) => q.to_dotnet(),
            AgentError::Io {
                message,
                denied: true,
            } => DotnetError::new("System.UnauthorizedAccessException", message),
            AgentError::Io { message, .. } => DotnetError::new("System.IO.IOException", message),
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_dotnet().message)
    }
}

impl std::error::Error for AgentError {}
