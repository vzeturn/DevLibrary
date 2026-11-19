//! The module's self-test (`dl-agent selftest`, the `selftest` command): the PMS config and
//! identity are where the module's settings point, `databases.json` opens and every
//! database answers, the cloud accepts the agent's signed key, and the read op
//! `pos.checkVersion.get` runs through the local executor. Read only: no op that writes,
//! no enrollment, no key is created.

use std::path::Path;
use std::sync::Arc;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::NAME;
use super::prepare::{Prepared, prepare};
use super::settings::Settings;
use crate::config::Config;
use crate::ctx::Ctx;
use crate::db::{self, Pools, PoolsError};
use crate::enroll::{self, EnrollError};
use crate::exec::{Executor, Limits, SqlExecutor};
use crate::keystore::{IDENTITY_FILE, Store};
use crate::log::Logger;
use crate::ops::Manifest;
use crate::wire::{self, Op};

/// The read op the self-test runs: an absent check, so the answer carries no guest data.
pub const PROBE_OP: &str = "pos.checkVersion.get";
const KEY_FILE: &str = "agent.key";
const REENROLL: &str = "enroll again: dl-agent pms-enroll --code <code>";

/// The self-test to register with `App::selftest`.
pub fn pms_site_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    let settings = Settings::from_config(ctx.config());
    let title = "Enroll config present";
    let cfg = match prepare(&settings) {
        Prepared::Waiting {
            detail,
            invalid: false,
        } => return vec![Check::skip(NAME, "config", title, &detail)],
        Prepared::Waiting {
            detail,
            invalid: true,
        } => {
            let o = Outcome::fail(detail).remedy("fix the PMS agent config file the detail names");
            return vec![Check::timed(NAME, "config", title, || o)];
        }
        Prepared::Ready(cfg) => cfg,
    };
    let mut checks = vec![Check::timed(NAME, "config", title, || config(&cfg))];
    let cfg = Arc::new(*cfg);
    checks.push(cloud(&cfg));
    let pools = match db::open_pools(&cfg.state_dir.join(db::DB_CONFIG_FILE)) {
        Ok(p) => p,
        Err(e) => {
            let missing = matches!(e, PoolsError::NoConfig);
            let o = Outcome::fail(if missing {
                format!("no {} in {}", db::DB_CONFIG_FILE, cfg.state_dir.display())
            } else {
                e.to_string()
            })
            .remedy(
                "write databases.json with dl-agent pms-migrate (or copy it from SmilePmsAgent)",
            );
            checks.push(Check::timed(
                NAME,
                "databases",
                "databases.json opens",
                || o,
            ));
            return checks;
        }
    };
    checks.push(Check::timed(
        NAME,
        "databases",
        "databases.json opens",
        || {
            let names: Vec<&str> = pools.keys().map(String::as_str).collect();
            Outcome::pass(format!("{} database(s): {}", names.len(), names.join(", ")))
        },
    ));
    for (name, pool) in &pools {
        let pool = Arc::clone(pool);
        let title = format!("{name} connects");
        checks.push(Check::bounded(
            NAME,
            &format!("db.{name}"),
            &title,
            PROBE_TIMEOUT * 2,
            move || {
                let ctx = Ctx::background().with_timeout(PROBE_TIMEOUT);
                let result = pool.get(&ctx).and_then(|mut c| c.batch(&ctx, "SELECT 1"));
                match result {
                    Ok(_) => Outcome::pass("SELECT 1 answered"),
                    Err(e) => Outcome::fail(e.to_string())
                        .remedy("check SQL Server and the login in databases.json"),
                }
            },
        ));
    }
    checks.push(op(&pools));
    db::close_all(&pools);
    checks
}

fn config(cfg: &Config) -> Outcome {
    let file = cfg
        .path
        .as_deref()
        .map_or_else(|| "(environment)".into(), |p| p.display().to_string());
    let state = cfg.state_dir.display();
    if !cfg.state_dir.join(IDENTITY_FILE).exists() {
        return Outcome::warn(format!(
            "{file}; state {state}: enrollment code waiting, the service enrolls on start"
        ));
    }
    Outcome::pass(format!("{file}; state {state}; {IDENTITY_FILE} present"))
}

fn identity_key(state: &Path) -> Result<(Store, String), Outcome> {
    if !state.join(KEY_FILE).exists() {
        return Err(
            Outcome::fail(format!("{KEY_FILE} missing in {}", state.display())).remedy(REENROLL),
        );
    }
    let store = Store::open(state).map_err(|e| Outcome::fail(e.to_string()))?;
    let id = store
        .identity()
        .map_err(|e| Outcome::fail(e.to_string()).remedy(REENROLL))?;
    Ok((store, id.agent_id))
}

/// A signed token request: the cloud is reachable and still knows this agent.
fn cloud(cfg: &Arc<Config>) -> Check {
    let title = "Cloud accepts the agent key";
    if !cfg.state_dir.join(IDENTITY_FILE).exists() {
        return Check::skip(NAME, "cloud", title, "not enrolled yet");
    }
    let cfg = Arc::clone(cfg);
    Check::bounded(NAME, "cloud", title, PROBE_TIMEOUT * 2, move || {
        let (store, agent_id) = match identity_key(&cfg.state_dir) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let key = match store.key() {
            Ok((k, _)) => k,
            Err(e) => return Outcome::fail(e.to_string()).remedy(REENROLL),
        };
        let client = enroll::Client {
            base_url: cfg.cloud_url.clone(),
            http: Arc::new(dl_http::Client::with_options(dl_http::ClientOptions {
                timeout: Some(PROBE_TIMEOUT),
                connect_timeout: Some(PROBE_TIMEOUT.min(std::time::Duration::from_secs(10))),
                user_agent: Some(format!("dl-agent/{}", dl_agent::agent_version())),
                ..dl_http::ClientOptions::default()
            })),
            key,
            now: dl_time::OffsetDateTime::now_utc,
        };
        let ctx = Ctx::background().with_timeout(PROBE_TIMEOUT);
        match client.token(&ctx, &agent_id) {
            Ok(_) => Outcome::pass(format!(
                "{} issued a token for agent {agent_id}",
                cfg.cloud_url
            )),
            Err(EnrollError::Refused) => {
                Outcome::fail(format!("{} refused agent {agent_id}", cfg.cloud_url))
                    .remedy(format!("the agent was revoked or replaced: {REENROLL}"))
            }
            Err(e) => Outcome::fail(format!("{}: {e}", cfg.cloud_url))
                .remedy("check the network or proxy to the cloud"),
        }
    })
}

/// `pos.checkVersion.get` for a check that does not exist, through the real executor.
fn op(pools: &Pools) -> Check {
    let title = "Read op runs locally";
    let manifest = match Manifest::load() {
        Ok(m) => Arc::new(m),
        Err(e) => return Check::timed(NAME, "op", title, || Outcome::fail(e)),
    };
    let exec = SqlExecutor {
        manifest,
        dbs: pools.clone(),
        limits: Limits::default(),
        log: Logger::discard(),
    };
    Check::bounded(NAME, "op", title, PROBE_TIMEOUT * 2, move || {
        let op = Op {
            id: dl_uuid::Uuid::new_v4().to_string(),
            op: PROBE_OP.into(),
            contract_ver: 1,
            args: dl_json::json!({"checkNo": 0, "rvcNo": 0}),
            request_key: None,
            expected_stamp: None,
            deadline_ms: i64::try_from(PROBE_TIMEOUT.as_millis()).unwrap_or(15_000),
        };
        let ctx = Ctx::background().with_timeout(PROBE_TIMEOUT);
        let r = exec.execute(&ctx, &op);
        let error = r
            .error
            .as_ref()
            .map(|e| format!("{} {}", e.code, e.message))
            .unwrap_or_default();
        match r.status.as_str() {
            wire::STATUS_APPLIED => Outcome::pass(format!(
                "{PROBE_OP} applied ({} result set(s))",
                r.sets.len()
            )),
            wire::STATUS_REJECTED => Outcome::pass(format!(
                "{PROBE_OP} ran and was rejected as expected: {error}"
            )),
            _ => Outcome::fail(format!("{PROBE_OP} failed: {error}"))
                .remedy("check that MM_PMS has the agent procedures (dl-agent pms-migrate)"),
        }
    })
}

#[cfg(test)]
mod tests {
    use dl_agent::selftest::Status;
    use dl_json::json;

    use super::*;

    #[test]
    fn a_site_without_the_pms_config_is_not_enrolled() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("pms-st-{}-{nanos}", std::process::id()));
        let path = dir.join("bench").join("config.json");
        let cfg = json!({"config_path": path.display().to_string()});
        let checks = run(&ModuleContext::standalone(NAME, cfg, dir.clone()));
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Skip);
        assert!(checks[0].detail.contains("bench"), "{}", checks[0].detail);
        assert!(!dir.exists(), "the self-test created nothing");
    }
}
