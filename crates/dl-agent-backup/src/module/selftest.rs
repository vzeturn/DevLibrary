//! The module's self-test (`dl-agent selftest`, the `selftest` command): the enrollment in
//! the module's data folder reads, the hub gives the config and master key, every SQL
//! Server may `BACKUP` its scheduled databases with the options its edition supports, and
//! every destination takes a small probe object, reads it back and deletes it (the one
//! write of this self-test). No backup runs and no queue or state file is touched.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::selftest::{Check, Outcome, PROBE_TIMEOUT, SelfTest, bounded, run_id};
use dl_agent::supervisor::ModuleContext;
use dl_tds::CancelToken;

use super::{NAME, settings};
use crate::Error;
use crate::crypto::decrypt_credential;
use crate::enroll::Enrollment;
use crate::hub::{CloudDestinationConfig, ConfigResponse, HubClient, ServerConfig};
use crate::sql::{BackupOptions, Connector, Target, Tds};
use crate::sync::HUB_USERNAME;
use crate::upload::{Destination, Real, Uploader, Uploaders};

const REENROLL: &str = "enroll again: dl-agent backup-enroll --hub-url <origin> --force";
/// What every destination gets, reads back and loses again.
pub const PROBE_BYTES: &[u8] = b"dl-agent self-test probe: safe to delete\n";
/// One destination's write, read-back and delete.
const DEST_BOUND: Duration = Duration::from_secs(45);

/// The self-test to register with `App::selftest`.
pub fn backup_selftest() -> SelfTest {
    SelfTest { module: NAME, run }
}

fn run(ctx: &ModuleContext) -> Vec<Check> {
    run_with(ctx, Arc::new(Tds), Arc::new(Real))
}

/// The hub's config and the key its credentials are sealed under.
struct Remote {
    config: ConfigResponse,
    key: Option<String>,
}

impl Remote {
    /// `Core::open_credential` without the state: a flagged credential is an error.
    fn open(&self, stored: &str, flagged: bool, what: &str) -> Result<String, Error> {
        if flagged {
            return Err(Error::Credential(format!(
                "the hub could not open the stored {what}; enter it again in the hub"
            )));
        }
        if stored.is_empty() {
            return Ok(String::new());
        }
        let Some(key) = &self.key else {
            return Err(Error::Credential(format!(
                "{what}: the hub gave no master key"
            )));
        };
        decrypt_credential(stored, key).map_err(|e| Error::Credential(format!("{what}: {e}")))
    }
}

fn run_with(ctx: &ModuleContext, sql: Arc<dyn Connector>, up: Arc<dyn Uploaders>) -> Vec<Check> {
    let title = "Enrollment reads";
    let started = Instant::now();
    let enrollment = match Enrollment::load(ctx.data_dir()) {
        Ok(None) => {
            let reason = format!(
                "not enrolled: no enrollment in {}",
                ctx.data_dir().display()
            );
            return vec![Check::skip(NAME, "enrollment", title, &reason)];
        }
        Err(e) => {
            let o = Outcome::fail(e.to_string()).remedy(REENROLL);
            return vec![Check::new(NAME, "enrollment", title, o, started.elapsed())];
        }
        Ok(Some(e)) => e,
    };
    let detail = format!("agent {} of {}", enrollment.agent_id, enrollment.hub_url);
    let mut checks = vec![Check::new(
        NAME,
        "enrollment",
        title,
        Outcome::pass(detail),
        started.elapsed(),
    )];
    let (check, remote) = hub(&enrollment);
    checks.push(check);
    let Some(remote) = remote else {
        return checks;
    };
    let backup_dir = settings::from_config(ctx.config()).backup_dir;
    for server in &remote.config.servers {
        checks.extend(server_checks(&sql, &remote, server, &backup_dir));
    }
    checks.extend(destinations(&up, &remote));
    checks
}

fn fetch(url: &str, token: &str) -> Result<Remote, Error> {
    let hub = HubClient::new(url, token);
    let config = hub.fetch_config()?;
    let sealed = config
        .servers
        .iter()
        .any(|s| !s.encrypted_password.is_empty())
        || config
            .schedules
            .iter()
            .any(|s| !s.cloud.encrypted_tokens.is_empty());
    let key = if sealed {
        Some(hub.master_key()?)
    } else {
        None
    };
    Ok(Remote { config, key })
}

/// `GET /api/agent/config` (and the master key when something is sealed).
fn hub(e: &Enrollment) -> (Check, Option<Arc<Remote>>) {
    let title = "Hub accepts the agent token";
    let started = Instant::now();
    let (url, token) = (e.hub_url.clone(), e.token.clone());
    let res = bounded("backup-hub", PROBE_TIMEOUT * 2, move || fetch(&url, &token));
    let (outcome, remote) = match res {
        Err(stalled) => (Outcome::fail(format!("the probe {stalled}")), None),
        Ok(Err(err)) => {
            let remedy = if matches!(err, Error::Auth(_)) {
                REENROLL
            } else {
                "check the network or proxy to the hub"
            };
            (
                Outcome::fail(format!("{}: {err}", e.hub_url)).remedy(remedy),
                None,
            )
        }
        Ok(Ok(r)) => {
            let c = &r.config;
            let detail = format!(
                "{}: {} server(s), {} schedule(s), {} pending command(s)",
                e.hub_url,
                c.servers.len(),
                c.schedules.len(),
                c.commands.len() + c.restore_requests.len()
            );
            (Outcome::pass(detail), Some(Arc::new(r)))
        }
    };
    (
        Check::new(NAME, "hub", title, outcome, started.elapsed()),
        remote,
    )
}

/// One login per server: the scheduled databases may be backed up, and the `WITH` options
/// match the edition (`COMPRESSION` off on Express and Web).
fn server_checks(
    sql: &Arc<dyn Connector>,
    remote: &Arc<Remote>,
    server: &ServerConfig,
    backup_dir: &str,
) -> Vec<Check> {
    let host = if server.host.is_empty() {
        "localhost"
    } else {
        server.host.as_str()
    };
    let id = format!("sql.{host}");
    let started = Instant::now();
    let (sql, remote, s, dir) = (
        Arc::clone(sql),
        Arc::clone(remote),
        server.clone(),
        backup_dir.to_owned(),
    );
    let res = bounded(&format!("{NAME}-{id}"), PROBE_TIMEOUT * 2, move || {
        probe_server(sql.as_ref(), &remote, &s, &dir)
    });
    let (perm, compression) =
        res.unwrap_or_else(|e| (Outcome::fail(format!("the probe {e}")), None));
    let mut out = vec![Check::new(
        NAME,
        &id,
        &format!("{host} may BACKUP"),
        perm,
        started.elapsed(),
    )];
    if let Some(c) = compression {
        let title = format!("{host} backup options fit the edition");
        out.push(Check::new(
            NAME,
            &format!("compression.{host}"),
            &title,
            c,
            Duration::ZERO,
        ));
    }
    out
}

fn probe_server(
    sql: &dyn Connector,
    remote: &Remote,
    s: &ServerConfig,
    backup_dir: &str,
) -> (Outcome, Option<Outcome>) {
    let what = format!("password of server {}", s.host);
    let password = match remote.open(&s.encrypted_password, s.credential_error, &what) {
        Ok(p) => p,
        Err(e) => {
            return (
                Outcome::fail(e.to_string()).remedy("enter the server password again in the hub"),
                None,
            );
        }
    };
    let target = Target {
        host: s.host.clone(),
        port: s.port,
        auth_type: s.auth_type.clone(),
        username: HUB_USERNAME.into(),
        password,
    };
    let mut conn = match sql.connect(&target) {
        Ok(c) => c,
        Err(e) => {
            let o = Outcome::fail(e.to_string())
                .remedy("check SQL Server, its port and the login stored in the hub");
            return (o, None);
        }
    };
    let info = conn.info().clone();
    let compression = Outcome::pass(BackupOptions::for_server(&info).reason);
    let (mut ok, mut denied, mut missing) = (Vec::new(), Vec::new(), Vec::new());
    for db in &s.databases {
        match conn.can_backup(&db.database_name) {
            Ok(Some(true)) => ok.push(db.database_name.as_str()),
            Ok(Some(false)) => denied.push(db.database_name.as_str()),
            Ok(None) => missing.push(db.database_name.as_str()),
            Err(e) => return (Outcome::fail(e.to_string()), Some(compression)),
        }
    }
    let folder = if backup_dir.is_empty() {
        match conn.backup_dir() {
            Ok(Some(d)) => format!("instance default {d}"),
            Ok(None) => "instance default (not readable)".into(),
            Err(e) => format!("instance default unreadable: {e}"),
        }
    } else {
        backup_dir.to_owned()
    };
    let detail = format!(
        "login over {}; {} database(s) may be backed up; backup folder {folder}",
        info.transport,
        ok.len()
    );
    let perm = if !denied.is_empty() {
        Outcome::fail(format!(
            "{detail}; BACKUP DATABASE denied on {}",
            denied.join(", ")
        ))
        .remedy("grant the login db_backupoperator (or sysadmin) on those databases")
    } else if !missing.is_empty() {
        Outcome::warn(format!(
            "{detail}; not on this server: {}",
            missing.join(", ")
        ))
        .remedy("remove or fix the schedule's database in the hub")
    } else {
        Outcome::pass(detail)
    };
    (perm, Some(compression))
}

/// One probe per distinct destination of the schedules.
fn destinations(up: &Arc<dyn Uploaders>, remote: &Arc<Remote>) -> Vec<Check> {
    let mut seen: Vec<&CloudDestinationConfig> = Vec::new();
    for s in &remote.config.schedules {
        let c = &s.cloud;
        let stored = c.credential_error || !c.encrypted_tokens.is_empty();
        if stored && !seen.contains(&c) {
            seen.push(c);
        }
    }
    if seen.is_empty() {
        let reason = "no schedule has a cloud destination (backups stay on the server)";
        return vec![Check::skip(
            NAME,
            "destination",
            "Destinations take a probe",
            reason,
        )];
    }
    seen.into_iter()
        .enumerate()
        .map(|(i, c)| {
            let (up, remote, c) = (Arc::clone(up), Arc::clone(remote), c.clone());
            let id = format!("destination.{}", i + 1);
            let title = format!("{} destination: write, read back, delete", c.provider);
            Check::bounded(NAME, &id, &title, DEST_BOUND, move || {
                probe_destination(up.as_ref(), &remote, &c)
            })
        })
        .collect()
}

fn probe_destination(up: &dyn Uploaders, remote: &Remote, c: &CloudDestinationConfig) -> Outcome {
    let what = format!("credentials of destination {}", c.provider);
    let opened = remote
        .open(&c.encrypted_tokens, c.credential_error, &what)
        .and_then(|plain| Destination::from_plain(&c.provider, &plain, c.endpoint.as_deref()))
        .and_then(|d| up.open(&d));
    match opened {
        Ok(u) => round_trip(u.as_ref(), &std::env::temp_dir(), &run_id()),
        Err(e) => Outcome::fail(e.to_string())
            .remedy("enter the destination's credentials again in the hub"),
    }
}

/// Writes [`PROBE_BYTES`], reads them back and deletes them; the local copies live in a
/// fresh folder under `tmp` that is removed again.
pub(crate) fn round_trip(u: &dyn Uploader, tmp: &Path, tag: &str) -> Outcome {
    let dir = tmp.join(format!("dl-agent-selftest-{tag}"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Outcome::fail(format!("create {}: {e}", dir.display()));
    }
    let outcome = round_trip_in(u, &dir, tag);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => outcome,
        Err(e) => Outcome {
            detail: format!(
                "{}; the local probe folder {} stays: {e}",
                outcome.detail,
                dir.display()
            ),
            ..outcome
        },
    }
}

fn round_trip_in(u: &dyn Uploader, dir: &Path, tag: &str) -> Outcome {
    let name = format!("dl-agent-selftest-{tag}.probe");
    let file = dir.join(&name);
    if let Err(e) = std::fs::write(&file, PROBE_BYTES) {
        return Outcome::fail(format!("write {}: {e}", file.display()));
    }
    let started = Instant::now();
    let cancel = CancelToken::new();
    let cloud = match u.upload(&file, &cancel, &mut |_, _| {}) {
        Ok(p) => p,
        Err(e) => {
            return Outcome::fail(format!("write: {e}"))
                .remedy("check the destination's credentials and quota");
        }
    };
    let back = dir.join("read-back.probe");
    let read = u.download(&cloud, &back, &cancel).and_then(|_| {
        std::fs::read(&back).map_err(|e| Error::Upload(format!("read {}: {e}", back.display())))
    });
    let deleted = u.delete(&cloud);
    let ms = started.elapsed().as_millis();
    match (read, deleted) {
        (Ok(bytes), Ok(())) if bytes == PROBE_BYTES => Outcome::pass(format!(
            "wrote, read back and deleted a {}-byte probe in {ms} ms",
            PROBE_BYTES.len()
        )),
        (Ok(_), Ok(())) => Outcome::fail("the probe read back with different bytes"),
        (Err(e), del) => Outcome::fail(format!(
            "read back: {e}; delete {}",
            del.map_or_else(|d| format!("failed too: {d}"), |()| "done".into())
        )),
        (Ok(_), Err(e)) => Outcome::fail(format!(
            "delete: {e}; the probe {name} stays at the destination"
        ))
        .remedy("allow delete for the agent's credentials (retention needs it too)"),
    }
}

#[cfg(test)]
#[path = "selftest_tests.rs"]
mod tests;
