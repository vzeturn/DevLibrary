//! Service-manager glue for the watchdog (PROTOCOL.md §6.3): registers the scheduled task
//! (Windows) or systemd timer that runs `<binary> watchdog` every five minutes, reads the
//! service state, and carries out what [`watchdog::decide`] chose.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::cli::{self, App};
use crate::hub::{HttpTransport, Transport};
use crate::identity::Identity;
use crate::report::{ProblemReport, Severity, Spool};
use crate::update::UpdateState;
use crate::watchdog::{self, Action, Service, State};
use crate::{Error, now};

/// Minutes between watchdog runs.
pub const EVERY_MINUTES: u32 = 5;

/// Name of the Windows scheduled task.
pub fn task_name(app: &App) -> String {
    format!("{} Watchdog", app.display)
}

pub(crate) fn run_tool(tool: &str, args: &[&str]) -> Result<String, Error> {
    let out = crate::error::io("run a service tool", Command::new(tool).args(args).output())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(Error::Config(format!(
            "{tool} {} failed ({}): {} {}",
            args.join(" "),
            out.status,
            text,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The systemd service + timer that run the watchdog.
pub fn systemd_watchdog_units(app: &App, exe: &str, data: &str) -> (String, String) {
    let service = format!(
        "[Unit]\nDescription={display} watchdog\n\n[Service]\nType=oneshot\n\
         ExecStart=\"{exe}\" watchdog --data \"{data}\"\n",
        display = app.display
    );
    let timer = format!(
        "[Unit]\nDescription={display} watchdog every {EVERY_MINUTES} min\n\n[Timer]\n\
         OnBootSec={EVERY_MINUTES}min\nOnUnitActiveSec={EVERY_MINUTES}min\n\n\
         [Install]\nWantedBy=timers.target\n",
        display = app.display
    );
    (service, timer)
}

fn systemd_dir() -> &'static Path {
    Path::new("/etc/systemd/system")
}

/// Registers the watchdog schedule (replacing an existing one).
pub fn register(app: &App, exe: &str, data: &str) -> Result<(), Error> {
    if cfg!(windows) {
        let tr = dl_winsvc::command_line(exe, &["watchdog", "--data", data]);
        let every = EVERY_MINUTES.to_string();
        let name = task_name(app);
        run_tool(
            "schtasks",
            &[
                "/Create", "/F", "/RU", "SYSTEM", "/SC", "MINUTE", "/MO", &every, "/TN", &name,
                "/TR", &tr,
            ],
        )?;
    } else if cfg!(target_os = "linux") {
        let (service, timer) = systemd_watchdog_units(app, exe, data);
        let base = format!("{}-watchdog", app.service);
        crate::store::write_bytes(
            &systemd_dir().join(format!("{base}.service")),
            service.as_bytes(),
        )?;
        crate::store::write_bytes(
            &systemd_dir().join(format!("{base}.timer")),
            timer.as_bytes(),
        )?;
        run_tool("systemctl", &["daemon-reload"])?;
        run_tool("systemctl", &["enable", "--now", &format!("{base}.timer")])?;
    }
    Ok(())
}

/// Removes the watchdog schedule; a missing one is not an error.
pub fn unregister(app: &App) {
    if cfg!(windows) {
        if let Err(e) = run_tool("schtasks", &["/Delete", "/F", "/TN", &task_name(app)]) {
            eprintln!("watchdog task: {e}");
        }
    } else if cfg!(target_os = "linux") {
        let base = format!("{}-watchdog", app.service);
        if let Err(e) = run_tool("systemctl", &["disable", "--now", &format!("{base}.timer")]) {
            eprintln!("watchdog timer: {e}");
        }
        for ext in ["service", "timer"] {
            match std::fs::remove_file(systemd_dir().join(format!("{base}.{ext}"))) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => eprintln!("watchdog {ext} unit: {e}"),
            }
        }
        if let Err(e) = run_tool("systemctl", &["daemon-reload"]) {
            eprintln!("watchdog timer: {e}");
        }
    }
}

/// The recovery settings [`set_recovery`] applies: restart after 5 s, 10 s, 60 s and reset
/// the failure counter daily.
pub(crate) fn expected_recovery() -> ([dl_winsvc::scm::RecoveryAction; 3], Duration) {
    use dl_winsvc::scm::RecoveryAction;
    (
        [
            RecoveryAction::Restart(Duration::from_secs(5)),
            RecoveryAction::Restart(Duration::from_secs(10)),
            RecoveryAction::Restart(Duration::from_secs(60)),
        ],
        Duration::from_secs(86_400),
    )
}

/// Windows service recovery: restart after 5 s, 10 s, 60 s; counter reset daily; exit codes
/// other than 0 (update installed = 75) count as failures.
pub fn set_recovery(service: &dl_winsvc::scm::Service) -> Result<(), Error> {
    let (restart, reset) = expected_recovery();
    service
        .set_recovery_actions(&restart, reset)
        .map_err(cli::svc)?;
    service
        .set_recovery_on_non_crash_failures(true)
        .map_err(cli::svc)
}

/// Whether the watchdog schedule is registered: the scheduled task (Windows) or an enabled
/// systemd timer (Linux). Read only.
pub(crate) fn watchdog_present(app: &App) -> Result<bool, Error> {
    let (tool, args): (&str, Vec<String>) = if cfg!(windows) {
        (
            "schtasks",
            vec!["/Query".into(), "/TN".into(), task_name(app)],
        )
    } else if cfg!(target_os = "linux") {
        let timer = format!("{}-watchdog.timer", app.service);
        if !systemd_dir().join(&timer).exists() {
            return Ok(false);
        }
        ("systemctl", vec!["is-enabled".into(), timer])
    } else {
        return Err(Error::Config(
            "the watchdog runs on Windows and Linux (systemd)".into(),
        ));
    };
    let out = Command::new(tool)
        .args(&args)
        .output()
        .map_err(|e| Error::Config(format!("{tool}: {e}")))?;
    Ok(out.status.success()
        && (cfg!(windows) || String::from_utf8_lossy(&out.stdout).trim() == "enabled"))
}

/// The Windows service's recovery settings: restart actions, counter reset and whether
/// non-crash failures count. `Ok(None)` off Windows.
pub(crate) fn recovery(
    app: &App,
) -> Result<Option<(Vec<dl_winsvc::scm::RecoveryAction>, Duration, bool)>, Error> {
    if !cfg!(windows) {
        return Ok(None);
    }
    let manager = dl_winsvc::scm::Manager::connect_read_only().map_err(cli::svc)?;
    let service = manager.open_read_only(&app.service).map_err(cli::svc)?;
    let (actions, reset) = service.recovery_actions().map_err(cli::svc)?;
    let non_crash = service.recovery_on_non_crash_failures().map_err(cli::svc)?;
    Ok(Some((actions, reset, non_crash)))
}

/// At service start: puts back a deleted watchdog task and the recovery settings, so an
/// agent installed before the watchdog existed (or tidied by an admin) gets them.
pub fn ensure(app: &App, dir: &Path) {
    if !cfg!(windows) {
        return;
    }
    let fix = || -> Result<bool, Error> {
        let manager = dl_winsvc::scm::Manager::connect().map_err(cli::svc)?;
        set_recovery(&manager.open(&app.service).map_err(cli::svc)?)?;
        if run_tool("schtasks", &["/Query", "/TN", &task_name(app)]).is_ok() {
            return Ok(false);
        }
        let exe = crate::error::io("locate executable", std::env::current_exe())?;
        register(app, &exe.display().to_string(), &dir.display().to_string())?;
        Ok(true)
    };
    match fix() {
        Ok(true) => dl_log::info!("watchdog task registered"),
        Ok(false) => {}
        Err(e) => dl_log::warn!("cannot set up the watchdog"; error = %e),
    }
}

/// The service as the service manager sees it (read only).
pub(crate) fn probe(app: &App) -> Result<Service, Error> {
    if cfg!(windows) {
        use dl_winsvc::scm::{Manager, StartType, State};
        let manager = Manager::connect_read_only().map_err(cli::svc)?;
        let service = match manager.open_read_only(&app.service) {
            Ok(s) => s,
            Err(e) if e.code() == Some(dl_winsvc::codes::SERVICE_DOES_NOT_EXIST) => {
                return Ok(Service::Missing);
            }
            Err(e) => return Err(cli::svc(e)),
        };
        if service.config().map_err(cli::svc)?.start_type == StartType::Disabled {
            return Ok(Service::Disabled);
        }
        Ok(match service.status().map_err(cli::svc)?.state {
            State::Stopped => Service::Stopped,
            State::Running => Service::Running,
            _ => Service::Pending,
        })
    } else if cfg!(target_os = "linux") {
        if !cli::unit_path(app).exists() {
            return Ok(Service::Missing);
        }
        // `is-enabled` and `is-active` exit non-zero for "disabled" / "inactive": read stdout.
        let ask = |verb: &str| {
            Command::new("systemctl")
                .args([verb, &app.service])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .map_err(|e| Error::Config(format!("systemctl {verb}: {e}")))
        };
        if matches!(ask("is-enabled")?.as_str(), "disabled" | "masked") {
            return Ok(Service::Disabled);
        }
        Ok(match ask("is-active")?.as_str() {
            "active" => Service::Running,
            "activating" | "deactivating" | "reloading" => Service::Pending,
            _ => Service::Stopped,
        })
    } else {
        Err(Error::Config(
            "the watchdog runs on Windows and Linux (systemd)".into(),
        ))
    }
}

fn kill(pid: u32) -> Result<(), Error> {
    let pid = pid.to_string();
    if cfg!(windows) {
        run_tool("taskkill", &["/F", "/PID", &pid]).map(drop)
    } else {
        run_tool("kill", &["-KILL", &pid]).map(drop)
    }
}

fn act(app: &App, action: &Action, pid: Option<u32>) -> Result<(), Error> {
    if cfg!(windows) {
        use dl_winsvc::scm::{Manager, State};
        let manager = Manager::connect().map_err(cli::svc)?;
        let service = manager.open(&app.service).map_err(cli::svc)?;
        match action {
            Action::Nothing(_) | Action::Start => {}
            Action::Kill(p) => kill(*p)?,
            Action::Restart(_) => {
                if let Err(e) = service.stop_and_wait(Duration::from_secs(60)) {
                    dl_log::warn!("stop did not finish, killing"; error = %e);
                    if let Some(p) = pid {
                        kill(p)?;
                    }
                }
            }
        }
        if !matches!(action, Action::Nothing(_)) {
            if let Err(e) = service.wait_for_state(State::Stopped, Duration::from_secs(30)) {
                dl_log::warn!("service did not reach stopped, starting anyway"; error = %e);
            }
            service.start(&[]).map_err(cli::svc)?;
        }
        Ok(())
    } else {
        let svc = app.service.as_str();
        match action {
            Action::Nothing(_) => Ok(()),
            Action::Start => run_tool("systemctl", &["start", svc]).map(drop),
            Action::Kill(_) => {
                run_tool("systemctl", &["kill", "-s", "KILL", svc])?;
                run_tool("systemctl", &["restart", svc]).map(drop)
            }
            Action::Restart(_) => run_tool("systemctl", &["restart", svc]).map(drop),
        }
    }
}

fn hub_reachable(dir: &Path) -> bool {
    let Ok(identity) = Identity::load(dir) else {
        return false;
    };
    let t = HttpTransport::new();
    identity.hub_urls.iter().any(|u| {
        let url = format!("{}/healthz", u.trim_end_matches('/'));
        matches!(t.send("GET", &url, &[], b""), Ok((200..=299, _)))
    })
}

/// The `watchdog` sub-command: one look, at most one action. Returns the exit code.
pub fn run(app: &App, dir: &Path) -> i32 {
    let at = now();
    let service = match probe(app) {
        Ok(s) => s,
        Err(e) => {
            dl_log::error!("cannot read the service state"; error = %e);
            return 1;
        }
    };
    let beat = watchdog::read_beat(dir);
    let pending = UpdateState::load(dir).is_ok_and(|u| u.pending.is_some());
    let mut state = State::load(dir);
    let action = watchdog::decide(&mut state, at, service, beat.as_ref(), pending, || {
        hub_reachable(dir)
    });
    if let Err(e) = state.save(dir) {
        dl_log::warn!("cannot save the watchdog state"; error = %e);
    }
    let (severity, code, what) = match &action {
        Action::Nothing(why) => {
            dl_log::info!("watchdog: nothing to do"; reason = why.as_str());
            return 0;
        }
        Action::Start => (
            Severity::Warning,
            "watchdog_started",
            "service was stopped; started it".to_string(),
        ),
        Action::Kill(pid) => (
            Severity::Error,
            "watchdog_killed",
            format!(
                "service stuck starting or stopping for 10 min; killed pid {pid} and started it"
            ),
        ),
        Action::Restart(why) => (
            Severity::Error,
            "watchdog_restarted",
            format!("{why}; restarted the service"),
        ),
    };
    let result = act(app, &action, beat.as_ref().map(|b| b.pid));
    let (severity, code, what) = match &result {
        Ok(()) => (severity, code, what),
        Err(e) => (
            Severity::Critical,
            "watchdog_action_failed",
            format!("{what} — failed: {e}"),
        ),
    };
    dl_log::warn!("watchdog acted"; code = code, detail = what.as_str());
    let mut report = ProblemReport::new(severity, "watchdog", code, &what, at);
    if let Some(b) = &beat {
        report = report
            .with("last_loop_at", b.at.to_string())
            .with("last_hub_at", b.hub_at.to_string())
            .with("version", b.version.clone());
    }
    if let Err(e) = Spool::open(dir.join(watchdog::REPORTS_FILE)).record(report) {
        dl_log::error!("cannot queue the watchdog report"; error = %e);
    }
    i32::from(result.is_err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_runs_the_watchdog_every_five_minutes() {
        let app = App::new("dl-agent", "DevLibrary Agent");
        let (service, timer) =
            systemd_watchdog_units(&app, "/opt/dl/dl-agent", "/var/lib/dl-agent");
        assert!(service.contains("Type=oneshot"));
        assert!(
            service
                .contains("ExecStart=\"/opt/dl/dl-agent\" watchdog --data \"/var/lib/dl-agent\"")
        );
        assert!(timer.contains("OnUnitActiveSec=5min") && timer.contains("timers.target"));
        assert_eq!(task_name(&app), "DevLibrary Agent Watchdog");
    }
}
