//! Wires the agent together: state permissions -> executor -> keystore -> enrollment
//! (first run only) -> token source -> stream loop. The `pms-site` module calls [`run`] on
//! a worker thread.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::acl::{self, Guard, Outcome};
use crate::attrs;
use crate::bundle::{self, Keyring};
use crate::config::{self, Config};
use crate::conn::{self, ConnError, MigrationHandler};
use crate::ctx::Ctx;
use crate::db::{self, Pool, PoolsError};
use crate::enroll::{self, EnrollError, HostInfo, TokenProvider, TokenSource};
use crate::exec::{self, Executor, SchemaVersion, SqlExecutor};
use crate::keystore::{Identity, KeystoreError, Store};
use crate::log::Logger;
use crate::migrate::{self, Event, Runner, backup};
use crate::ops::Manifest;
use crate::remotemigrate::{self, Handler, HttpFetcher};
use crate::retention::{Purger, SqlStore};
use crate::telemetry::Telemetry;

/// Injectable parts; `None` takes the production default.
#[derive(Default)]
pub struct Deps {
    /// Logger.
    pub log: Option<Logger>,
    /// HTTP client.
    pub http: Option<Arc<dl_http::Client>>,
    /// Keystore.
    pub store: Option<Store>,
    /// Op executor.
    pub exec: Option<Arc<dyn Executor>>,
    /// Schema version provider.
    pub schema: Option<Arc<dyn SchemaVersion>>,
    /// State directory guard.
    pub guard: Option<Box<dyn Guard>>,
    /// Build version.
    pub version: String,
    /// Reconnect/retry back-off.
    pub backoff: Option<fn(u32) -> Duration>,
    /// Cloud-triggered migrations.
    pub migrator: Option<Arc<dyn MigrationHandler>>,
    /// Counters and reports for the module.
    pub telemetry: Option<Arc<Telemetry>>,
}

/// Why the agent ended.
#[derive(Debug)]
pub enum AgentError {
    /// The cloud refused this agent for good (clean stop, no restart).
    Stopped(ConnError),
    /// No identity and no enrollment code.
    NotEnrolled,
    /// The cloud refused the enrollment code (wrong, used, expired or another tenant's).
    EnrollRefused,
    /// Anything else.
    Other(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentError::Stopped(e) => e.fmt(f),
            AgentError::NotEnrolled => write!(
                f,
                "agent is not enrolled: set enrollment_code in the config file or {}",
                config::ENV_ENROLLMENT_CODE
            ),
            AgentError::EnrollRefused => f.write_str(
                "enrollment refused: the code is wrong, used, expired or for another tenant",
            ),
            AgentError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<KeystoreError> for AgentError {
    fn from(e: KeystoreError) -> AgentError {
        AgentError::Other(e.to_string())
    }
}

/// Superseded.
pub const STOP_SUPERSEDED: &str = "superseded";
/// Revoked.
pub const STOP_REVOKED: &str = "revoked";
/// Contract unsupported.
pub const STOP_CONTRACT_UNSUPPORTED: &str = "contract_unsupported";

/// What an operator does about a terminal stop.
pub fn next_step(reason: &str) -> &'static str {
    match reason {
        STOP_SUPERSEDED => {
            "another agent was enrolled for this site; stop or uninstall the other agent (SmilePmsAgent), then send pms.reconnect"
        }
        STOP_REVOKED => {
            "the agent was revoked in the console; issue a new enrollment code and run dl-agent pms-enroll --force with it"
        }
        _ => "the cloud needs a newer agent (close 4426); upgrade dl-agent",
    }
}

/// The stop reason of a terminal error.
pub fn stop_reason(err: &AgentError) -> Option<&'static str> {
    match err {
        AgentError::Stopped(ConnError::Superseded) => Some(STOP_SUPERSEDED),
        AgentError::Stopped(ConnError::Revoked) => Some(STOP_REVOKED),
        AgentError::Stopped(ConnError::ContractUnsupported) => Some(STOP_CONTRACT_UNSUPPORTED),
        _ => None,
    }
}

/// True for the errors that end the service cleanly (exit 0).
pub fn clean_stop(err: &AgentError) -> bool {
    stop_reason(err).is_some()
}

/// Logs a created path as info and a repaired one as a warning.
pub fn log_state_outcome(log: &Logger, kind: &str, path: &Path, out: Outcome) {
    let p = path.display().to_string();
    match out {
        Outcome::Repaired => log.warn(
            "agent.state.acl_repaired",
            attrs!["kind" => kind, "path" => p],
        ),
        Outcome::Created => log.info(
            "agent.state.acl_created",
            attrs!["kind" => kind, "path" => p],
        ),
        Outcome::Unchanged => {}
    }
}

/// Makes sure the state directory exists restricted and the config file is restricted,
/// repairing either when too open and refusing to start when that fails.
pub fn secure_state(g: &dyn Guard, cfg: &Config, log: &Logger) -> Result<(), AgentError> {
    if !cfg.state_dir.as_os_str().is_empty() {
        let out = acl::ensure_dir(g, &cfg.state_dir)
            .map_err(|e| AgentError::Other(format!("state directory permissions: {e}")))?;
        log_state_outcome(log, "state_dir", &cfg.state_dir, out);
    }
    if let Some(p) = &cfg.path {
        let out = acl::ensure_file(g, p)
            .map_err(|e| AgentError::Other(format!("config file permissions: {e}")))?;
        log_state_outcome(log, "config", p, out);
    }
    Ok(())
}

/// What the executor wiring produced.
pub struct ExecSetup {
    /// The executor.
    pub exec: Arc<dyn Executor>,
    /// The schema version provider.
    pub schema: Arc<dyn SchemaVersion>,
    /// The MM_PMS pool (`None` on the stub).
    pub pms: Option<Arc<Pool>>,
    /// Every pool, closed at the end.
    pub pools: db::Pools,
}

/// The SQL executor over `<state dir>/databases.json`, or the stub that refuses every op
/// when the site has no usable connection file yet (the agent still connects, so every
/// op is answered instead of timing out).
pub fn executor(cfg: &Config, log: &Logger) -> Result<ExecSetup, AgentError> {
    let stub = |reason: &str| {
        log.warn(
            "agent.exec.stub",
            attrs!["reason" => reason, "config" => db::DB_CONFIG_FILE],
        );
        Ok(ExecSetup {
            exec: Arc::new(exec::Stub),
            schema: Arc::new(exec::UnknownSchema),
            pms: None,
            pools: db::Pools::new(),
        })
    };
    if cfg.state_dir.as_os_str().is_empty() {
        return stub("no_state_dir");
    }
    // A build defect: the embedded manifest is checked by the tests.
    let manifest = Arc::new(Manifest::load().map_err(AgentError::Other)?);
    let pools = match db::open_pools(&cfg.state_dir.join(db::DB_CONFIG_FILE)) {
        Ok(p) => p,
        Err(PoolsError::NoConfig) => return stub("no_db_config"),
        Err(e) => {
            // The error names the file and database, never a connection string.
            log.error("agent.exec.db_config", attrs!["reason" => e.to_string()]);
            return stub("db_config_invalid");
        }
    };
    let names: Vec<String> = pools.keys().cloned().collect();
    log.info(
        "agent.exec.sql",
        attrs!["databases" => names, "ops_allowlist" => &manifest.allowlist_hash],
    );
    let pms = pools.get(db::DB_PMS).cloned();
    Ok(ExecSetup {
        exec: Arc::new(SqlExecutor {
            manifest,
            dbs: pools.clone(),
            limits: exec::Limits::default(),
            log: log.clone(),
        }),
        schema: Arc::new(migrate::ledger::LedgerVersion { pool: pms.clone() }),
        pms,
        pools,
    })
}

/// No bundle can be verified.
pub const ERR_NO_KEYRING: &str = "migration.keyring is not configured: no bundle can be verified";

/// The configured signing keys.
pub fn pinned_keyring(cfg: &Config) -> Result<Keyring, String> {
    match &cfg.migration.keyring {
        None => Err(ERR_NO_KEYRING.into()),
        Some(v) => bundle::parse_keyring(v),
    }
}

/// A runner over the MM_PMS pool from the agent config (CLI and cloud-triggered runs).
pub fn migration_runner(
    cfg: &Config,
    pool: Arc<Pool>,
    version: &str,
    rerun: i64,
    progress: Box<dyn Fn(Event) + Send + Sync>,
    log: &Logger,
) -> Result<Runner, String> {
    let gate = backup::new_backup_gate(&cfg.migration.backup, &cfg.state_dir)?;
    let mut o = migrate::Options::new(pool, version);
    o.backup = gate;
    o.rerun_partial = rerun;
    o.log = log.clone();
    o.progress = progress;
    o.legacy = migrate::LegacyDbs {
        fo: cfg.migration.legacy_fo_db.clone(),
        pos: cfg.migration.legacy_pos_db.clone(),
        bo: cfg.migration.legacy_bo_db.clone(),
    };
    Ok(Runner::new(o))
}

fn migrator(
    cfg: &Config,
    log: &Logger,
    version: &str,
    pms: Option<Arc<Pool>>,
    http: Arc<dl_http::Client>,
    tokens: Arc<dyn TokenProvider>,
) -> Option<Arc<dyn MigrationHandler>> {
    let pms = pms?;
    let keys = match pinned_keyring(cfg) {
        Ok(k) => k,
        Err(e) => {
            log.warn("agent.migrate.disabled", attrs!["reason" => e]);
            return None;
        }
    };
    let fetcher = HttpFetcher {
        cloud_url: cfg.cloud_url.clone(),
        http,
        tokens,
        max_bytes: crate::wire::MAX_BUNDLE_BYTES,
    };
    let (cfg2, log2, version) = (cfg.clone(), log.clone(), version.to_owned());
    let factory: remotemigrate::RunnerFactory = Box::new(move |rerun, progress| {
        migration_runner(&cfg2, pms.clone(), &version, rerun, progress, &log2)
            .map(|r| Box::new(r) as Box<dyn remotemigrate::Runner>)
    });
    Some(Arc::new(Handler::new(
        Some(Box::new(fetcher)),
        keys,
        Some(factory),
        log.clone(),
    )))
}

fn host_name() -> String {
    for k in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(k)
            && !v.is_empty()
        {
            return v;
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// `runtime.GOOS + "/" + runtime.GOARCH`.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        a => a,
    };
    format!("{os}/{arch}")
}

/// Redeems the code, retrying only transient failures (network, 429, 5xx). `Ok(None)`
/// when the context ended first.
fn enroll_once(
    ctx: &Ctx,
    cfg: &Config,
    log: &Logger,
    store: &Store,
    client: &enroll::Client,
    version: &str,
    backoff: fn(u32) -> Duration,
) -> Result<Option<Identity>, AgentError> {
    if cfg.enrollment_code.is_empty() {
        return Err(AgentError::NotEnrolled);
    }
    let info = HostInfo {
        hostname: host_name(),
        os: platform(),
        agent_version: version.to_owned(),
    };
    let mut attempt = 1u32;
    loop {
        match client.enroll(ctx, &cfg.enrollment_code, &info) {
            Ok(got) => {
                let id = Identity {
                    agent_id: got.agent_id,
                    site_id: got.site_id,
                    tenant_id: got.tenant_id,
                };
                store.save_identity(&id)?;
                log.info(
                    "agent.enrolled",
                    attrs!["agent_id" => &id.agent_id, "site_id" => &id.site_id],
                );
                if cfg.code_from_file
                    && let Some(p) = &cfg.path
                    && let Err(e) = config::scrub_enrollment_code(p)
                {
                    log.warn("agent.config.scrub_failed", attrs!["reason" => e]);
                }
                return Ok(Some(id));
            }
            Err(EnrollError::Refused) => return Err(AgentError::EnrollRefused),
            Err(e) => {
                let mut wait = backoff(attempt);
                if let Some(ra) = e.retry_after()
                    && ra > wait
                {
                    wait = ra;
                }
                log.warn(
                    "agent.enroll.retry",
                    attrs![
                        "attempt" => attempt,
                        "in_ms" => i64::try_from(wait.as_millis()).unwrap_or(i64::MAX),
                        "reason" => e.to_string()
                    ],
                );
                if !ctx.sleep(wait) {
                    return Ok(None);
                }
                attempt += 1;
            }
        }
    }
}

/// Blocks until `ctx` ends (`Ok`) or the agent must stop.
pub fn run(ctx: &Ctx, cfg: &Config, d: Deps) -> Result<(), AgentError> {
    let log = d.log.unwrap_or_else(Logger::discard);
    let http = d.http.unwrap_or_else(enroll::new_http_client);
    let backoff = d.backoff.unwrap_or(conn::backoff);
    let guard = d.guard.unwrap_or_else(acl::platform);
    secure_state(guard.as_ref(), cfg, &log)?;
    // After secure_state: databases.json lives in the restricted state directory.
    let setup = match d.exec {
        Some(e) => ExecSetup {
            exec: e,
            schema: d
                .schema
                .clone()
                .unwrap_or_else(|| Arc::new(exec::UnknownSchema)),
            pms: None,
            pools: db::Pools::new(),
        },
        None => executor(cfg, &log)?,
    };
    let schema = d.schema.unwrap_or_else(|| setup.schema.clone());
    let res = run_with(
        ctx,
        cfg,
        &log,
        http,
        backoff,
        d.store,
        setup.exec.clone(),
        schema,
        setup.pms.clone(),
        d.migrator,
        d.telemetry,
        &d.version,
    );
    db::close_all(&setup.pools);
    res
}

#[allow(clippy::too_many_arguments)]
fn run_with(
    ctx: &Ctx,
    cfg: &Config,
    log: &Logger,
    http: Arc<dl_http::Client>,
    backoff: fn(u32) -> Duration,
    store: Option<Store>,
    exec: Arc<dyn Executor>,
    schema: Arc<dyn SchemaVersion>,
    pms: Option<Arc<Pool>>,
    migrator_dep: Option<Arc<dyn MigrationHandler>>,
    telemetry: Option<Arc<Telemetry>>,
    version: &str,
) -> Result<(), AgentError> {
    let store = match store {
        Some(s) => s,
        None => Store::open(&cfg.state_dir)?,
    };
    let (key, created) = store.key()?;
    if created {
        log.info("agent.key.created", &[]);
    }
    let client = Arc::new(enroll::Client {
        base_url: cfg.cloud_url.clone(),
        http: http.clone(),
        key,
        now: dl_time::OffsetDateTime::now_utc,
    });
    let id = match store.identity() {
        Ok(id) => id,
        Err(KeystoreError::NoIdentity) => {
            match enroll_once(ctx, cfg, log, &store, &client, version, backoff)? {
                Some(id) => id,
                None => return Ok(()),
            }
        }
        Err(e) => return Err(e.into()),
    };
    if ctx.is_done() {
        return Ok(());
    }
    log.info(
        "agent.start",
        attrs!["agent_id" => &id.agent_id, "site_id" => &id.site_id, "version" => version],
    );
    let purge = pms.clone().map(|pool| {
        let pctx = ctx.child();
        let p = Purger::new(Box::new(SqlStore { pool }), log.clone());
        let c = pctx.clone();
        (pctx, std::thread::spawn(move || p.run(&c)))
    });
    let tokens: Arc<dyn TokenProvider> = Arc::new(TokenSource::new(client, &id.agent_id));
    let migrator =
        migrator_dep.or_else(|| migrator(cfg, log, version, pms, http.clone(), tokens.clone()));
    let mut o = conn::Options::new(&cfg.cloud_url, http, tokens, exec);
    o.schema = schema;
    o.log = log.clone();
    o.agent_version = version.to_owned();
    o.migrator = migrator;
    o.backoff = backoff;
    if let Some(t) = telemetry {
        o.telemetry = t;
    }
    let res = conn::Client::new(o).run(ctx).map_err(AgentError::Stopped);
    if let Some((pctx, h)) = purge {
        pctx.cancel();
        if h.join().is_err() {
            log.error("agent.retention.panicked", &[]);
        }
    }
    if let Err(e) = &res
        && let Some(reason) = stop_reason(e)
    {
        log.error(
            "agent.stopped",
            attrs![
                "reason" => reason,
                "detail" => e.to_string(),
                "next_step" => next_step(reason)
            ],
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reasons_and_platform() {
        assert_eq!(
            stop_reason(&AgentError::Stopped(ConnError::Superseded)),
            Some(STOP_SUPERSEDED)
        );
        assert!(clean_stop(&AgentError::Stopped(ConnError::Revoked)));
        assert!(!clean_stop(&AgentError::Stopped(ConnError::Other(
            "x".into()
        ))));
        assert!(!clean_stop(&AgentError::NotEnrolled));
        assert!(platform().contains('/'));
    }
}
