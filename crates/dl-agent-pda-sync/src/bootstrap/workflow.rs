//! `enroll --code XXXX --url https://...` (port of `Bootstrap/EnrollWorkflow.cs`): the SQL
//! preflight and bootstrap first, then a sign-in as `pda_sync` (so a SQL problem never
//! burns the one-time code), then the key (an existing one is reused), the code
//! redemption and the persistence. `--dry-run` changes nothing and redeems nothing: it
//! reports every finding, the cloud probe included.

use super::bootstrapper::{STEPS, SqlBootstrapper, throw_if_blocked};
use super::cloud_probe::{CloudProbe, CloudProbeResult};
use super::error::{Error, Result};
use super::models::{BootstrapReport, StepKind};
use super::preflight::{PreflightFinding, SqlPreflight};
use super::probes::AgentLoginProbe;
use crate::config::time::SharedClock;
use crate::config::{AgentError, AgentIdentity, AgentPaths, AgentStatus, codes};
use crate::contracts::time::TICKS_PER_SECOND;
use crate::contracts::wire::site_event_levels::{INFO, WARN};
use crate::reporting::log::{Exception, Logger, Value};
use crate::reporting::run_stages::RunStages;
use crate::security::{AgentKey, KeyError, SharedSecretStore, names};
use crate::transport::{EnrollmentClient, SyncHttpClient};
use crate::util::ctx::Ctx;
use dl_json::Map;
use std::sync::Arc;
use std::time::Duration;

/// `ILogger<EnrollWorkflow>`.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Bootstrap.EnrollWorkflow";
/// The database was located (every run).
pub const SQL_LOCATED: &str = "ENROLL_SQL_LOCATED";
/// The bootstrap ran.
pub const SQL_BOOTSTRAPPED: &str = "ENROLL_SQL_BOOTSTRAPPED";
/// The dry run planned the bootstrap.
pub const SQL_PLANNED: &str = "ENROLL_SQL_PLANNED";
/// `pda_sync` signed in and read.
pub const LOGIN_VERIFIED: &str = "ENROLL_LOGIN_VERIFIED";
/// The dry run probed the cloud.
pub const CLOUD_PROBED: &str = "ENROLL_CLOUD_PROBED";
/// The code was redeemed.
pub const REDEEMED: &str = "ENROLL_REDEEMED";
/// The usage line of a real run without code or URL.
pub const USAGE: &str = "Usage: enroll --code <code> --url <https://host> [--dry-run]";

/// The inputs of `enroll` (`EnrollArgs`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrollArgs {
    /// `--code`.
    pub code: Option<String>,
    /// `--url`.
    pub url: Option<String>,
    /// `--dry-run`.
    pub dry_run: bool,
    /// `--sql-instance`.
    pub sql_instance: Option<String>,
    /// `--database`.
    pub database: Option<String>,
}

/// What `enroll` did (`EnrollResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct EnrollResult {
    /// The bootstrap (or, in a dry run, its plan); `None` when the dry run could not
    /// locate the database.
    pub sql: Option<BootstrapReport>,
    /// The new identity (real run).
    pub identity: Option<AgentIdentity>,
    /// Preflight warnings (real run) or every preflight finding (dry run).
    pub findings: Vec<PreflightFinding>,
    /// The dry run's cloud probe; `None` when no URL was known or in a real run.
    pub cloud: Option<CloudProbeResult>,
}

impl EnrollResult {
    /// No blocking finding.
    pub fn preflight_ok(&self) -> bool {
        self.findings.iter().all(|f| !f.is_error())
    }

    /// The codes of the blocking findings, once each, in order.
    pub fn failed_codes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in self.findings.iter().filter(|f| f.is_error()) {
            if !out.contains(&f.code) {
                out.push(f.code.clone());
            }
        }
        out
    }
}

/// Builds the signed cloud client for a base URL and the agent key.
pub type HttpFactory = Arc<
    dyn Fn(&str, Arc<AgentKey>) -> std::result::Result<SyncHttpClient, crate::transport::Error>
        + Send
        + Sync,
>;

/// Applies the service recovery policy (`WindowsServiceRecovery.TryConfigure`).
pub type ServiceRecovery = Arc<dyn Fn(&Logger) + Send + Sync>;

/// The parts of the workflow.
pub struct EnrollParts {
    /// The data folder.
    pub paths: AgentPaths,
    /// The secret store.
    pub secrets: SharedSecretStore,
    /// The SQL bootstrap.
    pub bootstrapper: Arc<SqlBootstrapper>,
    /// The `pda_sync` sign-in check.
    pub login_probe: Arc<dyn AgentLoginProbe>,
    /// The dry run's cloud probe.
    pub cloud_probe: Arc<dyn CloudProbe>,
    /// The cloud client factory.
    pub http: HttpFactory,
    /// The root logger.
    pub log: Logger,
    /// The clock of `status.json`.
    pub clock: SharedClock,
    /// The service recovery policy (tests pass a no-op).
    pub service_recovery: ServiceRecovery,
}

/// The enrollment (`EnrollWorkflow`).
pub struct EnrollWorkflow {
    p: EnrollParts,
    log: Logger,
}

impl std::fmt::Debug for EnrollWorkflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrollWorkflow")
            .field("paths", &self.p.paths)
            .finish_non_exhaustive()
    }
}

fn io_error(e: &std::io::Error) -> Error {
    let ty = match e.kind() {
        std::io::ErrorKind::PermissionDenied => "System.UnauthorizedAccessException",
        _ => "System.IO.IOException",
    };
    Error::unexpected(ty, e.to_string())
}

/// `Math.Round(x, digits)` (banker's rounding).
fn round_even(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round_ties_even() / f
}

/// `FormatSkew`: `+3 s`, `-1.5 s` (`0.#`).
pub fn format_skew(ticks: i64) -> String {
    let sign = if ticks < 0 { "-" } else { "+" };
    let secs = ticks.unsigned_abs() as f64 / TICKS_PER_SECOND as f64;
    let r = (secs * 10.0).round() / 10.0;
    let text = if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r:.1}")
    };
    format!("{sign}{text} s")
}

/// `Uri.GetLeftPart(UriPartial.Authority)`: `scheme://host[:port]`, the default port
/// left out.
pub fn left_part(url: &dl_http::Url) -> String {
    let scheme = url.scheme();
    let host = if url.is_ipv6() {
        format!("[{}]", url.host())
    } else {
        url.host().to_ascii_lowercase()
    };
    if url.port() == scheme.default_port() {
        format!("{}://{host}", scheme.as_str())
    } else {
        format!("{}://{host}:{}", scheme.as_str(), url.port())
    }
}

fn blank(s: Option<&str>) -> bool {
    s.is_none_or(|s| s.trim().is_empty())
}

impl EnrollWorkflow {
    /// A workflow over `parts`.
    pub fn new(parts: EnrollParts) -> EnrollWorkflow {
        let log = parts.log.for_category(CATEGORY);
        EnrollWorkflow { p: parts, log }
    }

    /// Runs `enroll` for this machine (`RunAsync`).
    pub fn run(
        &self,
        args: &EnrollArgs,
        machine_id: &str,
        agent_version: &str,
        stages: &dyn RunStages,
        ctx: &Ctx,
    ) -> Result<EnrollResult> {
        self.p.paths.ensure_created().map_err(|e| io_error(&e))?;
        let existing = AgentIdentity::try_load(&self.p.paths)
            .map_err(|e| Error::unexpected("System.Text.Json.JsonException", e.to_string()))?;
        let url = args
            .url
            .clone()
            .or_else(|| existing.as_ref().map(|i| i.cloud_url.clone()));
        if args.dry_run {
            return self.preflight_only(args, url.as_deref(), existing.as_ref(), stages, ctx);
        }
        let base = match (&args.code, &url) {
            (code, Some(u)) if !blank(code.as_deref()) && !blank(Some(u)) => {
                dl_http::Url::parse(u).ok()
            }
            _ => None,
        };
        let (Some(code), Some(base)) = (args.code.as_deref(), base) else {
            return Err(AgentError::new(codes::BAD_ARGUMENTS, USAGE).into());
        };
        crate::reporting::scrubber::register(Some(code));

        let preflight = self.locate(args, existing.as_ref(), stages, ctx)?;
        let warnings: Vec<PreflightFinding> = preflight
            .findings
            .iter()
            .filter(|f| !f.is_error())
            .cloned()
            .collect();
        report(&warnings, stages);
        throw_if_blocked(&preflight)?;
        let sql = self.p.bootstrapper.apply(&preflight, false, ctx)?;
        let mut d = Map::new();
        d.insert(
            "applied",
            sql.steps.iter().filter(|s| s.executed).count() as i64,
        );
        d.insert(
            "skipped",
            sql.steps.iter().filter(|s| s.skipped).count() as i64,
        );
        d.insert("dry_run", false);
        stages.stage(
            INFO,
            SQL_BOOTSTRAPPED,
            "POS database bootstrap applied",
            Some(d),
        );

        self.verify_login(&sql, stages, ctx)?;

        let key = match AgentKey::load_or_create(&*self.p.secrets) {
            Ok((key, created)) => {
                if created {
                    self.log.info("Generated a new agent key", &[]);
                } else {
                    self.log.info("Reusing the stored agent key", &[]);
                }
                Arc::new(key)
            }
            Err(KeyError::Random(e)) => {
                return Err(Error::unexpected(
                    "System.Security.Cryptography.CryptographicException",
                    e.to_string(),
                ));
            }
            Err(KeyError::Store(e)) if e.agent_error().is_some() => {
                return Err(Error::from_secret(&e));
            }
            Err(e) => {
                return Err(AgentError::with_source(
                    codes::KEY_STORE_FAILED,
                    "Cannot read or write the agent key store.",
                    e,
                )
                .into());
            }
        };

        let http = (self.p.http)(&base.to_string(), key.clone()).map_err(transport_error)?;
        let client = EnrollmentClient::new(&http, &key, &self.p.log);
        let redeemed = client
            .redeem(ctx, code, machine_id, agent_version)
            .map_err(transport_error)?;
        crate::reporting::scrubber::register(Some(&redeemed.agent_token));

        self.p
            .secrets
            .save(names::AGENT_TOKEN, redeemed.agent_token.as_bytes())
            .map_err(|e| Error::from_secret(&e))?;
        let identity = AgentIdentity {
            agent_id: redeemed.agent_id,
            tenant_id: redeemed.tenant_id.clone(),
            cloud_url: left_part(&base),
            machine_id: machine_id.to_owned(),
            sql_instance: Some(sql.instance.server_name.clone()),
            sql_database: Some(sql.database.clone()),
        };
        identity.save(&self.p.paths).map_err(|e| io_error(&e))?;
        AgentStatus::write(
            &self.p.paths,
            &AgentStatus::new("enrolled", None, None, self.p.clock.now()),
        )
        .map_err(|e| io_error(&e))?;
        let mut d = Map::new();
        d.insert("agent_id", identity.agent_id_text().as_str());
        d.insert("tenant", redeemed.tenant_id.as_str());
        stages.stage(
            INFO,
            REDEEMED,
            &format!("Enrolled as agent {}", identity.agent_id_text()),
            Some(d),
        );
        (self.p.service_recovery)(&self.log);
        Ok(EnrollResult {
            sql: Some(sql),
            identity: Some(identity),
            findings: warnings,
            cloud: None,
        })
    }

    /// Reads only: the SQL preflight and plan, the existing login when there is one, and
    /// the cloud probe.
    fn preflight_only(
        &self,
        args: &EnrollArgs,
        url: Option<&str>,
        existing: Option<&AgentIdentity>,
        stages: &dyn RunStages,
        ctx: &Ctx,
    ) -> Result<EnrollResult> {
        let mut findings = Vec::new();
        let mut sql = None;
        match self.plan(args, existing, &mut findings, stages, ctx) {
            Ok(plan) => sql = Some(plan),
            Err(Error::Agent(e)) => findings.push(PreflightFinding::from_agent(&e)),
            Err(e) => return Err(e),
        }

        let mut cloud = None;
        match url {
            None => stages.line(
                "Cloud not probed: add --url <https://host> to check reachability and the clock.",
            ),
            Some(u) if u.trim().is_empty() => stages.line(
                "Cloud not probed: add --url <https://host> to check reachability and the clock.",
            ),
            Some(u) => match dl_http::Url::parse(u) {
                Err(_) => findings.push(PreflightFinding::error(
                    codes::BAD_ARGUMENTS,
                    format!("--url '{u}' is not an absolute http(s) URL."),
                )),
                Ok(base) => {
                    let result = self.p.cloud_probe.probe(&base.to_string(), ctx);
                    findings.extend(result.findings.iter().cloned());
                    let host = base.host().to_owned();
                    let skew_text = result
                        .clock_skew
                        .map(|s| format!(", clock skew {}", format_skew(s)))
                        .unwrap_or_default();
                    stages.line(&match result.http_status {
                        Some(status) => format!("Cloud {host}: HTTP {status}{skew_text}"),
                        None => format!("Cloud {host}: no answer"),
                    });
                    let mut d = Map::new();
                    d.insert("host", host.as_str());
                    d.insert("ok", result.ok());
                    if let Some(st) = result.http_status {
                        d.insert("http_status", i64::from(st));
                    }
                    if let Some(skew) = result.clock_skew {
                        let secs = skew as f64 / TICKS_PER_SECOND as f64;
                        d.insert("skew_seconds", round_even(secs, 1));
                    }
                    stages.stage(
                        if result.ok() { INFO } else { WARN },
                        CLOUD_PROBED,
                        &format!("Cloud probe of {host}"),
                        Some(d),
                    );
                    cloud = Some(result);
                }
            },
        }
        report(&findings, stages);
        Ok(EnrollResult {
            sql,
            identity: None,
            findings,
            cloud,
        })
    }

    /// The dry run's SQL part: locate, plan, and check an existing login.
    fn plan(
        &self,
        args: &EnrollArgs,
        existing: Option<&AgentIdentity>,
        findings: &mut Vec<PreflightFinding>,
        stages: &dyn RunStages,
        ctx: &Ctx,
    ) -> Result<BootstrapReport> {
        let preflight = self.locate(args, existing, stages, ctx)?;
        findings.extend(preflight.findings.iter().cloned());
        let sql = self.p.bootstrapper.apply(&preflight, true, ctx)?;
        let mut d = Map::new();
        d.insert(
            "pending",
            sql.steps.iter().filter(|s| !s.skipped).count() as i64,
        );
        d.insert(
            "skipped",
            sql.steps.iter().filter(|s| s.skipped).count() as i64,
        );
        d.insert("dry_run", true);
        stages.stage(
            INFO,
            SQL_PLANNED,
            "POS database bootstrap planned (dry run)",
            Some(d),
        );
        if !preflight.blocked() {
            self.try_verify_existing_login(&sql, findings, stages, ctx)?;
        }
        Ok(sql)
    }

    /// Locates the database and reads the server; prints and reports what it found.
    fn locate(
        &self,
        args: &EnrollArgs,
        existing: Option<&AgentIdentity>,
        stages: &dyn RunStages,
        ctx: &Ctx,
    ) -> Result<SqlPreflight> {
        let instance = args
            .sql_instance
            .as_deref()
            .or_else(|| existing.and_then(|i| i.sql_instance.as_deref()));
        let database = args
            .database
            .as_deref()
            .or_else(|| existing.and_then(|i| i.sql_database.as_deref()));
        let preflight = self.p.bootstrapper.preflight(instance, database, ctx)?;
        let describe = preflight.describe();
        stages.line(&describe);
        let s = &preflight.server;
        let mut d = Map::new();
        d.insert("instance", preflight.instance.server_name.as_str());
        d.insert("database", preflight.database.as_str());
        d.insert("sql_version", s.product_version.as_str());
        d.insert("sql_level", s.product_level.as_str());
        d.insert("sql_edition", s.edition.as_str());
        d.insert("sql_major", i64::from(s.major()));
        d.insert("integrated_security_only", s.integrated_security_only);
        d.insert("schema_match", round_even(preflight.fingerprint_score, 3));
        d.insert("dry_run", args.dry_run);
        stages.stage(INFO, SQL_LOCATED, &describe, Some(d));
        Ok(preflight)
    }

    fn stored_sql_password(&self) -> Result<Option<String>> {
        self.p
            .secrets
            .load(names::SQL_PASSWORD)
            .map(|v| v.map(|b| String::from_utf8_lossy(&b).into_owned()))
            .map_err(|e| Error::from_secret(&e))
    }

    fn verify_login(&self, sql: &BootstrapReport, stages: &dyn RunStages, ctx: &Ctx) -> Result<()> {
        let Some(password) = self.stored_sql_password()? else {
            return Err(AgentError::new(
                codes::SQL_LOGIN_VERIFY_FAILED,
                "The pda_sync password is not in the agent secret store, so the login cannot be checked. Run enroll again. The enrollment code was not used.",
            )
            .into());
        };
        self.p
            .login_probe
            .verify(&sql.instance, &sql.database, &password, ctx)?;
        stages.line(&format!("pda_sync signs in and reads {}.", sql.database));
        stages.stage(
            INFO,
            LOGIN_VERIFIED,
            "pda_sync signed in and read the captured and agent tables",
            None,
        );
        Ok(())
    }

    /// Dry run: checks the login only when it already exists and its password is held.
    fn try_verify_existing_login(
        &self,
        plan: &BootstrapReport,
        findings: &mut Vec<PreflightFinding>,
        stages: &dyn RunStages,
        ctx: &Ctx,
    ) -> Result<()> {
        let login = STEPS
            .iter()
            .find(|s| s.kind == StepKind::Login)
            .map_or("login", |s| s.id);
        let exists_with_secret = plan.steps.iter().any(|s| s.id == login && s.skipped);
        let tables_exist = plan.steps.iter().all(|s| s.skipped);
        if !exists_with_secret || !tables_exist {
            stages.line(
                "pda_sync login check: done by the real run, after it creates the login and the agent tables.",
            );
            return Ok(());
        }
        match self.verify_login(plan, stages, ctx) {
            Ok(()) => Ok(()),
            Err(Error::Agent(e)) => {
                findings.push(PreflightFinding::from_agent(&e));
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

fn transport_error(e: crate::transport::Error) -> Error {
    match e {
        crate::transport::Error::Agent(a) => Error::Agent(a),
        crate::transport::Error::Cancelled => Error::Unexpected(Box::new(Exception::new(
            "System.Threading.Tasks.TaskCanceledException",
            "A task was canceled.",
        ))),
        other => Error::Unexpected(Box::new(other.to_exception())),
    }
}

fn report(findings: &[PreflightFinding], stages: &dyn RunStages) {
    for f in findings {
        stages.line(&format!("  [{}] {}", f.code, f.message));
        stages.progress(&f.level, &f.code, &f.message, None);
    }
}

/// The Windows service name (`WindowsServiceRecovery.ServiceName`).
pub const SERVICE_NAME: &str = "PdaSyncAgent";

/// `WindowsServiceRecovery.TryConfigure`: the Service Control Manager restarts the agent
/// after a crash (5 s, 15 s, then 60 s; the count resets after a day). Idempotent and
/// never fatal; nothing off Windows. The C# agent runs `sc.exe failure`; this sets the
/// same policy through the SCM API, and logs a refusal with the Win32 error `sc.exe`
/// would have exited with.
pub fn try_configure_service_recovery(log: &Logger) {
    if !cfg!(windows) {
        return;
    }
    use dl_winsvc::scm::{Manager, RecoveryAction};
    let applied = Manager::connect()
        .and_then(|m| m.open(SERVICE_NAME))
        .and_then(|s| {
            s.set_recovery_actions(
                &[
                    RecoveryAction::Restart(Duration::from_secs(5)),
                    RecoveryAction::Restart(Duration::from_secs(15)),
                    RecoveryAction::Restart(Duration::from_secs(60)),
                ],
                Duration::from_secs(86_400),
            )
        });
    match applied {
        Ok(()) => {}
        Err(e) => match e.code() {
            Some(code) => log.warn(
                "Service recovery policy not applied (sc.exe exit {ExitCode}); the service may not be installed yet",
                &[Value::from(i64::from(code))],
            ),
            None => log.warn_ex(
                Exception::new("System.ComponentModel.Win32Exception", e.to_string()),
                "Could not configure the service recovery policy",
                &[],
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skews_and_urls_format_like_dotnet() {
        assert_eq!(format_skew(3 * TICKS_PER_SECOND), "+3 s");
        assert_eq!(format_skew(-15 * TICKS_PER_SECOND / 10), "-1.5 s");
        assert_eq!(format_skew(0), "+0 s");
        assert_eq!(round_even(0.25, 1), 0.2);
        assert_eq!(round_even(0.912_345, 3), 0.912);
        let u = |s: &str| left_part(&dl_http::Url::parse(s).unwrap());
        assert_eq!(u("http://127.0.0.1:5000/"), "http://127.0.0.1:5000");
        assert_eq!(u("https://Cloud.Example:443/x/y"), "https://cloud.example");
        assert_eq!(u("http://cloud.example:8080"), "http://cloud.example:8080");
    }
}
