//! Agent options, paths, identity, health and error codes (port of `Config/`), plus the
//! runtime SQL connection settings (`Bootstrap/AgentSqlConnection.cs`, [`sql`]).

pub mod errors;
pub mod health;
pub mod identity;
pub mod options;
pub mod paths;
pub mod providers;
pub mod runtime;
pub mod sql;
pub mod status;
pub mod stj_read;
pub mod time;

pub use errors::{AGENT_EXCEPTION_TYPE, AgentError, codes};
pub use health::AgentHealth;
pub use identity::AgentIdentity;
pub use options::{AgentOptions, CaptureFlags, PrintRelayOptions};
pub use paths::{AgentPaths, write_atomic};
pub use providers::{ConfigError, Configuration, Provider};
pub use runtime::{RuntimeConfig, RuntimeConfigStore};
pub use status::{AgentStatus, ReadError};
pub use time::{Clock, ManualClock, SharedClock, SystemClock, Timestamp};
