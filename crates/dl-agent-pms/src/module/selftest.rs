//! The module's self-test (`dl-agent selftest`, the `selftest` and `pms.selftest`
//! commands): the PMS config and identity are where the module's settings point, the SMILE
//! databases are configured (`sql.json` through the core, then `databases.json`) and each
//! answers with its auth mode, the op journal is healthy, the cloud accepts the agent's
//! signed key, and the read op `fo.hotelDate.get` runs through the local executor. Read
//! only: no op that writes, no enrollment, no key, no journal is created.

use std::path::Path;
use std::sync::Arc;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::NAME;
use super::prepare::{Prepared, prepare};
use super::settings::Settings;
use crate::config::Config;
use crate::ctx::Ctx;
use crate::db::{self, Pool, Pools, Source};
use crate::enroll::{self, EnrollError};
use crate::exec::{Executor, Limits, SqlExecutor};
use crate::journal;
use crate::keystore::{IDENTITY_FILE, Store};
use crate::log::Logger;
use crate::ops::Manifest;
use crate::wire::{self, Op};

/// The read op the self-test runs: the hotel date, no guest data.
pub const PROBE_OP: &str = "fo.hotelDate.get";
const KEY_FILE: &str = "agent.key";
const REENROLL: &str = "enroll again: dl-agent pms-enroll --code <code>";
const DB_REMEDY: &str = "set the SMILE databases with dl-agent sql-setup (sql.json), or list SMILE_FO, SMILE_POS and SMILE_BO in databases.json";
/// A journal this full (percent of either cap) is a warning.
const JOURNAL_WARN_PCT: u64 = 80;

/// The self-test to register with `App::selftest`.
pub fn pms_site_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

/// Runs every row (the `selftest` command calls it too).
pub fn run(ctx: &ModuleContext) -> Vec<Check> {
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
    checks.push(Check::timed(NAME, "journal", "Op journal healthy", || {
        journal_row(&cfg.state_dir)
    }));
    let dbs_title = "SMILE databases configured";
    let site = match db::open(
        &db::shared_pools(ctx),
        &cfg.state_dir.join(db::DB_CONFIG_FILE),
    ) {
        Ok(s) => s,
        Err(e) => {
            let o = Outcome::fail(e.to_string()).remedy(DB_REMEDY);
            checks.push(Check::timed(NAME, "databases", dbs_title, || o));
            return checks;
        }
    };
    checks.push(Check::timed(NAME, "databases", dbs_title, || {
        databases(&site.pools, &site.ignored)
    }));
    for (name, pool) in &site.pools {
        checks.push(connects(name, Arc::clone(pool)));
    }
    checks.push(op(&site.pools));
    db::close_all(&site.pools);
    checks
}

fn databases(pools: &Pools, ignored: &[String]) -> Outcome {
    if pools.is_empty() {
        return Outcome::fail("no SMILE database configured: every op fails DB_UNAVAILABLE")
            .remedy(DB_REMEDY);
    }
    let names: Vec<String> = pools
        .values()
        .map(|p| format!("{} ({}, auth {})", p.name(), p.source().as_str(), p.auth()))
        .collect();
    let mut detail = format!("{} database(s): {}", names.len(), names.join(", "));
    if !ignored.is_empty() {
        detail.push_str(&format!(
            "; {} in databases.json ignored (cloud-only)",
            ignored.join(", ")
        ));
    }
    if !pools.contains_key(db::DB_FO) {
        return Outcome::warn(format!("{detail}; {} missing: {PROBE_OP} fails", db::DB_FO))
            .remedy(DB_REMEDY);
    }
    Outcome::pass(detail)
}

/// `SELECT 1` on one database; the detail names where its settings come from.
fn connects(name: &str, pool: Arc<Pool>) -> Check {
    let title = format!("{name} connects");
    Check::bounded(
        NAME,
        &format!("db.{name}"),
        &title,
        PROBE_TIMEOUT * 2,
        move || {
            let how = match pool.rung() {
                Some(r) => format!("{}, auth {}, TLS {r}", pool.source().as_str(), pool.auth()),
                None => format!("{}, auth {}", pool.source().as_str(), pool.auth()),
            };
            let ctx = Ctx::background().with_timeout(PROBE_TIMEOUT);
            let result = pool.get(&ctx).and_then(|mut c| c.batch(&ctx, "SELECT 1"));
            match result {
                Ok(_) => Outcome::pass(format!("SELECT 1 answered ({how})")),
                Err(e) => {
                    let o = Outcome::fail(format!("{e} ({how})"));
                    match pool.source() {
                        Source::SqlJson => o.remedy(
                            "check SQL Server and the login: dl-agent sql-test, then dl-agent sql-setup",
                        ),
                        Source::DatabasesJson => {
                            o.remedy("check SQL Server and the login in databases.json")
                        }
                    }
                }
            }
        },
    )
}

/// The journal as it is on disk (read only: the service may be writing it).
fn journal_row(state_dir: &Path) -> Outcome {
    let h = match journal::inspect(state_dir) {
        Ok(h) => h,
        Err(e) => {
            return Outcome::fail(e.to_string())
                .remedy("give the service account full control of the state directory");
        }
    };
    let detail = format!(
        "{} entries, {} bytes in {} (keeps {} days; cap {} entries, {} bytes)",
        h.entries,
        h.bytes,
        h.dir.display(),
        h.limits.retention.as_secs() / 86_400,
        h.limits.max_entries,
        h.limits.max_bytes
    );
    let pct = |n: u64, cap: u64| n.saturating_mul(100) / cap.max(1);
    let fill = pct(h.entries, h.limits.max_entries).max(pct(h.bytes, h.limits.max_bytes));
    if h.full() {
        return Outcome::fail(format!("full: {detail}; keyed writes are refused"))
            .remedy("entries expire after the retention; report it to SMILE PMS support");
    }
    if fill >= JOURNAL_WARN_PCT {
        return Outcome::warn(format!("{fill}% full: {detail}"));
    }
    Outcome::pass(detail)
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

/// `fo.hotelDate.get` on SMILE_FO through the real executor.
fn op(pools: &Pools) -> Check {
    let title = "Read op runs locally";
    if !pools.contains_key(db::DB_FO) {
        return Check::skip(NAME, "op", title, "SMILE_FO is not configured");
    }
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
            target_db: db::DB_FO.into(),
            args: dl_json::json!({}),
            request_key: None,
            expected_stamp: None,
            deadline_ms: i64::try_from(PROBE_TIMEOUT.as_millis()).unwrap_or(15_000),
            replay: None,
        };
        let ctx = Ctx::background().with_timeout(PROBE_TIMEOUT);
        let r = exec.execute(&ctx, &op);
        let error = r
            .error
            .as_ref()
            .map(|e| format!("{} {}", e.code, e.message))
            .unwrap_or_default();
        if r.status == wire::STATUS_APPLIED {
            let date = r
                .sets
                .first()
                .and_then(|s| s.first())
                .and_then(|row| row.get("HotelDate"))
                .and_then(|v| v.as_str())
                .map_or("", |d| d.get(..10).unwrap_or(d));
            return Outcome::pass(format!("{PROBE_OP} applied: hotel date {date}"));
        }
        Outcome::fail(format!("{PROBE_OP} {}: {error}", r.status)).remedy(
            "check that SMILE_FO has the vendor procedure GetHotelDate1 and one HotelParameter row",
        )
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
