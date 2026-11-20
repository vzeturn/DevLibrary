//! `enroll` as a tool run (port of `Hosting/EnrollCommand.cs`): its log goes to
//! `logs/tools-.log` and its events (`ENROLL_STARTED`, the preflight findings and stages,
//! `ENROLL_FINISHED`/`ENROLL_FAILED`) are queued in the data folder and sent as soon as
//! the machine has an identity, which a successful enroll creates.

use super::cli::{EXIT_FAILED, EXIT_OK, EXIT_USAGE, Options, unhandled};
use super::enroll_attempts::{DefaultLogin, attempts_for, run_attempts};
use super::secret_prompt::{ConsoleSecretPrompt, SecretPrompt};
use super::verbs::{HostContext, VerbContext};
use crate::bootstrap::SqlBootstrapper;
use crate::bootstrap::admin::{PASSWORD_ENV_VAR, SqlAdminCredential, SqlServerAdmin};
use crate::bootstrap::cloud_probe::HttpCloudProbe;
use crate::bootstrap::discovery::LocalSqlDiscovery;
use crate::bootstrap::error::Error;
use crate::bootstrap::fingerprint::{DEFAULT_MINIMUM_SCORE, SchemaFingerprint};
use crate::bootstrap::models::StepOutcome;
use crate::bootstrap::probes::SqlAgentLoginProbe;
use crate::bootstrap::workflow::{
    EnrollArgs, EnrollParts, EnrollResult, EnrollWorkflow, HttpFactory,
};
use crate::config::time::SystemClock;
use crate::config::{AgentError, AgentPaths, AgentStatus, codes};
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::wire::site_event_levels::{ERROR, WARN};
use crate::contracts::wire::site_event_sources;
use crate::reporting::log::{Exception, Logger};
use crate::reporting::run_stages::RunStages;
use crate::reporting::{ToolRun, ToolRunOptions};
use crate::security::SharedSecretStore;
use crate::transport::http::SERVICE_TIMEOUT;
use crate::transport::{RequestSigner, RetryPolicy, SyncHttpClient};
use crate::util::console::LineWriter;
use crate::util::ctx::Ctx;
use dl_json::Map;
use std::sync::Arc;

/// The dry run's verdict when nothing blocks.
pub const PREFLIGHT_OK: &str = "PREFLIGHT OK";
/// The dry run's verdict prefix when something blocks.
pub const PREFLIGHT_FAILED: &str = "PREFLIGHT FAILED";
/// The option naming a SQL Server administrator login.
pub const SQL_ADMIN_USER_OPTION: &str = "sql-admin-user";

/// The verb (`EnrollCommand.RunAsync(builder, paths, opts)`).
pub fn run(ctx: &VerbContext<'_>) -> i32 {
    let Some(host) = ctx.host else {
        return super::verbs::not_available(ctx);
    };
    run_with(host, ctx.opts, &ConsoleSecretPrompt, ctx.stdout, ctx.stderr)
}

/// The tool run options of a verb run from the data folder.
pub fn tool_options(paths: &AgentPaths, verb: &str, failure_code: &str) -> ToolRunOptions {
    let mut o = ToolRunOptions::new(site_event_sources::ENROLL, verb, failure_code);
    o.data_dir = Some(paths.data_dir().to_string_lossy().into_owned());
    o.agent_version = super::agent_version();
    o
}

/// `"sql"` with `--sql-admin-user`, else `"windows"` (the first identity tried; the SMILE
/// default login follows when Windows is refused, see [`super::enroll_attempts`]).
pub fn sql_auth(opts: &Options) -> &'static str {
    if opts.contains(SQL_ADMIN_USER_OPTION) {
        "sql"
    } else {
        "windows"
    }
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
    let mut detail = start_detail(&args);
    detail.insert("sql_auth", sql_auth(opts));
    let paths = host.paths.clone();
    let result = ToolRun::execute(
        tool_options(&paths, "enroll", codes::ENROLL_FAILED),
        Some(detail),
        |tool| {
            let attempts = match attempts_for(opts, passwords, &DefaultLogin::from_process()) {
                Ok(a) => a,
                Err(e) => {
                    tool.fail(e.code(), e.message(), WARN);
                    stderr.write_line(&format!("[{}] {}", e.code(), e.message()));
                    return Ok(EXIT_USAGE);
                }
            };
            let secrets = crate::security::create(&paths);
            let build = |credential: &SqlAdminCredential| {
                real_workflow(&paths, secrets.clone(), credential.clone(), tool.logger())
                    .map_err(|e| Box::new(e.to_exception()))
            };
            run_attempts(
                attempts,
                &build,
                &paths,
                &args,
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

/// The SQL bootstrap of the agent (`AddAgentCore`): local discovery, the setup identity,
/// the catalog fingerprint.
pub fn real_bootstrapper(
    secrets: SharedSecretStore,
    credential: SqlAdminCredential,
    log: &Logger,
) -> Result<SqlBootstrapper, Error> {
    let catalog = SyncCatalog::load()
        .map_err(|e| Error::unexpected("System.InvalidOperationException", e.to_string()))?;
    Ok(SqlBootstrapper::new(
        Arc::new(LocalSqlDiscovery::new(log)),
        Arc::new(SqlServerAdmin::new(credential)),
        SchemaFingerprint::new(catalog, DEFAULT_MINIMUM_SCORE),
        secrets,
        log,
    ))
}

/// The real workflow: SQL Server, the system clock, the agent's cloud client.
pub fn real_workflow(
    paths: &AgentPaths,
    secrets: SharedSecretStore,
    credential: SqlAdminCredential,
    log: &Logger,
) -> Result<EnrollWorkflow, Error> {
    let bootstrapper = real_bootstrapper(secrets.clone(), credential, log)?;
    let http_log = log.clone();
    let http: HttpFactory = Arc::new(move |url, key| {
        SyncHttpClient::new(
            url,
            SERVICE_TIMEOUT,
            Some(RequestSigner::new(key, SystemClock::shared())),
            RetryPolicy::default(),
            &http_log,
        )
    });
    Ok(EnrollWorkflow::new(EnrollParts {
        paths: paths.clone(),
        secrets,
        bootstrapper: Arc::new(bootstrapper),
        login_probe: Arc::new(SqlAgentLoginProbe),
        cloud_probe: Arc::new(HttpCloudProbe::create_default()),
        http,
        log: log.clone(),
        clock: SystemClock::shared(),
        // The dl-agent installer owns the service and its recovery policy; the C# one set
        // it on `PdaSyncAgent`, which must not be touched while the old agent is removed.
        service_recovery: Arc::new(|_: &Logger| {}),
    }))
}

/// The verb on a built workflow (tests run it against fakes); the exit code is the
/// workflow's alone. An unexpected failure is returned for the tool run to report.
pub fn run_workflow(
    workflow: &EnrollWorkflow,
    paths: &AgentPaths,
    args: &EnrollArgs,
    run: &ToolRun,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    ctx: &Ctx,
) -> Result<i32, Box<Exception>> {
    let machine = crate::diagnostics::machine_name();
    let result = workflow.run(args, &machine, &super::agent_version(), run, ctx);
    finish(result, paths, args, run, stdout, stderr)
}

/// The outcome of the workflow run that counts: status file, console lines, exit code.
pub fn finish(
    result: Result<EnrollResult, Error>,
    paths: &AgentPaths,
    args: &EnrollArgs,
    run: &ToolRun,
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
) -> Result<i32, Box<Exception>> {
    match result {
        Ok(result) => {
            for step in result.sql.iter().flat_map(|s| s.steps.iter()) {
                stdout.write_line(&format!("  sql step {}: {}", step.id, describe(step)));
            }
            if !args.dry_run {
                run.identity_changed();
                let id = result
                    .identity
                    .as_ref()
                    .map(crate::config::AgentIdentity::agent_id_text)
                    .unwrap_or_default();
                stdout.write_line(&format!(
                    "Enrolled as agent {id}. dl-agent runs it once modules.pda-sync is on for this machine in the hub config."
                ));
                return Ok(EXIT_OK);
            }
            stdout.write_line("Dry run complete; nothing was changed.");
            if result.preflight_ok() {
                stdout.write_line(PREFLIGHT_OK);
                return Ok(EXIT_OK);
            }
            let failed = result.failed_codes();
            if let Some(first) = failed.first() {
                run.record_failure(first);
            }
            stdout.write_line(&format!("{PREFLIGHT_FAILED}: {}", failed.join(", ")));
            Ok(EXIT_FAILED)
        }
        Err(Error::Agent(e)) => {
            let status = AgentStatus::new(
                "enroll_failed",
                Some(e.code()),
                Some(e.message()),
                SystemClock::shared().now(),
            );
            AgentStatus::write(paths, &status)
                .map_err(|io| Box::new(Exception::new("System.IO.IOException", io.to_string())))?;
            Ok(refuse(&e, run, stderr))
        }
        Err(Error::Unexpected(ex)) => Err(ex),
    }
}

/// A coded refusal: `warn` and exit 2 for usage problems, `error` and exit 1 otherwise.
pub fn refuse(e: &AgentError, run: &dyn RunStages, stderr: &dyn LineWriter) -> i32 {
    let usage = e.code() == codes::BAD_ARGUMENTS;
    run.fail(e.code(), e.message(), if usage { WARN } else { ERROR });
    stderr.write_line(&format!("[{}] {}", e.code(), e.message()));
    if usage { EXIT_USAGE } else { EXIT_FAILED }
}

/// `EnrollCommand.AdminCredential`: Windows by default; `--sql-admin-user` with its
/// password from [`PASSWORD_ENV_VAR`] or the prompt (masked in every log line and event).
pub fn admin_credential(
    opts: &Options,
    passwords: &dyn SecretPrompt,
) -> Result<SqlAdminCredential, AgentError> {
    let Some(user) = opts.get(SQL_ADMIN_USER_OPTION) else {
        return Ok(SqlAdminCredential::windows());
    };
    if user.trim().is_empty() || user == "true" {
        return Err(AgentError::new(
            codes::BAD_ARGUMENTS,
            format!("--{SQL_ADMIN_USER_OPTION} needs a login name."),
        ));
    }
    let password = passwords
        .read(
            PASSWORD_ENV_VAR,
            &format!("Password for SQL Server login '{user}': "),
        )
        .filter(|p| !p.is_empty());
    let Some(password) = password else {
        return Err(AgentError::new(
            codes::BAD_ARGUMENTS,
            format!(
                "--{SQL_ADMIN_USER_OPTION} needs a password: set {PASSWORD_ENV_VAR} for this command, or run it in a console \
                 to be asked. It is never taken from the command line."
            ),
        ));
    };
    crate::reporting::scrubber::register(Some(&password));
    Ok(SqlAdminCredential::sql(user, &password))
}

/// `EnrollCommand.ArgsFrom`.
pub fn args_from(opts: &Options) -> EnrollArgs {
    EnrollArgs {
        code: opts.get("code").map(str::to_owned),
        url: opts.get("url").map(str::to_owned),
        dry_run: opts.contains("dry-run"),
        sql_instance: opts.get("sql-instance").map(str::to_owned),
        database: opts.get("database").map(str::to_owned),
    }
}

/// `EnrollCommand.StartDetail`: `dry_run`, `has_code`, `has_url`.
pub fn start_detail(args: &EnrollArgs) -> Map {
    let has = |s: &Option<String>| s.as_deref().is_some_and(|s| !s.trim().is_empty());
    let mut d = Map::new();
    d.insert("dry_run", args.dry_run);
    d.insert("has_code", has(&args.code));
    d.insert("has_url", has(&args.url));
    d
}

/// How a step outcome reads on the console.
pub fn describe(step: &StepOutcome) -> &'static str {
    if step.executed {
        "applied"
    } else if step.skipped {
        "already present"
    } else {
        "would apply (dry-run)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosting::secret_prompt::FixedSecretPrompt;

    fn opts(user: Option<&str>) -> Options {
        let mut o = Options::default();
        o.insert("code", "c");
        if let Some(u) = user {
            o.insert(SQL_ADMIN_USER_OPTION, u);
        }
        o
    }

    #[test]
    fn without_the_option_the_setup_uses_windows_authentication_and_asks_for_nothing() {
        let prompt = FixedSecretPrompt(vec![(PASSWORD_ENV_VAR.into(), "unused".into())]);
        let c = admin_credential(&opts(None), &prompt).unwrap();
        assert_eq!(c, SqlAdminCredential::windows());
        assert!(!c.is_sql_login());
    }

    #[test]
    fn a_sql_administrator_login_takes_its_password_from_the_prompt_and_masks_it() {
        let password = format!("pw-{}", dl_uuid::Uuid::new_v4().simple());
        let prompt = FixedSecretPrompt(vec![(PASSWORD_ENV_VAR.into(), password.clone())]);
        let c = admin_credential(&opts(Some("sa")), &prompt).unwrap();
        assert_eq!(c, SqlAdminCredential::sql("sa", &password));
        assert_eq!(crate::reporting::scrub(&password), crate::reporting::MASK);
    }

    #[test]
    fn a_login_without_a_name_or_a_password_is_a_usage_error() {
        let cases: [(&str, Option<&str>, &str); 4] = [
            ("true", None, "needs a login name"),
            (" ", Some("x"), "needs a login name"),
            ("sa", None, "needs a password"),
            ("sa", Some(""), "needs a password"),
        ];
        for (user, password, expected) in cases {
            let prompt = FixedSecretPrompt(
                password
                    .map(|p| vec![(PASSWORD_ENV_VAR.to_owned(), p.to_owned())])
                    .unwrap_or_default(),
            );
            let e = admin_credential(&opts(Some(user)), &prompt).unwrap_err();
            assert_eq!(e.code(), codes::BAD_ARGUMENTS);
            assert!(e.message().contains(expected), "{user:?}: {}", e.message());
        }
    }

    #[test]
    fn steps_read_as_applied_present_or_planned() {
        let s = |skipped, executed| StepOutcome {
            id: "x".into(),
            skipped,
            executed,
        };
        assert_eq!(describe(&s(false, true)), "applied");
        assert_eq!(describe(&s(true, false)), "already present");
        assert_eq!(describe(&s(false, false)), "would apply (dry-run)");
    }
}
