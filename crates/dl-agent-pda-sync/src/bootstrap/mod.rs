//! SQL bootstrap, enrollment workflow and probes (port of `Bootstrap/`).
//!
//! - [`models`]: `BootstrapModels.cs`; [`error`]: the failure of a bootstrap call.
//! - [`preflight`]: `SqlPreflight.cs` (server info, findings, version floor).
//! - [`fingerprint`]: `SchemaFingerprint.cs`; [`discovery`]: `LocalSqlDiscovery.cs`.
//! - [`admin`]: `ISqlAdmin.cs`, `SqlServerAdmin.cs` (setup identity, connection failures).
//! - [`probes`]: `AgentLoginProbe.cs`, `OrderGrantsProbe.cs` (reads as `pda_sync`).
//! - [`bootstrapper`]: `SqlBootstrapper.cs` and the embedded `Sql/*.sql` scripts.
//! - [`cloud_probe`]: `CloudProbe.cs`; [`workflow`]: `EnrollWorkflow.cs` (with
//!   `WindowsServiceRecovery`).

pub mod admin;
pub mod bootstrapper;
pub mod cloud_probe;
pub mod discovery;
pub mod error;
pub mod fingerprint;
pub mod models;
pub mod preflight;
pub mod probes;
pub mod workflow;

pub use admin::{PASSWORD_ENV_VAR, SqlAdmin, SqlAdminCredential, SqlServerAdmin, connect_failure};
pub use bootstrapper::{DEFAULT_DATABASE, STEPS, SqlBootstrapper, read_script, throw_if_blocked};
pub use discovery::{LocalSqlDiscovery, SqlInstanceDiscovery, parse_browser_reply};
pub use error::Error;
pub use fingerprint::SchemaFingerprint;
pub use models::{BootstrapReport, BootstrapStep, SqlInstance, StepKind, StepOutcome, TableSet};
pub use preflight::{PreflightFinding, SqlPreflight, SqlServerInfo, check_server};
pub use probes::{
    AgentLoginProbe, CHECKED, OrderGrantsProbe, SqlAgentLoginProbe, SqlOrderGrantsProbe,
    is_permission_denied,
};
