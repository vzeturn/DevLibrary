//! Core checks that need no hub: identity, private files, the service and its watchdog, the
//! report spool on disk, the declared databases and leftover per-product agents.

use std::path::Path;
use std::time::{Duration, Instant};

use dl_json::Value;

use super::{CORE, Check, Outcome, PROBE_TIMEOUT, bounded};
use crate::cli::App;
use crate::identity::Identity;
use crate::sql::{SQL_FILE, Site, probe, survey};
use crate::watchdog::Service;
use crate::{Error, store};

/// Survey warnings that only describe the site (old engine, Express): shown, not warned.
const INFO_ONLY: [&str; 4] = [
    "engine_pre_2012",
    "compat_below_2012",
    "express_edition",
    "non_vietnamese_code_page",
];

/// The agent identity; also returns it for the hub checks.
pub(crate) fn identity(app: &App, dir: &Path) -> (Check, Option<Identity>) {
    let started = Instant::now();
    let (outcome, id) = match Identity::load(dir) {
        Ok(id) => (
            Outcome::pass(format!(
                "agent {} (tenant {}, site {}), {} hub address(es)",
                id.agent_id,
                id.tenant,
                id.site,
                id.hub_urls.len()
            )),
            Some(id),
        ),
        Err(Error::NotEnrolled) => (
            Outcome::fail(format!("not enrolled: no identity in {}", dir.display())).remedy(
                format!("run `{} enroll --hub <url> --code <code>`", app.service),
            ),
            None,
        ),
        Err(e) => (
            Outcome::fail(format!("identity unreadable: {e}")).remedy(format!(
                "enroll again with `{} enroll --force`",
                app.service
            )),
            None,
        ),
    };
    let check = Check::new(
        CORE,
        "identity",
        "Agent identity",
        outcome,
        started.elapsed(),
    );
    (check, id)
}

#[cfg(unix)]
fn private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(format!("mode {:o}, want 600", mode & 0o777))
    }
}

#[cfg(windows)]
fn private(path: &Path) -> Result<(), String> {
    let d = dl_winsvc::acl::read(path).map_err(|e| e.to_string())?;
    dl_winsvc::acl::check(&d)
}

#[cfg(not(any(unix, windows)))]
fn private(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Problem with the data directory itself, if any (`Err`: cannot tell).
#[cfg(unix)]
fn dir_problem(dir: &Path) -> Result<Option<String>, String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode();
    Ok((mode & 0o002 != 0)
        .then(|| format!("data directory is world-writable ({:o})", mode & 0o777)))
}

#[cfg(windows)]
fn dir_problem(dir: &Path) -> Result<Option<String>, String> {
    use dl_winsvc::acl::{SID_ADMINISTRATORS, SID_LOCAL_SYSTEM};
    let d = dl_winsvc::acl::read(dir).map_err(|e| e.to_string())?;
    Ok(
        (d.owner != SID_LOCAL_SYSTEM && d.owner != SID_ADMINISTRATORS).then(|| {
            format!(
                "data directory owner is {}, not SYSTEM or Administrators",
                d.owner
            )
        }),
    )
}

#[cfg(not(any(unix, windows)))]
fn dir_problem(_: &Path) -> Result<Option<String>, String> {
    Ok(None)
}

/// `identity.key` and `sql.json` readable by the service account and administrators only;
/// the data directory owned by them (Windows) or not world-writable (Linux).
pub(crate) fn permissions(dir: &Path) -> Check {
    Check::timed(CORE, "permissions", "Private files locked down", || {
        let files: Vec<_> = ["identity.key", SQL_FILE]
            .into_iter()
            .filter(|f| dir.join(f).exists())
            .collect();
        if files.is_empty() {
            return Outcome::skip("not enrolled: no private files yet");
        }
        let bad: Vec<String> = files
            .iter()
            .filter_map(|f| private(&dir.join(f)).err().map(|why| format!("{f}: {why}")))
            .collect();
        if !bad.is_empty() {
            let remedy = if cfg!(windows) {
                "limit the file to SYSTEM and Administrators: icacls <file> /inheritance:r /grant:r *S-1-5-18:F *S-1-5-32-544:F"
            } else {
                "chmod 600 <file> (owner: the service account)"
            };
            return Outcome::fail(bad.join("; ")).remedy(remedy);
        }
        match dir_problem(dir) {
            Ok(None) => Outcome::pass(format!("{} restricted", files.join(", "))),
            Ok(Some(p)) => Outcome::warn(p)
                .remedy("reinstall from an elevated prompt, or give the directory to SYSTEM/root"),
            Err(e) => Outcome::warn(format!(
                "{} restricted; data directory not readable: {e}",
                files.join(", ")
            )),
        }
    })
}

fn recovery_note(app: &App) -> Outcome {
    match crate::service_watch::recovery(app) {
        Ok(None) => Outcome::pass("running (systemd restarts it)"),
        Ok(Some((actions, reset, non_crash))) => {
            let (want, want_reset) = crate::service_watch::expected_recovery();
            if actions.as_slice() == want && reset == want_reset && non_crash {
                Outcome::pass("running; recovery restarts after 5 s, 10 s, 60 s")
            } else {
                Outcome::warn(format!(
                    "running, but recovery is {actions:?} (reset {} s, non-crash failures {non_crash})",
                    reset.as_secs()
                ))
                .remedy("restart the service: it puts the recovery actions back at start")
            }
        }
        Err(e) => Outcome::warn(format!("running; recovery settings unreadable: {e}")),
    }
}

/// The system service: installed, running and (Windows) set to restart on failure. Also
/// returns what the service manager said, for the hub check.
pub(crate) fn service(app: &App) -> (Check, Option<Service>) {
    let title = "Service installed and running";
    if !cfg!(windows) && !cfg!(target_os = "linux") {
        let skip = "services are checked on Windows and Linux (systemd) only";
        return (Check::skip(CORE, "service", title, skip), None);
    }
    let started = Instant::now();
    let start = if cfg!(windows) {
        format!("sc start {}", app.service)
    } else {
        format!("systemctl start {}", app.service)
    };
    let (outcome, state) = match crate::service_watch::probe(app) {
        Ok(Service::Running) => (recovery_note(app), Some(Service::Running)),
        Ok(Service::Missing) => (
            Outcome::fail(format!("service {} is not installed", app.service)).remedy(format!(
                "run `{} install` as Administrator/root",
                app.service
            )),
            Some(Service::Missing),
        ),
        Ok(Service::Disabled) => (
            Outcome::fail("installed but disabled").remedy(if cfg!(windows) {
                format!("sc config {} start= auto, then {start}", app.service)
            } else {
                format!("systemctl enable --now {}", app.service)
            }),
            Some(Service::Disabled),
        ),
        Ok(Service::Stopped) => (
            Outcome::fail("installed but stopped").remedy(start),
            Some(Service::Stopped),
        ),
        Ok(Service::Pending) => (
            Outcome::warn("starting or stopping").remedy("run the self-test again in a minute"),
            Some(Service::Pending),
        ),
        Err(e) => (
            Outcome::fail(format!("cannot ask the service manager: {e}")),
            None,
        ),
    };
    (
        Check::new(CORE, "service", title, outcome, started.elapsed()),
        state,
    )
}

/// The watchdog schedule (scheduled task or systemd timer).
pub(crate) fn watchdog(app: &App) -> Check {
    let title = "Watchdog scheduled";
    if !cfg!(windows) && !cfg!(target_os = "linux") {
        return Check::skip(
            CORE,
            "watchdog",
            title,
            "the watchdog runs on Windows and Linux only",
        );
    }
    Check::timed(
        CORE,
        "watchdog",
        title,
        || match crate::service_watch::watchdog_present(app) {
            Ok(true) => Outcome::pass(if cfg!(windows) {
                format!("scheduled task {}", crate::service_watch::task_name(app))
            } else {
                format!("timer {}-watchdog.timer enabled", app.service)
            }),
            Ok(false) => Outcome::warn("not registered")
                .remedy("restart the service (it registers the watchdog at start) or reinstall"),
            Err(e) => Outcome::warn(format!("cannot tell: {e}")),
        },
    )
}

/// The data directory takes writes and the report spool on disk parses.
pub(crate) fn spool_disk(dir: &Path) -> Outcome {
    let probe_file = dir.join("selftest.probe");
    let write = std::fs::write(&probe_file, b"selftest")
        .and_then(|()| std::fs::read(&probe_file))
        .and_then(|back| {
            std::fs::remove_file(&probe_file)?;
            Ok(back == b"selftest")
        });
    match write {
        Ok(true) => {}
        Ok(false) => return Outcome::fail("data directory reads back other bytes than written"),
        Err(e) => {
            return Outcome::fail(format!("data directory not writable: {e}"))
                .remedy("give the service account write access to the data directory");
        }
    }
    // Read without `Spool::open`: that renames a corrupt file, and the service owns it.
    match store::read_json(&dir.join(super::SPOOL_FILE)) {
        Ok(None) => Outcome::pass("writable; no reports waiting"),
        Ok(Some(v)) => {
            let n = v.as_array().map_or(0, Vec::len);
            Outcome::pass(format!("writable; {n} report(s) waiting"))
        }
        Err(e) => Outcome::warn(format!("report spool unreadable: {e}"))
            .remedy("the service sets a corrupt spool aside at start; restart it"),
    }
}

fn rung(stats: &Value) -> Option<String> {
    stats
        .get("transport")
        .and_then(|t| t.get("rung"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One database: connects, reads the survey warnings (C3) and, when the hub sent one with
/// the command, probes the schema requirement (C4).
fn database(pool: &crate::sql::SqlPool, req: Option<&probe::Requirement>) -> Outcome {
    let took = match pool.check() {
        Ok(d) => d,
        Err(e) => {
            return Outcome::fail(format!("cannot connect: {e}")).remedy(format!(
                "check the server and the login, then `sql-test {}`",
                pool.name()
            ));
        }
    };
    let transport = rung(&pool.stats());
    let surveyed = pool
        .get()
        .and_then(|mut conn| survey::survey(&mut *conn, transport.as_deref()));
    let mut detail = format!(
        "connected in {} ms over {}",
        took.as_millis(),
        transport.as_deref().unwrap_or("-")
    );
    let (info, actionable): (Vec<String>, Vec<String>) = match &surveyed {
        Ok(s) => s
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .partition(|w| INFO_ONLY.contains(&w.as_str())),
        Err(e) => (Vec::new(), vec![format!("survey failed: {e}")]),
    };
    if let Ok(s) = &surveyed
        && let Some(engine) = s
            .get("engine")
            .and_then(|e| e.get("name"))
            .and_then(Value::as_str)
    {
        detail.push_str(&format!("; {engine}"));
    }
    if !info.is_empty() {
        detail.push_str(&format!("; notes: {}", info.join(", ")));
    }
    let (schema, schema_ok) = match req {
        None => (
            "no schema requirement declared (C4 not probed)".to_string(),
            true,
        ),
        Some(r) => match pool.with(|conn| probe::probe(conn, r)) {
            Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => {
                ("schema requirement met".into(), true)
            }
            Ok(v) => {
                let missing = v.get("missing").map(dl_json::to_string).unwrap_or_default();
                return Outcome::fail(format!("{detail}; schema missing: {missing}"))
                    .remedy("install the module's SQL bundle (sql.install) or ask support");
            }
            Err(e) => (format!("schema probe failed: {e}"), false),
        },
    };
    detail.push_str(&format!("; {schema}"));
    if actionable.is_empty() && schema_ok {
        Outcome::pass(detail)
    } else {
        Outcome::warn(format!("{detail}; warnings: {}", actionable.join(", ")))
            .remedy("see the site survey in the hub console for each warning")
    }
}

/// Every database in `sql.json`. `req` is the optional `{requirement, database?}` the hub
/// sent with the command (the same shape as `sql.probe`).
pub(crate) fn databases(dir: &Path, site: &Site, args: &Value) -> Vec<Check> {
    let title = "Database reachable";
    if let Some(e) = &site.load_error {
        let o = Outcome::fail(format!("{SQL_FILE} rejected: {e}"))
            .remedy("fix the file or declare the databases again with `sql-add`");
        return vec![Check::new(CORE, "sql", title, o, Duration::ZERO)];
    }
    if !dir.join(SQL_FILE).exists() || site.pools.names().is_empty() {
        return vec![Check::skip(
            CORE,
            "sql",
            title,
            "no database declared in sql.json",
        )];
    }
    let req = match args.get("requirement").map(probe::Requirement::from_json) {
        None => None,
        Some(Ok(r)) => Some(r),
        Some(Err(e)) => {
            let o = Outcome::fail(format!("schema requirement in the command is invalid: {e}"));
            return vec![Check::new(CORE, "sql", title, o, Duration::ZERO)];
        }
    };
    let only = args
        .get("database")
        .and_then(Value::as_str)
        .map(str::to_string);
    site.pools
        .all()
        .into_iter()
        .map(|pool| {
            let id = format!("sql.{}", pool.name());
            let req = req
                .clone()
                .filter(|_| only.as_deref().is_none_or(|o| o == pool.name()));
            let started = Instant::now();
            let outcome = bounded(&id, PROBE_TIMEOUT * 2, move || {
                database(&pool, req.as_ref())
            })
            .unwrap_or_else(|e| Outcome::fail(format!("the database check {e}")));
            Check::new(CORE, &id, title, outcome, started.elapsed())
        })
        .collect()
}
