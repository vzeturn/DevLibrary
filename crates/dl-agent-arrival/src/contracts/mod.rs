//! Port of `MM.Arrival.Sync.Contracts`: the wire contract shared with the Check-In cloud.
//! Everything here must produce the same bytes as the .NET code; `tests/golden_*.rs` hold
//! it to fixtures the .NET code wrote.

pub mod catalog;
pub mod delta;
pub mod envelope;
pub mod hasher;
pub mod key_vault;
pub mod messages;
pub mod projection;
pub mod row;
pub mod validator;
pub mod values;
pub mod vault_ops;
pub mod xml;

pub use catalog::{Catalog, Operation, OperationKind, ParamType, Parameter, catalog};
pub use envelope::{Direction, EnvelopeCipher, ReplayGuard, SyncEnvelope};
pub use messages::{
    AgentHello, AgentPollRequest, AgentPollResponse, AgentProblem, Params, PmsChangeNotice,
    PmsRequest, PmsResponse, PmsResultSet, Status,
};
pub use row::PmsRow;

/// `PmsErrors`: only the SQL error number crosses the wire (messages can carry PMS data).
pub mod errors {
    /// Lock request time out period exceeded.
    pub const LOCK_TIMEOUT: i32 = 1222;
    /// Client-side command timeout.
    pub const COMMAND_TIMEOUT: i32 = -2;

    /// `PmsErrors.Describe`.
    pub fn describe(n: i32) -> String {
        match n {
            LOCK_TIMEOUT => {
                format!("PMS error {LOCK_TIMEOUT}: record locked by another PMS session")
            }
            COMMAND_TIMEOUT => format!("PMS error {COMMAND_TIMEOUT}: PMS did not answer in time"),
            _ => format!("PMS error {n}"),
        }
    }

    /// `PmsErrors.IsRecordLocked`.
    pub fn is_record_locked(error: Option<&str>) -> bool {
        error.is_some_and(|e| e.starts_with(&format!("PMS error {LOCK_TIMEOUT}")))
    }
}
