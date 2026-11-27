//! Capture engine (port of `src/MM.PdaOrder.Sync.Capture`) and the agent capture runner (`Capture/`).

pub mod codecs;
pub mod context;
pub mod engine;
pub mod error;
pub mod failure;
pub mod governor;
pub mod key_repair;
pub mod ledger;
pub mod master;
pub mod nudge;
pub mod open_check;
pub mod options;
pub mod pipeline;
pub mod plan;
pub mod report;
pub mod runner;
pub mod scheduler;
pub mod shadow_manifest;
pub mod sinks;
pub mod sinks_bridge;
pub mod snapshot;
pub mod source;
pub mod sql_builder;
pub mod sql_server_source;
pub mod sqlite_state;
pub mod src_ver;
pub mod state;
pub mod status;
pub mod util;
pub mod writer;

#[cfg(test)]
mod hop_stamp_tests;
#[cfg(test)]
mod open_check_two_phase_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod strategy_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod text_revision_tests;

pub use engine::{CaptureEngine, OPEN_STREAM, TEXT_REVISION, TEXT_REVISION_CURSOR};
pub use error::{CaptureError, ErrorKind, Result};
pub use failure::{CaptureFailureInfo, classify as classify_failure, code_for as sql_code_for};
pub use governor::{CycleDeadline, PollBudget, PollGovernor};
pub use key_repair::{KeyRepairList, KeyRepairTarget};
pub use nudge::{CaptureNudge, UploadWake};
pub use options::CaptureOptions;
pub use plan::{CapturePlan, CaptureStrategy, PlanDecision, PlanTable};
pub use report::{CaptureReport, RejectedRow, TableFault};
pub use runner::{
    CaptureRunnerDependencies, CaptureTimings, SourceFactory, SqlCaptureRunner, StateFactory,
    ZoneFactory,
};
pub use scheduler::CaptureScheduler;
pub use sinks::{BatchSink, BlobItem, BlobSink, EchoSuppressor, NoEchoSuppressor};
pub use sinks_bridge::{
    BatchStore, BlobStore, OutboxBatchSink, OutboxBatchStore, QueueBlobSink, RuntimeConfigSource,
};
pub use snapshot::SnapshotResult;
pub use source::{CaptureSource, SourceQuery, VendorRow};
pub use sql_builder::{BuiltQuery, ReadHintPolicy};
pub use sql_server_source::{SqlServerCaptureSource, SqlServerSourceOptions};
pub use sqlite_state::SqliteCaptureState;
pub use src_ver::SrcVerClock;
pub use state::{
    CaptureState, InMemoryCaptureState, Shadow, ShadowEntry, ShadowWrite, StateCommit,
};
pub use status::CaptureStatusTracker;
