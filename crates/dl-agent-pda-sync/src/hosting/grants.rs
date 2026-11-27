//! `grants` (GR1, port of `Hosting/GrantsCommand.cs`): re-applies the idempotent SQL
//! bootstrap of an enrolled store under the operator's Windows login (or
//! `--sql-admin-user`), without touching the cloud identity. An existing `pda_sync` login
//! whose password the agent holds is never re-created, so the password does not change.
//! Its events are `GRANTS_STARTED/FINISHED/FAILED` on source `enroll`. With the flag
//! `capture.change_tracking` on it then runs the Change Tracking step of [`grants_ct`]
//! (RT-1318).
//!
//! [`grants_ct`]: super::grants_ct

use super::cli::{EXIT_FAILED, EXIT_OK, EXIT_USAGE, Options, unhandled};
use super::enroll::{
    admin_credential, describe, real_bootstrapper, refuse, sql_auth, tool_options,
};
use super::grants_ct::{ChangeTrackingStep, real_step};
use super::secret_prompt::{ConsoleSecretPrompt, SecretPrompt};
use super::verbs::{HostContext, VerbContext};
use crate::bootstrap::error::Error;
use crate::bootstrap::models::BootstrapReport;
use crate::bootstrap::probes::{OrderGrantsProbe, SqlOrderGrantsProbe};
use crate::bootstrap::{SqlBootstrapper, throw_if_blocked};
use crate::config::{AgentError, AgentIdentity, AgentPaths, codes};
use crate::contracts::wire::site_event_levels::{INFO, WARN};
use crate::reporting::ToolRun;
use crate::reporting::log::Exception;
use crate::reporting::run_stages::RunStages;
use crate::security::{SecretStore, names};
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;
use dl_json::Map;

/// The database was located.
pub const SQL_LOCATED: &str = "GRANTS_SQL_LOCATED";
/// The bootstrap was applied (or planned).
pub const SQL_APPLIED: &str = "GRANTS_SQL_APPLIED";
/// `pda_sync` has the order grants.
pub const VERIFIED: &str = "GRANTS_VERIFIED";

/// The inputs of `grants` (`GrantsArgs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantsArgs {
    /// `--dry-run`.
    pub dry_run: bool,
    /// `--sql-instance`.
    pub sql_instance: Option<String>,
    /// `--database`.
    pub database: Option<String>,
}

/// `GrantsCommand.ArgsFrom`.
pub fn args_from(opts: &Options) -> GrantsArgs {
    GrantsArgs {
        dry_run: opts.contains("dry-run"),
        sql_instance: opts.get("sql-instance").map(str::to_owned),
        database: opts.get("database").map(str::to_owned),
    }
}

/// The verb (`GrantsCommand.RunAsync(builder, paths, opts)`).
pub fn run(ctx: &VerbContext<'_>) -> i32 {
    let Some(host) = ctx.host else {
        return super::verbs::not_available(ctx);
    };
    run_with(host, ctx.opts, &ConsoleSecretPrompt, ctx.stdout, ctx.stderr)
}

/// The verb with an injectable password source.
pub fn run_with(
    host: &HostContext,
    opts: &Options,
    passwords: &dyn SecretPrompt,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
) -> i32 {
    let args = args_from(opts);
    let mut detail = Map::new();
    detail.insert("dry_run", args.dry_run);
    detail.insert("sql_auth", sql_auth(opts));
    let paths = host.paths.clone();
    let result = ToolRun::execute(
        tool_options(&paths, "grants", codes::GRANTS_FAILED),
        Some(detail),
        |tool| {
            let credential = match admin_credential(opts, passwords) {
                Ok(c) => c,
                Err(e) => {
                    tool.fail(e.code(), e.message(), WARN);
                    stderr.write_line(&format!("[{}] {}", e.code(), e.message()));
                    return Ok(EXIT_USAGE);
                }
            };
            let secrets = crate::security::create(&paths);
            let change_tracking = real_step(
                host.options.capture.change_tracking,
                &credential,
                tool.logger(),
            )?;
            let bootstrapper = real_bootstrapper(secrets.clone(), credential, tool.logger())
                .map_err(|e| Box::new(e.to_exception()))?;
            run_parts_with(
                &bootstrapper,
                &SqlOrderGrantsProbe,
                &*secrets,
                &paths,
                &args,
                change_tracking.as_ref(),
                tool,
                stdout,
                stderr,
                &Ctx::background(),
            )
        },
    );
    match result {
        Ok(exit) => exit,
        Err(ex) => unhandled(stderr, &format!("{}: {}", ex.type_name, ex.message)),
    }
}

/// The verb on built parts (tests run it against fakes); the exit code is the result
/// alone. An unexpected failure is returned for the tool run to report.
#[allow(clippy::too_many_arguments)]
pub fn run_parts(
    bootstrapper: &SqlBootstrapper,
    probe: &dyn OrderGrantsProbe,
    secrets: &dyn SecretStore,
    paths: &AgentPaths,
    args: &GrantsArgs,
    run: &dyn RunStages,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    ctx: &Ctx,
) -> Result<i32, Box<Exception>> {
    run_parts_with(
        bootstrapper,
        probe,
        secrets,
        paths,
        args,
        None,
        run,
        stdout,
        stderr,
        ctx,
    )
}

/// [`run_parts`] with the Change Tracking step of the flag `capture.change_tracking`
/// (RT-1318), run once the order grants are verified; `None` issues no Change Tracking
/// statement.
#[allow(clippy::too_many_arguments)]
pub fn run_parts_with(
    bootstrapper: &SqlBootstrapper,
    probe: &dyn OrderGrantsProbe,
    secrets: &dyn SecretStore,
    paths: &AgentPaths,
    args: &GrantsArgs,
    change_tracking: Option<&ChangeTrackingStep<'_>>,
    run: &dyn RunStages,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    ctx: &Ctx,
) -> Result<i32, Box<Exception>> {
    match apply(bootstrapper, paths, args, run, stdout, ctx) {
        Ok(None) => Ok(EXIT_OK),
        Ok(Some(report)) => {
            let exit = verify(probe, secrets, &report, run, stdout, stderr, ctx)?;
            match change_tracking {
                Some(step) if exit == EXIT_OK => step.run(
                    secrets,
                    &report.instance.server_name,
                    &report.database,
                    run,
                    stdout,
                    stderr,
                    ctx,
                ),
                _ => Ok(exit),
            }
        }
        Err(Error::Agent(e)) => Ok(refuse(&e, run, stderr)),
        Err(Error::Unexpected(ex)) => Err(ex),
    }
}

/// Locates, applies and prints; `None` after a dry run.
fn apply(
    bootstrapper: &SqlBootstrapper,
    paths: &AgentPaths,
    args: &GrantsArgs,
    run: &dyn RunStages,
    stdout: &dyn LineWriter,
    ctx: &Ctx,
) -> Result<Option<BootstrapReport>, Error> {
    let identity = AgentIdentity::try_load(paths)
        .map_err(|e| Error::unexpected("System.Text.Json.JsonException", e.to_string()))?
        .ok_or_else(|| {
            AgentError::new(
                codes::NOT_ENROLLED,
                "This machine is not enrolled, so there is nothing to repair. Run: dl-agent pda-enroll --code <code> --url <url>",
            )
        })?;
    let preflight = bootstrapper.preflight(
        args.sql_instance
            .as_deref()
            .or(identity.sql_instance.as_deref()),
        args.database
            .as_deref()
            .or(identity.sql_database.as_deref()),
        ctx,
    )?;
    stdout.write_line(&preflight.describe());
    let mut d = Map::new();
    d.insert("instance", preflight.instance.server_name.as_str());
    d.insert("database", preflight.database.as_str());
    d.insert("dry_run", args.dry_run);
    run.stage(INFO, SQL_LOCATED, &preflight.describe(), Some(d));
    throw_if_blocked(&preflight)?;

    let report = bootstrapper.apply(&preflight, args.dry_run, ctx)?;
    for step in &report.steps {
        stdout.write_line(&format!("  sql step {}: {}", step.id, describe(step)));
    }
    let mut d = Map::new();
    d.insert(
        "applied",
        report.steps.iter().filter(|s| s.executed).count() as i64,
    );
    d.insert(
        "skipped",
        report.steps.iter().filter(|s| s.skipped).count() as i64,
    );
    d.insert("dry_run", args.dry_run);
    run.stage(
        INFO,
        SQL_APPLIED,
        if args.dry_run {
            "Grants planned (dry run)"
        } else {
            "POS database grants applied"
        },
        Some(d),
    );
    if args.dry_run {
        stdout.write_line("Dry run complete; nothing was changed.");
        return Ok(None);
    }
    Ok(Some(report))
}

fn verify(
    probe: &dyn OrderGrantsProbe,
    secrets: &dyn SecretStore,
    report: &BootstrapReport,
    run: &dyn RunStages,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    ctx: &Ctx,
) -> Result<i32, Box<Exception>> {
    let password = secrets
        .load(names::SQL_PASSWORD)
        .map_err(|e| Box::new(Error::from_secret(&e).to_exception()))?;
    let Some(password) = password else {
        stdout.write_line(
            "Order grants not verified: the pda_sync password is not in the agent secret store.",
        );
        return Ok(EXIT_OK);
    };
    let password = String::from_utf8_lossy(&password).into_owned();
    let missing = match probe.missing(
        &report.instance.server_name,
        &report.database,
        &password,
        ctx,
    ) {
        Ok(m) => m,
        Err(e) => match e.sql() {
            Some(f) => {
                let message = format!(
                    "The grants were applied, but pda_sync could not be checked (SQL error {}).",
                    f.number()
                );
                run.fail_error(codes::SQL_LOGIN_VERIFY_FAILED, &message);
                stderr.write_line(&format!("[{}] {message}", codes::SQL_LOGIN_VERIFY_FAILED));
                return Ok(EXIT_FAILED);
            }
            None => return Err(Box::new(e.to_exception())),
        },
    };
    if !missing.is_empty() {
        let message = format!("pda_sync still lacks: {}.", missing.join(", "));
        run.fail_error(codes::SQL_GRANTS_MISSING, &message);
        stderr.write_line(&format!("[{}] {message}", codes::SQL_GRANTS_MISSING));
        return Ok(EXIT_FAILED);
    }
    stdout.write_line("pda_sync has the order grants. No restart is needed.");
    run.stage(INFO, VERIFIED, "pda_sync has the order grants", None);
    Ok(EXIT_OK)
}
