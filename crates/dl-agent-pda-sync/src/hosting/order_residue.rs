//! `order-residue [--apply] [--older-than-minutes <n>]` (RT-1345, `OrderResidueCommand.cs`): the
//! read-only reconcile of the order rows this agent's commands left in SMILE_POS, and with
//! `--apply` the audited cleanup of the part POS WIN never clears. Every action is appended
//! to [`AUDIT_FILE`] in the agent data folder before and after the cleanup.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use super::cli::{EXIT_FAILED, EXIT_OK, EXIT_USAGE, Options, unhandled};
use super::verbs::HostContext;
use crate::commands::order_residue::{self, DEFAULT_MIN_AGE_MINUTES, OrderResidueSource};
use crate::commands::{CommandError, ConfigSource, SqlSiteCommandStore};
use crate::config::{AgentIdentity, Clock, SystemClock, codes};
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;

/// `OrderResidueCommand.Usage`.
pub const USAGE: &str = "order-residue [--apply] [--older-than-minutes <n>]";
/// `OrderResidueCommand.AuditFile`: the audit log in the agent data folder.
pub const AUDIT_FILE: &str = "order-residue-audit.log";
/// Printed after a report run without `--apply`.
pub const NOTHING_CHANGED: &str = "Nothing was changed. Add --apply to clean the cleanable rows.";
/// The refusal of a bad `--older-than-minutes`.
pub const BAD_AGE: &str = "--older-than-minutes must be a whole number of minutes, 1 or more.";

/// `Environment.NewLine` of the store machine (the audit file's line end).
#[cfg(windows)]
const NEW_LINE: &str = "\r\n";
#[cfg(not(windows))]
const NEW_LINE: &str = "\n";

/// One run of the verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidueArgs {
    /// `--apply`: clean the cleanable rows (otherwise read-only).
    pub apply: bool,
    /// `--older-than-minutes` (default [`DEFAULT_MIN_AGE_MINUTES`]).
    pub min_age_minutes: i32,
    /// `user@machine`: who cleaned, in the audit lines and the SMILE_POS rows.
    pub by: String,
}

/// `--older-than-minutes` as `int.TryParse(NumberStyles.None)`, 1 or more; absent is the
/// default. `None`: a bad value.
pub fn min_age(opts: &Options) -> Option<i32> {
    let Some(raw) = opts.get("older-than-minutes") else {
        return Some(DEFAULT_MIN_AGE_MINUTES);
    };
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<i32>().ok().filter(|m| *m >= 1)
}

/// `OrderResidueCommand.RunAsync` on the enrolled SQL target.
pub fn run(
    host: &HostContext,
    opts: &Options,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
) -> i32 {
    let Some(min_age_minutes) = min_age(opts) else {
        stderr.write_line(&format!("[{}] {BAD_AGE}", codes::BAD_ARGUMENTS));
        return EXIT_USAGE;
    };
    let identity = match AgentIdentity::try_load(&host.paths) {
        Ok(Some(id)) => id,
        Ok(None) => {
            stderr.write_line(&format!(
                "[{}] This machine is not enrolled.",
                codes::NOT_ENROLLED
            ));
            return EXIT_FAILED;
        }
        Err(e) => return unhandled(stderr, &e),
    };
    let secrets = crate::security::create(&host.paths);
    let config: ConfigSource = Arc::new(move || {
        crate::config::sql::agent_connection_for(&identity, &*secrets).map_err(Into::into)
    });
    let source = order_residue::source_of(&Arc::new(SqlSiteCommandStore::new(config)));
    let args = ResidueArgs {
        apply: opts.contains("apply"),
        min_age_minutes,
        by: format!("{}@{}", user_name(), crate::diagnostics::machine_name()),
    };
    run_parts(
        &*source,
        &args,
        &host.paths.data_dir().join(AUDIT_FILE),
        &SystemClock,
        stdout,
        stderr,
        &Ctx::background(),
    )
}

/// The verb on built parts (tests run it against a fake source): prints the report and,
/// without `--apply`, that nothing changed. A coded failure (`AgentException`) prints
/// `[code] message` and exits 1; any other failure, an audit write among them, is
/// unhandled.
pub fn run_parts(
    source: &dyn OrderResidueSource,
    args: &ResidueArgs,
    audit_file: &Path,
    clock: &dyn Clock,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    ctx: &Ctx,
) -> i32 {
    let by = args.by.as_str();
    let mut audit = |line: &str| -> crate::commands::Result<()> {
        let text = format!(
            "{}Z {by} {line}{NEW_LINE}",
            crate::config::time::wall_text(clock.now().unix_nanos(), true)
        );
        append(audit_file, &text).map_err(|e| {
            CommandError::invalid(format!("Could not append to {}: {e}", audit_file.display()))
        })
    };
    match order_residue::run(
        source,
        args.apply,
        by,
        args.min_age_minutes,
        &mut audit,
        ctx,
    ) {
        Ok(report) => {
            stdout.write_line(&order_residue::describe(&report));
            if !args.apply {
                stdout.write_line(NOTHING_CHANGED);
            }
            EXIT_OK
        }
        Err(CommandError::Agent(e)) => {
            stderr.write_line(&format!("[{}] {}", e.code(), e.message()));
            EXIT_FAILED
        }
        Err(e) => {
            let ex = e.to_exception();
            unhandled(stderr, &format!("{}: {}", ex.type_name, ex.message))
        }
    }
}

/// `File.AppendAllText`.
fn append(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// `Environment.UserName`.
fn user_name() -> String {
    let key = if cfg!(windows) { "USERNAME" } else { "USER" };
    std::env::var(key).unwrap_or_default()
}
