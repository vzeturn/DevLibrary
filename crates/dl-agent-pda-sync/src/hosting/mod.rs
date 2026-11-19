//! CLI verbs, host loop and Windows service (port of `Hosting/`).
//!
//! - [`cli`]: verbs, options, usage and exit codes (`Cli.cs`); [`run`] is `Main`.
//! - [`verbs`]: the table the verbs of other modules plug into.
//! - [`status`]: `status [--wait]` (`StatusCommand.cs`); the diagnostics bundle is dl-agent's.
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

/// `HeartbeatSender.AgentVersion`: what the agent calls itself to the PDA cloud
/// (heartbeat, enroll, site events): `dl-agent/<version>`, the released dl-agent build
/// (`DL_AGENT_VERSION` at build), not this crate's own version.
pub fn agent_version() -> String {
    format!("dl-agent/{}", dl_agent::agent_version())
}
