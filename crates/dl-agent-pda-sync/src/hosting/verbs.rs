//! The verb table: where the verbs ported by other modules plug into the CLI.
//!
//! [`table`] lists every verb whose implementation lives outside `hosting`. A module
//! that ports one sets its field to `Some(its_fn)`; until then the CLI prints
//! [`not_available`] and exits 1.

use super::cli::{EXIT_FAILED, Options};
use crate::config::{AgentOptions, AgentPaths, Configuration};
use crate::util::console::LineWriter;

/// What the CLI built before the host verbs (`enroll`, `grants`, `run`) run: the host
/// configuration (`HostApplicationBuilder.Configuration`), the bound options (with
/// `--data-dir` applied) and the created data folder.
pub struct HostContext {
    /// The full host configuration (shipped and site settings, environment, command line).
    pub configuration: Configuration,
    /// `Agent` section bound over the defaults, `--data-dir` applied.
    pub options: AgentOptions,
    /// Paths under the data folder (already created, with `logs`).
    pub paths: AgentPaths,
}

impl std::fmt::Debug for HostContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostContext")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

/// The inputs of one verb.
pub struct VerbContext<'a> {
    /// The verb (`legacy-export`, `enroll`, ...).
    pub verb: &'a str,
    /// The whole command line without the program name.
    pub args: &'a [String],
    /// `--name value` options (`Cli.ParseOptions`).
    pub opts: &'a Options,
    /// Standard output (`Console.Out`).
    pub stdout: &'a dyn LineWriter,
    /// Standard error (`Console.Error`).
    pub stderr: &'a dyn LineWriter,
    /// `None` for the verbs that run before the host is built (`legacy-*`,
    /// `apply-update`); set for `enroll`, `grants` and `run`.
    pub host: Option<&'a HostContext>,
}

/// A verb implementation: returns the process exit code.
pub type VerbFn = fn(&VerbContext<'_>) -> i32;

/// The verbs implemented outside `hosting` (`None`: not in this build yet).
#[derive(Debug, Clone, Copy, Default)]
pub struct VerbTable {
    /// `legacy-export` (ADR 0085), before the host is built.
    pub legacy_export: Option<VerbFn>,
    /// `legacy-archive` (ADR 0089), before the host is built.
    pub legacy_archive: Option<VerbFn>,
    /// `legacy-upload` (ADR 0095), before the host is built; unexpected errors exit 3.
    pub legacy_upload: Option<VerbFn>,
    /// `apply-update` (ADR 0113), the detached self-updater, before the host is built.
    pub apply_update: Option<VerbFn>,
    /// `enroll` (`EnrollCommand.RunAsync`), with the host context.
    pub enroll: Option<VerbFn>,
    /// `grants` (`GrantsCommand.RunAsync`), with the host context.
    pub grants: Option<VerbFn>,
    /// `run` / no verb: the service host (`Cli.RunServiceAsync`), with the host context.
    pub run: Option<VerbFn>,
}

/// The verbs of this build. Modules that port a verb add it here.
pub fn table() -> VerbTable {
    VerbTable {
        // legacy-* are phase-B tools of the dl-agent module; apply-update is the core self-update.
        legacy_export: None,
        legacy_archive: None,
        legacy_upload: None,
        apply_update: None,
        enroll: Some(super::enroll::run),
        grants: Some(super::grants::run),
        run: Some(super::host::run_verb),
    }
}

/// `[NOT_AVAILABLE] ...` in English and Vietnamese on standard error; exit 1. Temporary,
/// while the verb's module is not ported.
pub fn not_available(ctx: &VerbContext<'_>) -> i32 {
    ctx.stderr.write_line(&format!(
        "[NOT_AVAILABLE] The '{v}' command is not available in this build of PdaSyncAgent. / Lệnh '{v}' chưa có trong bản PdaSyncAgent này.",
        v = ctx.verb
    ));
    EXIT_FAILED
}

/// Runs `f`, or [`not_available`] when it is `None`.
pub fn dispatch(f: Option<VerbFn>, ctx: &VerbContext<'_>) -> i32 {
    match f {
        Some(f) => f(ctx),
        None => not_available(ctx),
    }
}
