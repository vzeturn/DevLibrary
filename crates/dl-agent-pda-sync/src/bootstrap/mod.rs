//! SQL bootstrap, enrollment workflow and probes (port of `Bootstrap/`).
//!
//! - [`models`]: `BootstrapModels.cs`; [`error`]: the failure of a bootstrap call.
//! - [`preflight`]: `SqlPreflight.cs` (server info, findings, version floor).
//! - [`fingerprint`]: `SchemaFingerprint.cs`; [`discovery`]: `LocalSqlDiscovery.cs`.
//! - [`admin`]: `ISqlAdmin.cs`, `SqlServerAdmin.cs` (setup identity, connection failures);
//!   [`fo_room_charge_check`]: the existence check of script 160 and [`bo_grant_checks`]
//!   those of the SMILE_BO scripts 175, 176 and 177 (RT-1427); [`exists_sql`]: the
//!   existence query of every other step.
//! - [`probes`]: `AgentLoginProbe.cs`, `OrderGrantsProbe.cs` (reads as `pda_sync`).
//! - [`bootstrapper`]: `SqlBootstrapper.cs` and the embedded `Sql/*.sql` scripts.
//! - [`change_tracking`]: the SQL Server Change Tracking plan and SQL text;
//!   [`change_tracking_sql`]: its probe as `pda_sync` and its steps as an administrator
//!   (RT-1318).
//! - [`cloud_probe`]: `CloudProbe.cs`; [`workflow`]: `EnrollWorkflow.cs` (with
//!   `WindowsServiceRecovery`).

pub mod admin;
pub mod bo_grant_checks;
pub mod bootstrapper;
pub mod change_tracking;
pub mod change_tracking_sql;
pub mod cloud_probe;
pub mod discovery;
pub mod error;
pub mod exists_sql;
pub mod fingerprint;
pub mod fo_room_charge_check;
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
