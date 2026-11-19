//! The module's self-test (`dl-agent selftest`, the `selftest` command): `config.dpapi` in
//! the module's `program_data_dir` opens, the hub answers `sync-config` with the agent
//! token, and SMILE answers the read queries the scanners run. The write side is `skip`
//! while `read_only` is true; when writes are on, only their settings are checked (the
//! self-test never pulls a job nor writes SMILE).

use std::sync::Arc;

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest};
use dl_agent::supervisor::ModuleContext;

use super::{NAME, TOKEN_RETRY, agent_version};
use crate::config::{AgentConfig, ConfigStore, Dpapi, FILE_NAME, Sealer};
use crate::hub::gate::TokenGate;
use crate::hub::{HubApi, HubClient, HubError};
use crate::identity::IdentityProbe;
use crate::settings::Settings;
use crate::smile::{FolioSource, RoomSource, SmileHealth, TdsSmile};

const REENROLL: &str = "enroll again: dl-agent membership-enroll";

/// The self-test to register with `App::selftest`.
pub fn membership_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    run_with(ctx, Arc::new(Dpapi))
}

fn run_with(ctx: &ModuleContext, sealer: Arc<dyn Sealer>) -> Vec<Check> {
    let settings = Settings::from_config(ctx.config(), ctx.data_dir());
    let store = ConfigStore::new(settings.program_data_dir.clone(), sealer);
    let title = "Config opens (DPAPI)";
    let cfg = match store.load() {
        Ok(Some(c)) => c,
        Ok(None) => {
            let reason = format!("not enrolled: no {FILE_NAME} in {}", store.dir().display());
            return vec![Check::skip(NAME, "config", title, &reason)];
        }
        Err(e) => {
            let o = Outcome::fail(format!("{}: {e}", store.path().display())).remedy(REENROLL);
            return vec![Check::timed(NAME, "config", title, || o)];
        }
    };
    let detail = format!("{} opens; hotel {}", store.path().display(), cfg.hotel_id);
    let mut checks = vec![
        Check::timed(NAME, "config", title, || Outcome::pass(detail)),
        hub(&cfg),
    ];
    checks.extend(smile(&cfg.smile_connection_string));
    checks.push(write_side(&settings));
    checks
}

/// `GET api/agent/sync-config` with the agent token (read only on the hub).
fn hub(cfg: &AgentConfig) -> Check {
    let (url, token, hotel) = (
        cfg.hub_base_url.clone(),
        cfg.agent_token.clone(),
        cfg.hotel_id,
    );
    Check::bounded(
        NAME,
        "hub",
        "Hub accepts the agent token",
        PROBE_TIMEOUT * 3,
        move || {
            let gate = Arc::new(TokenGate::new(TOKEN_RETRY));
            let client = match HubClient::new(&url, &token, hotel, agent_version(), gate) {
                Ok(c) => c,
                Err(e) => return Outcome::fail(format!("hub address: {e}")).remedy(REENROLL),
            };
            match client.sync_config() {
                Ok(c) => Outcome::pass(format!(
                    "{url} answered sync-config (rooms {}, availability {})",
                    on_off(c.room_inventory_sync_enabled),
                    on_off(c.availability_sync_enabled)
                )),
                Err(e @ HubError::TokenRejected { .. }) => {
                    Outcome::fail(format!("{url}: {e}")).remedy(REENROLL)
                }
                Err(e) => Outcome::fail(format!("{url}: {e}"))
                    .remedy("check the network or proxy to the hub"),
            }
        },
    )
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// The read queries of the heartbeat, the identity gate, the room scan and the folio scan.
fn smile(connection: &str) -> Vec<Check> {
    let title = "SMILE answers";
    let db = match TdsSmile::new(connection) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            let o = Outcome::fail(format!("the SMILE connection string does not parse: {e}"))
                .remedy(REENROLL);
            return vec![Check::timed(NAME, "smile", title, || o)];
        }
    };
    let t = db.target();
    let at = format!("{}/{}", t.data_source, t.database);
    let bound = PROBE_TIMEOUT * 2;
    let (d, a) = (Arc::clone(&db), at.clone());
    let ping = Check::bounded(NAME, "smile", title, bound, move || match d.ping() {
        Ok(()) => Outcome::pass(format!("{a}: SELECT 1 answered")),
        Err(e) => Outcome::fail(format!("{a}: {e}")).remedy("check SQL Server and the SMILE login"),
    });
    if ping.status != dl_agent::selftest::Status::Pass {
        return vec![ping];
    }
    let (d, a) = (Arc::clone(&db), at.clone());
    let identity = Check::bounded(
        NAME,
        "smile.identity",
        "SMILE identity reads",
        bound,
        move || match d.read_identity() {
            Ok(s) => Outcome::pass(format!("{a}: SMILE clock {}", s.pms_now)),
            Err(e) => Outcome::fail(format!("{a}: {e}"))
                .remedy("the login needs read access to SMILE_FO's folio tables"),
        },
    );
    let (d, a) = (Arc::clone(&db), at.clone());
    let rooms = Check::bounded(
        NAME,
        "smile.room_types",
        "Room types read",
        bound,
        move || match d.room_types() {
            Ok(rows) => Outcome::pass(format!("{a}: {} room type(s)", rows.len())),
            Err(e) => Outcome::fail(format!("{a}: {e}")).remedy("the database is not a SMILE_FO"),
        },
    );
    let folio = Check::bounded(
        NAME,
        "smile.folio",
        "Folio columns read",
        bound,
        move || match db.occupancy_columns() {
            Ok(c) => Outcome::pass(format!(
                "{at}: NumEnf {}, ExtraBed {}, Folio_Special {}",
                yes_no(c.num_enf),
                yes_no(c.extra_bed),
                yes_no(c.has_special)
            )),
            Err(e) => Outcome::fail(format!("{at}: {e}")).remedy("the database is not a SMILE_FO"),
        },
    );
    vec![ping, identity, rooms, folio]
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// `skip` while `read_only`; with writes on, the settings the writers need.
fn write_side(settings: &Settings) -> Check {
    let title = "SMILE write side";
    if settings.read_only {
        return Check::skip(
            NAME,
            "write",
            title,
            "read_only is true: nothing is written",
        );
    }
    let w = &settings.write;
    Check::timed(NAME, "write", title, || {
        if let Some(problem) = &w.extra_bed_mode_problem {
            return Outcome::warn(problem.clone())
                .remedy("set extra_bed_write_mode to FOLD_LEGACY or SMILE_SPECIAL");
        }
        if !settings.pull_jobs {
            return Outcome::warn("read_only is false but pull_jobs is false: no job is pulled")
                .remedy("set pull_jobs to true, or read_only back to true");
        }
        Outcome::pass(format!(
            "writes on: up to {} job(s) per pull, extra-bed mode {}; the SMILE schema is probed on the first job",
            settings.max_jobs_per_pull, w.extra_bed_mode
        ))
    })
}

#[cfg(test)]
mod tests {
    use dl_agent::selftest::Status;
    use dl_json::json;

    use super::*;
    use crate::config::PlainSealer;

    fn temp(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!("mb-st-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn a_module_without_config_is_not_enrolled() {
        let dir = temp("none").join("bench").join("membership");
        let cfg = json!({"program_data_dir": dir.display().to_string()});
        let checks = run_with(
            &ModuleContext::standalone(NAME, cfg, dir.clone()),
            Arc::new(PlainSealer),
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Skip);
        assert!(checks[0].detail.contains("bench"), "{}", checks[0].detail);
        assert!(!dir.exists(), "the self-test created nothing");
    }

    #[test]
    fn a_config_that_does_not_open_fails() {
        let dir = temp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE_NAME), b"not sealed").unwrap();
        let cfg = json!({"program_data_dir": dir.display().to_string()});
        let checks = run_with(
            &ModuleContext::standalone(NAME, cfg, dir.clone()),
            Arc::new(PlainSealer),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Fail);
        assert!(
            checks[0]
                .remedy
                .as_deref()
                .unwrap_or("")
                .contains("membership-enroll")
        );
    }

    #[test]
    fn the_write_side_skips_while_read_only() {
        let data = std::path::Path::new("/data");
        let ro = Settings::from_config(&json!({}), data);
        assert_eq!(write_side(&ro).status, Status::Skip);
        let rw = Settings::from_config(&json!({"read_only": false, "pull_jobs": true}), data);
        assert_eq!(
            write_side(&rw).status,
            Status::Pass,
            "{}",
            write_side(&rw).detail
        );
        let idle = Settings::from_config(&json!({"read_only": false, "pull_jobs": false}), data);
        assert_eq!(write_side(&idle).status, Status::Warn);
    }

    #[test]
    fn a_bad_connection_string_fails_without_echoing_it() {
        let checks = smile("Server=;Password=hunter2-secret;Bogus");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, Status::Fail);
        assert!(
            !checks[0].detail.contains("hunter2-secret"),
            "{}",
            checks[0].detail
        );
    }
}
