//! The module's self-test (`dl-agent selftest`, the `selftest` and `einvoice.selftest`
//! commands): `config.enc` opens, the platform answers the agent's key (and its collect
//! profile), the FO/POS databases have the collector's procedures (and when they last
//! changed), one read of the pending bills (count only), how SQL Server is reached
//! (authentication, transport rung), the last collect cycle and the outbox depth. Read
//! only: nothing is collected, marked, pushed or created.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use dl_json::Value;

use super::collector::LAST_CYCLE_FILE;
use super::settings::Settings;
use super::{NAME, OUTBOX_FILE};
use crate::agent_config::AgentConfig;
use crate::outbox::Outbox;
use crate::platform::{ApiError, Platform};
use crate::runner::sources_for;
use crate::sql::Sql;

const REENROLL: &str =
    "enroll again: dl-agent einvoice-enroll --token <token> --platform-url <url> --force";

/// The self-test to register with `App::selftest`.
pub fn einvoice_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

pub(crate) fn run(ctx: &ModuleContext) -> Vec<Check> {
    let settings = Settings::from_config(ctx.config());
    let (check, cfg) = config(&settings.config_dir);
    let Some(cfg) = cfg else {
        return vec![check];
    };
    let cfg = Arc::new(cfg);
    let mut checks = vec![check, platform(&cfg)];
    let started = Instant::now();
    let sql = match settings.open_sql(ctx, &cfg) {
        Ok(sql) => Arc::new(sql),
        Err(e) => {
            let outcome = Outcome::fail(e).remedy(
                "declare the database with dl-agent sql-setup, or remove sql_source from the einvoice section",
            );
            let title = "SQL source (sql_source)";
            checks.push(Check::new(
                NAME,
                "sql.source",
                title,
                outcome,
                started.elapsed(),
            ));
            return checks;
        }
    };
    checks.push(schema_fo(&sql, &cfg));
    checks.push(schema_pos(&sql, &cfg));
    for source in sources_for(&cfg.collect_sources) {
        checks.push(pending(&sql, &cfg, source));
    }
    checks.push(procs(&sql, &cfg));
    checks.push(transport(&sql));
    let data = ctx.data_dir();
    checks.push(last_cycle(&data.join(LAST_CYCLE_FILE), dl_agent::now()));
    checks.push(outbox(&data.join(OUTBOX_FILE)));
    checks
}

/// `config.enc` in the configured directory decrypts; `None` stops the module's checks.
fn config(dir: &Path) -> (Check, Option<AgentConfig>) {
    let title = "Sealed config (config.enc) opens";
    let started = Instant::now();
    let (outcome, cfg) = match AgentConfig::load(dir) {
        Ok(None) => {
            let reason = format!("not enrolled: no config.enc in {}", dir.display());
            return (Check::skip(NAME, "config", title, &reason), None);
        }
        Ok(Some(cfg)) => {
            let detail = format!(
                "config.enc in {} opens; device {}, sources {}",
                dir.display(),
                cfg.device_id,
                if cfg.collect_sources.is_empty() {
                    "FO,POS"
                } else {
                    &cfg.collect_sources
                }
            );
            (Outcome::pass(detail), Some(cfg))
        }
        Err(e) => (
            Outcome::fail(format!(
                "config.enc in {} does not open: {e}",
                dir.display()
            ))
            .remedy(format!("{REENROLL} (DPAPI binds it to this machine)")),
            None,
        ),
    };
    (
        Check::new(NAME, "config", title, outcome, started.elapsed()),
        cfg,
    )
}

/// `GET /config` with the agent's key: the platform is reachable and accepts the key.
fn platform(cfg: &Arc<AgentConfig>) -> Check {
    let cfg = Arc::clone(cfg);
    Check::bounded(
        NAME,
        "platform",
        "Platform accepts the agent key",
        PROBE_TIMEOUT * 2,
        move || {
            let p = Platform::new(&cfg.platform_base_url, &cfg.api_key, cfg.device_id);
            match p.config(Instant::now() + PROBE_TIMEOUT) {
                Ok(remote) => Outcome::pass(format!(
                    "{} answered GET /config (collect_sources {}, collect_from {}, \
                     recalc_tax_into_sc {}, collect_profile pos_item_name {})",
                    host(&cfg.platform_base_url),
                    if remote.collect_sources.is_empty() {
                        "default"
                    } else {
                        &remote.collect_sources
                    },
                    remote
                        .collect_from
                        .map_or_else(|| "default".to_owned(), |t| t.to_rfc3339()),
                    remote.recalc_tax_into_sc,
                    format!("{:?}", remote.collect_profile.pos_item_name).to_lowercase(),
                )),
                Err(ApiError::DeviceReplaced) => Outcome::fail(
                    ApiError::DeviceReplaced.to_string(),
                )
                .remedy("this key now belongs to another machine; stop one of the two agents"),
                Err(e) => Outcome::fail(format!("{}: {e}", host(&cfg.platform_base_url))).remedy(
                    format!("check the network to the platform; a refused key needs {REENROLL}"),
                ),
            }
        },
    )
}

/// Scheme and host of a URL, without path or query.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    match url.split_once("://") {
        Some((scheme, _)) => format!("{scheme}://{host}"),
        None => host.to_owned(),
    }
}

fn missing_detail(db: &str, missing: &[String]) -> Outcome {
    if missing.is_empty() {
        return Outcome::pass(format!("{db}: every collector procedure is installed"));
    }
    Outcome::fail(format!("{db} lacks {}", missing.join(", ")))
        .remedy("install the collector procedures again: dl-agent einvoice-enroll ... --force")
}

/// SMILE_FO answers, has the FO procedures, and `sp_ProbeFOSchema` reads its schema.
fn schema_fo(sql: &Arc<Sql>, cfg: &Arc<AgentConfig>) -> Check {
    let (sql, cfg) = (Arc::clone(sql), Arc::clone(cfg));
    Check::bounded(
        NAME,
        "sql.fo",
        "SMILE_FO schema",
        PROBE_TIMEOUT * 2,
        move || {
            let names = [
                cfg.select_fo_sp.as_str(),
                &cfg.fetch_fo_sp,
                &cfg.mark_collected_sp,
                &cfg.writeback_sp,
                &cfg.probe_fo_schema_sp,
            ];
            let missing = match sql.missing_procs("FO", &names, PROBE_TIMEOUT) {
                Ok(m) => m,
                Err(e) => {
                    return Outcome::fail(e)
                        .remedy("check SQL Server and the FO login in config.enc");
                }
            };
            let outcome = missing_detail("SMILE_FO", &missing);
            if !missing.is_empty() {
                return outcome;
            }
            match sql.probe_fo_schema(PROBE_TIMEOUT) {
                Ok(p) if p.has_transaction_group && p.has_tax_def => Outcome::pass(format!(
                    "{}; TransactionGroup, TaxDef present; group 1 has {} charge codes",
                    outcome.detail, p.group1_count
                )),
                Ok(p) => Outcome::warn(format!(
                    "{}; TransactionGroup {}, TaxDef {}: tax lines fall back to the standard rate",
                    outcome.detail, p.has_transaction_group, p.has_tax_def
                )),
                Err(e) => Outcome::fail(e),
            }
        },
    )
}

/// SMILE_POS (or SMILE_FO on a single-database site) answers and has the POS procedures.
fn schema_pos(sql: &Arc<Sql>, cfg: &Arc<AgentConfig>) -> Check {
    let title = "SMILE_POS schema";
    if !sources_for(&cfg.collect_sources).contains(&"POS") {
        return Check::skip(NAME, "sql.pos", title, "POS bills are not collected here");
    }
    let (sql, cfg) = (Arc::clone(sql), Arc::clone(cfg));
    Check::bounded(NAME, "sql.pos", title, PROBE_TIMEOUT * 2, move || {
        let db = if cfg.pos_sql_dsn.is_empty() {
            "SMILE_FO (POS bills)"
        } else {
            "SMILE_POS"
        };
        let names = [cfg.select_pos_sp.as_str(), &cfg.fetch_pos_sp];
        match sql.missing_procs("POS", &names, PROBE_TIMEOUT) {
            Ok(missing) => missing_detail(db, &missing),
            Err(e) => Outcome::fail(e).remedy("check SQL Server and the POS login in config.enc"),
        }
    })
}

/// One collector read of the pending bills of `source`, counted, never fetched or marked.
fn pending(sql: &Arc<Sql>, cfg: &Arc<AgentConfig>, source: &'static str) -> Check {
    let (sql, cfg) = (Arc::clone(sql), Arc::clone(cfg));
    let id = format!("pending.{}", source.to_lowercase());
    let title = format!("Collector reads pending {source} bills");
    Check::bounded(NAME, &id, &title, PROBE_TIMEOUT * 2, move || {
        let limit = cfg.batch_size.max(1);
        let timeout = PROBE_TIMEOUT.min(cfg.cycle_timeout.max(Duration::from_secs(1)));
        match sql.select_pending(source, limit, cfg.collect_from, timeout) {
            Ok(keys) if keys.len() as i64 >= limit => Outcome::pass(format!(
                "{limit}+ pending (one batch read, nothing collected)"
            )),
            Ok(keys) => Outcome::pass(format!("{} pending (nothing collected)", keys.len())),
            Err(e) => Outcome::fail(e),
        }
    })
}

/// When the collector procedures last changed, per database: the installed version as far
/// as SQL Server records it (`sys.objects.modify_date`).
fn procs(sql: &Arc<Sql>, cfg: &Arc<AgentConfig>) -> Check {
    let (sql, cfg) = (Arc::clone(sql), Arc::clone(cfg));
    let title = "Collector procedures installed (version)";
    Check::bounded(NAME, "sql.procs", title, PROBE_TIMEOUT * 2, move || {
        let mut groups = vec![(
            "FO",
            vec![
                cfg.select_fo_sp.as_str(),
                &cfg.fetch_fo_sp,
                &cfg.mark_collected_sp,
                &cfg.writeback_sp,
                &cfg.probe_fo_schema_sp,
                crate::sql::drift::SOURCE_HASH_FUNC,
            ],
        )];
        if sources_for(&cfg.collect_sources).contains(&"POS") {
            groups.push((
                "POS",
                vec![
                    cfg.select_pos_sp.as_str(),
                    &cfg.fetch_pos_sp,
                    &cfg.mark_collected_sp,
                    crate::sql::drift::SOURCE_HASH_FUNC,
                ],
            ));
        }
        let mut parts = Vec::new();
        let mut missing = Vec::new();
        for (source, names) in groups {
            let db = sql.database(source).to_owned();
            match sql.proc_dates(source, &names, PROBE_TIMEOUT) {
                Ok(dates) => parts.push(proc_summary(&db, &dates, &mut missing)),
                Err(e) => return Outcome::fail(e),
            }
        }
        let detail = parts.join("; ");
        if missing.is_empty() {
            Outcome::pass(detail)
        } else {
            Outcome::warn(format!("{detail}; missing {}", missing.join(", "))).remedy(
                "install the collector procedures again: dl-agent einvoice-enroll ... --force",
            )
        }
    })
}

fn proc_summary(
    db: &str,
    dates: &[(String, Option<dl_time::DateTime>)],
    missing: &mut Vec<String>,
) -> String {
    let have = dates.iter().filter(|(_, d)| d.is_some()).count();
    for (name, d) in dates {
        if d.is_none() {
            missing.push(format!("{db}.{name}"));
        }
    }
    match dates.iter().filter_map(|(_, d)| *d).max() {
        Some(last) => format!(
            "{db}: {have}/{} installed, last changed {last}",
            dates.len()
        ),
        None => format!("{db}: 0/{} installed", dates.len()),
    }
}

/// Authentication (Windows = the service identity, or SQL) and the transport rung in use,
/// per source, after the checks above connected.
fn transport(sql: &Sql) -> Check {
    let started = Instant::now();
    let d = sql.describe();
    let mut parts = Vec::new();
    let mut clear = false;
    for source in ["FO", "POS"] {
        let Some(v) = d.get(source) else { continue };
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("-").to_owned();
        let rung = v
            .get("rung")
            .and_then(Value::as_str)
            .unwrap_or("not connected");
        clear |= rung == "unencrypted";
        let sealed = v
            .get("sealed_transport")
            .and_then(Value::as_str)
            .map(|t| format!(", sealed {t}"))
            .unwrap_or_default();
        parts.push(format!(
            "{source}: {} via {}, {} auth{sealed}, rung {rung}",
            s("database"),
            s("source"),
            s("auth"),
        ));
    }
    let detail = parts.join("; ");
    let outcome = if clear {
        Outcome::warn(detail).remedy(
            "the SQL login travels unencrypted: install the TLS 1.2 update for this SQL Server",
        )
    } else {
        Outcome::pass(detail)
    };
    let title = "SQL authentication and transport";
    Check::new(NAME, "sql.transport", title, outcome, started.elapsed())
}

/// The last collect cycle the running module recorded.
fn last_cycle(path: &Path, now: u64) -> Check {
    let started = Instant::now();
    let title = "Last collect cycle";
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let reason =
                "no cycle recorded yet (module not started, or its first cycle is running)";
            return Check::skip(NAME, "collect.last", title, reason);
        }
        Err(e) => {
            let outcome = Outcome::fail(format!("{}: {e}", path.display()));
            return Check::new(NAME, "collect.last", title, outcome, started.elapsed());
        }
    };
    let outcome = match dl_json::parse(&text) {
        Err(e) => Outcome::fail(format!("{}: {e}", path.display())),
        Ok(v) => {
            let n = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0);
            let at = v.get("at").and_then(Value::as_u64).unwrap_or(0);
            let ago = now.saturating_sub(at);
            let detail = format!(
                "{ago} s ago: read {}, pushed {}, errored {}, write-backs applied {}",
                n("read"),
                n("pushed"),
                n("errored"),
                n("applied")
            );
            match v.get("problem").and_then(Value::as_str) {
                Some(p) => Outcome::warn(format!("{detail}; problem: {p}")),
                None if ago > STALE_CYCLE_SECS => Outcome::warn(format!(
                    "{detail}; no cycle for {} min: is the service running?",
                    ago / 60
                )),
                None => Outcome::pass(detail),
            }
        }
    };
    Check::new(NAME, "collect.last", title, outcome, started.elapsed())
}

/// A cycle older than this is reported (the default interval is 30 s).
const STALE_CYCLE_SECS: u64 = 30 * 60;
/// Queued items above this are reported.
const OUTBOX_WARN: usize = 200;

/// Items waiting in the outbox; read only (a missing file is not created).
fn outbox(path: &Path) -> Check {
    let started = Instant::now();
    let title = "Outbox depth";
    if !path.exists() {
        return Check::skip(NAME, "outbox", title, "no outbox yet (nothing queued)");
    }
    let outcome = match Outbox::open(path) {
        Ok(ob) if ob.len() > OUTBOX_WARN => {
            Outcome::warn(format!("{} items waiting to be pushed", ob.len()))
                .remedy("check the platform connection; the outbox drains on the next good push")
        }
        Ok(ob) => Outcome::pass(format!("{} items waiting to be pushed", ob.len())),
        Err(e) => Outcome::fail(e),
    };
    Check::new(NAME, "outbox", title, outcome, started.elapsed())
}

#[cfg(test)]
mod tests {
    use dl_agent::selftest::Status;
    use dl_json::json;

    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!("einv-st-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn a_site_without_config_enc_is_not_enrolled() {
        let dir = temp("none");
        let ctx = ModuleContext::standalone(
            NAME,
            json!({"config_dir": dir.display().to_string()}),
            dir.clone(),
        );
        let checks = run(&ctx);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Skip);
        assert!(
            checks[0]
                .detail
                .starts_with("not enrolled: no config.enc in")
        );
        assert!(
            checks[0].detail.contains("einv-st-"),
            "reads the configured dir"
        );
    }

    #[test]
    fn an_unreadable_config_enc_fails() {
        let dir = temp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.enc"), b"not sealed").unwrap();
        let (c, cfg) = config(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(cfg.is_none());
        assert_eq!(c.status, Status::Fail);
        assert!(c.remedy.unwrap().contains("einvoice-enroll"));
    }

    #[test]
    fn last_cycle_reads_the_module_record() {
        let dir = temp("cycle");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LAST_CYCLE_FILE);
        assert_eq!(last_cycle(&path, 100).status, Status::Skip);
        std::fs::write(
            &path,
            r#"{"at":100,"read":3,"pushed":3,"errored":0,"applied":1,"problem":null}"#,
        )
        .unwrap();
        let c = last_cycle(&path, 130);
        assert_eq!(c.status, Status::Pass, "{}", c.detail);
        assert!(
            c.detail.starts_with("30 s ago: read 3, pushed 3"),
            "{}",
            c.detail
        );
        assert_eq!(
            last_cycle(&path, 100 + STALE_CYCLE_SECS + 1).status,
            Status::Warn
        );
        std::fs::write(&path, r#"{"at":100,"problem":"push failed"}"#).unwrap();
        assert_eq!(last_cycle(&path, 101).status, Status::Warn);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn outbox_depth_never_creates_the_file() {
        let dir = temp("outbox");
        let path = dir.join(OUTBOX_FILE);
        assert_eq!(outbox(&path).status, Status::Skip);
        assert!(!path.exists(), "read only");
    }

    #[test]
    fn transport_names_auth_and_rung_before_any_connection() {
        let sql = Sql::new(
            "sqlserver://sa:x@127.0.0.1:1?database=SMILE_FO&tlsmin=1.0",
            "",
            AgentConfig::default().sp_names(),
        );
        let c = transport(&sql);
        assert_eq!(c.status, Status::Pass);
        assert!(
            c.detail.contains("FO: SMILE_FO via config.enc, sql auth"),
            "{}",
            c.detail
        );
        assert!(c.detail.contains("rung not connected"), "{}", c.detail);
    }

    #[test]
    fn proc_summary_names_the_missing_and_the_newest() {
        let t: dl_time::DateTime = "2026-11-19T10:20:30".parse().unwrap();
        let mut missing = Vec::new();
        let s = proc_summary(
            "SMILE_FO",
            &[("a".into(), Some(t)), ("b".into(), None)],
            &mut missing,
        );
        assert!(
            s.starts_with("SMILE_FO: 1/2 installed, last changed 2026-11-19"),
            "{s}"
        );
        assert_eq!(missing, vec!["SMILE_FO.b".to_owned()]);
    }

    #[test]
    fn host_drops_path_and_query() {
        assert_eq!(host("https://e.example/api/v1?k=1"), "https://e.example");
        assert_eq!(host("e.example/x"), "e.example");
    }
}
