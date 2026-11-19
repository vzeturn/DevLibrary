//! The module's self-test (`dl-agent selftest`, the `selftest` command): `config.enc`
//! opens, the platform answers the agent's key, the FO/POS databases have the collector's
//! procedures, and one read of the pending bills (count only). Read only: nothing is
//! collected, marked or pushed.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::NAME;
use super::settings::Settings;
use crate::agent_config::AgentConfig;
use crate::platform::{ApiError, Platform};
use crate::runner::sources_for;
use crate::sql::Sql;

const REENROLL: &str =
    "enroll again: dl-agent einvoice-enroll --token <token> --platform-url <url> --force";

/// The self-test to register with `App::selftest`.
pub fn einvoice_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    let settings = Settings::from_config(ctx.config());
    let (check, cfg) = config(&settings.config_dir);
    let Some(cfg) = cfg else {
        return vec![check];
    };
    let cfg = Arc::new(cfg);
    let sql = Arc::new(Sql::new(&cfg.sql_dsn, &cfg.pos_sql_dsn, cfg.sp_names()));
    let mut checks = vec![check, platform(&cfg)];
    checks.push(schema_fo(&sql, &cfg));
    checks.push(schema_pos(&sql, &cfg));
    for source in sources_for(&cfg.collect_sources) {
        checks.push(pending(&sql, &cfg, source));
    }
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
                    "{} answered GET /config (collect_sources {})",
                    host(&cfg.platform_base_url),
                    if remote.collect_sources.is_empty() {
                        "default"
                    } else {
                        &remote.collect_sources
                    }
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
    fn host_drops_path_and_query() {
        assert_eq!(host("https://e.example/api/v1?k=1"), "https://e.example");
        assert_eq!(host("e.example/x"), "e.example");
    }
}
