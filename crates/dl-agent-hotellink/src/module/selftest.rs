//! The module's self-test (`dl-agent selftest`, the `selftest` command): the config at the
//! module's `config_path` decrypts (DPAPI), the hub accepts the agent token, the job queue
//! reads, and SMILE answers the read queries the runner uses. Read only: the config is not
//! re-encrypted, no job is taken, no mapping table is created.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::settings::Settings;
use super::{NAME, QUEUE_FILE, hub_client, sql_repo};
use crate::config::{self, LocalConfig};
use crate::jobqueue::FileQueue;

const REENROLL: &str = "enroll again: dl-agent hl-enroll --code <code>";
/// The hub client tries the configured hub, then its fallbacks.
const HUB_BOUND: Duration = Duration::from_secs(45);

/// The self-test to register with `App::selftest`.
pub fn hotellink_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    let settings = Settings::from_config(ctx.config());
    let (check, cfg) = config_check(&settings.config_path);
    let Some(cfg) = cfg else {
        return vec![check];
    };
    let cfg = Arc::new(cfg);
    let mut checks = vec![check, hub(&cfg), queue(&ctx.data_dir().join(NAME))];
    let repo = match sql_repo(&cfg) {
        Ok(r) => r,
        Err(e) => {
            let o = Outcome::fail(format!("SQL settings do not parse: {e}"))
                .remedy("fix the sql section of the config");
            checks.push(Check::timed(NAME, "sql", "SMILE answers", || o));
            return checks;
        }
    };
    let (server, db) = (cfg.sql.server.clone(), cfg.sql.database.clone());
    let r = Arc::clone(&repo);
    checks.push(Check::bounded(
        NAME,
        "sql",
        "SMILE answers",
        PROBE_TIMEOUT * 2,
        move || {
            if let Err(e) = r.test_connection() {
                return Outcome::fail(format!("{server}/{db}: {}", e.message()))
                    .remedy("check SQL Server and the login in the config's sql section");
            }
            match r.get_hotel_date() {
                Ok(d) => Outcome::pass(format!("{server}/{db}: SELECT 1 answered; hotel date {d}")),
                Err(e) => Outcome::fail(format!("{server}/{db}: {}", e.message()))
                    .remedy("the database is not a SMILE_FO with HotelParameter"),
            }
        },
    ));
    checks.push(Check::bounded(
        NAME,
        "sql.mapping",
        "Room type mapping reads",
        PROBE_TIMEOUT * 2,
        move || match repo.get_room_type_mapped() {
            Ok(rows) => Outcome::pass(format!("RoomTypeMapped: {} row(s)", rows.len())),
            Err(e) => Outcome::warn(format!("RoomTypeMapped: {}", e.message()))
                .remedy("the module creates the table when it starts; start the service once"),
        },
    ));
    checks
}

/// The config decrypts without being rewritten; `None` stops the module's checks.
fn config_check(path: &Path) -> (Check, Option<LocalConfig>) {
    let title = "Config decrypts (DPAPI)";
    let started = std::time::Instant::now();
    let (outcome, cfg) = match config::read(path) {
        Ok(None) => {
            let reason = format!("not enrolled: no config at {}", path.display());
            return (Check::skip(NAME, "config", title, &reason), None);
        }
        Err(e) => (Outcome::fail(e).remedy(REENROLL), None),
        Ok(Some(cfg)) => match cfg.problem() {
            Some(p) => {
                let reason = format!("not enrolled: {p} ({})", path.display());
                return (Check::skip(NAME, "config", title, &reason), None);
            }
            None => {
                let detail = format!("{} opens; hotel {}", path.display(), cfg.hotel_id);
                (Outcome::pass(detail), Some(cfg))
            }
        },
    };
    (
        Check::new(NAME, "config", title, outcome, started.elapsed()),
        cfg,
    )
}

/// `GET /api/agent/config` with the agent token.
fn hub(cfg: &Arc<LocalConfig>) -> Check {
    let cfg = Arc::clone(cfg);
    Check::bounded(
        NAME,
        "hub",
        "Hub accepts the agent token",
        HUB_BOUND,
        move || match hub_client(&cfg).get_config() {
            Ok(c) => Outcome::pass(format!(
                "{} answered the config of hotel {} (sync {}, every {} min)",
                cfg.hub_url,
                cfg.hotel_id,
                if c.enabled { "on" } else { "off" },
                c.availability_interval_min
            )),
            Err(e) => Outcome::fail(format!("{}: {}", cfg.hub_url, e.message())).remedy(format!(
                "check the network to the hub; a refused token needs {REENROLL}"
            )),
        },
    )
}

/// The job queue in the module's state folder reads (nothing is taken).
fn queue(state: &Path) -> Check {
    let path = state.join(QUEUE_FILE);
    Check::timed(NAME, "queue", "Job queue reads", || {
        if !path.exists() {
            return Outcome::pass(format!("no queue yet in {}", state.display()));
        }
        match FileQueue::open(&path) {
            Ok(q) => Outcome::pass(format!(
                "{}: {} pending, {} applied but not acknowledged",
                path.display(),
                q.pending().len(),
                q.unacked().len()
            )),
            Err(e) => Outcome::fail(format!("{}: {}", path.display(), e.message()))
                .remedy("move the damaged queue file aside; the hub sends the jobs again"),
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
        std::env::temp_dir().join(format!("hl-st-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn a_missing_or_template_config_is_not_enrolled() {
        let dir = temp("none");
        let path = dir.join("bench").join("config.json");
        let ctx = ModuleContext::standalone(
            NAME,
            json!({"config_path": path.display().to_string()}),
            dir.clone(),
        );
        let checks = run(&ctx);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Skip);
        assert!(checks[0].detail.contains("bench"), "{}", checks[0].detail);

        std::fs::create_dir_all(&dir).unwrap();
        let template = dir.join("config.json");
        let body = r#"{"hubUrl":"https://hub.example.com","token":"plain-secret-token"}"#;
        std::fs::write(&template, body).unwrap();
        let (c, cfg) = config_check(&template);
        let after = std::fs::read_to_string(&template).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(cfg.is_none());
        assert_eq!(c.status, Status::Skip);
        assert_eq!(after, body, "the self-test never rewrites the config");
    }

    #[test]
    fn a_damaged_config_fails() {
        let dir = temp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, b"[1").unwrap();
        let (c, _) = config_check(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(c.status, Status::Fail);
        assert!(c.remedy.unwrap().contains("hl-enroll"));
    }

    #[test]
    fn the_queue_check_creates_nothing() {
        let dir = temp("queue");
        let c = queue(&dir);
        assert_eq!(c.status, Status::Pass);
        assert!(!dir.exists());
    }
}
