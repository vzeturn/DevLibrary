//! CLI verbs, host loop and Windows service (port of `Hosting/`).
//!
//! - [`cli`]: verbs, options, usage and exit codes (`Cli.cs`); [`run`] is `Main`.
//! - [`verbs`]: the table the verbs of other modules plug into.
//! - [`status`]: `status [--wait]` (`StatusCommand.cs`); [`diag`]: `diag` (`Cli.Diag`).
//! - [`secret_prompt`]: passwords from the environment or a no-echo prompt.
//! - [`enroll`]: `enroll` (`EnrollCommand.cs`); [`grants`]: `grants` (`GrantsCommand.cs`).
//! - [`host`]: composition root, service host and Windows service (`AgentHost.cs`,
//!   `Cli.RunServiceAsync`); [`services`]: the background loops (`AgentServices.cs`).

pub mod cli;
pub mod enroll;
pub mod grants;
pub mod host;
pub mod secret_prompt;
pub mod services;
pub mod status;
pub mod verbs;

pub use cli::{
    EXIT_FAILED, EXIT_OK, EXIT_UNEXPECTED, EXIT_UNHANDLED, EXIT_USAGE, Options, run, run_with,
};
pub use secret_prompt::{ConsoleSecretPrompt, SecretPrompt};
pub use verbs::{HostContext, VerbContext, VerbFn, VerbTable};

/// The Windows service name the installer registers (`WindowsServiceRecovery.ServiceName`).
pub const SERVICE_NAME: &str = "PdaSyncAgent";

/// `HeartbeatSender.AgentVersion` (`AgentBuild.VersionText`): the stamped `version+commit`
/// (`PDA_AGENT_BUILD_VERSION` at build time) when set, else the package version.
pub fn agent_version() -> String {
    match option_env!("PDA_AGENT_BUILD_VERSION") {
        Some(v) if !v.is_empty() => v.to_owned(),
        _ => env!("CARGO_PKG_VERSION").to_owned(),
    }
}
