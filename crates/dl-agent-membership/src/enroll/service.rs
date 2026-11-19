//! The .NET agent's Windows service (`install.ps1`: `MM.Membership.Agent`, LocalSystem,
//! automatic start, restart on failure). Only one writer may run per hotel, so the switch
//! stops it and sets it to *disabled* (a reboot does not bring it back); the rollback puts
//! the recorded start type back and starts it. Off Windows there is no such service and
//! every call answers [`Stopped::NotWindows`] / [`Started::NotWindows`].

use std::path::PathBuf;

/// The service name the .NET installer registers.
pub const SERVICE_NAME: &str = "MM.Membership.Agent";

/// What stopping the .NET service did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// Not a Windows build: nothing to stop.
    NotWindows,
    /// No such service on this machine.
    NotInstalled,
    /// Stopped (or already stopped) and set to disabled.
    Disabled {
        /// Whether it was running before.
        was_running: bool,
        /// Its start type before (`automatic`, `manual`, `disabled`, `other:N`).
        start_before: String,
    },
}

/// What restarting the .NET service did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// Not a Windows build.
    NotWindows,
    /// No such service on this machine.
    NotInstalled,
    /// Start type restored; `running` tells whether it was started.
    Restored {
        /// Whether the service was started (not when the restored type is `disabled`).
        running: bool,
    },
}

/// Control over the .NET agent's service; tests use a fake.
pub trait LegacyService {
    /// The folder of the service executable, when the service exists.
    fn exe_dir(&self) -> Option<PathBuf>;
    /// Stops the service and sets it to disabled.
    fn stop_and_disable(&self) -> Result<Stopped, String>;
    /// Sets the start type back to `start` and starts the service unless it is `disabled`.
    fn restore_and_start(&self, start: &str) -> Result<Started, String>;
}

/// The real Service Control Manager.
#[derive(Debug, Default, Clone, Copy)]
pub struct Scm;

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::time::Duration;

    use dl_winsvc::Error;
    use dl_winsvc::codes::SERVICE_DOES_NOT_EXIST;
    use dl_winsvc::scm::{Manager, Service, StartType, State};

    use super::{LegacyService, SERVICE_NAME, Scm, Started, Stopped};

    const WAIT: Duration = Duration::from_secs(60);

    fn missing(e: &Error) -> bool {
        e.code() == Some(SERVICE_DOES_NOT_EXIST)
    }

    fn open() -> Result<Option<Service>, String> {
        let m = Manager::connect().map_err(|e| format!("service manager: {e}"))?;
        match m.open(SERVICE_NAME) {
            Ok(s) => Ok(Some(s)),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(format!("open {SERVICE_NAME}: {e}")),
        }
    }

    fn name(t: StartType) -> String {
        match t {
            StartType::Automatic => "automatic".into(),
            StartType::Manual => "manual".into(),
            StartType::Disabled => "disabled".into(),
            StartType::Other(v) => format!("other:{v}"),
        }
    }

    fn parse(s: &str) -> StartType {
        match s {
            "manual" => StartType::Manual,
            "disabled" => StartType::Disabled,
            other => other
                .strip_prefix("other:")
                .and_then(|v| v.parse().ok())
                .map_or(StartType::Automatic, StartType::Other),
        }
    }

    impl LegacyService for Scm {
        fn exe_dir(&self) -> Option<PathBuf> {
            let m = Manager::connect_read_only().ok()?;
            let s = m.open_read_only(SERVICE_NAME).ok()?;
            let cfg = s.config().ok()?;
            crate::enroll::legacy::exe_dir(&cfg.binary_path_name)
        }

        fn stop_and_disable(&self) -> Result<Stopped, String> {
            let Some(s) = open()? else {
                return Ok(Stopped::NotInstalled);
            };
            let start_before = name(
                s.config()
                    .map_err(|e| format!("read {SERVICE_NAME}: {e}"))?
                    .start_type,
            );
            let was_running = s
                .status()
                .map_err(|e| format!("query {SERVICE_NAME}: {e}"))?
                .state
                != State::Stopped;
            // Disabled first: the failure actions (restart after 60 s) must not bring it back
            // while it stops.
            s.set_start_type(StartType::Disabled)
                .map_err(|e| format!("disable {SERVICE_NAME}: {e}"))?;
            s.stop_and_wait(WAIT)
                .map_err(|e| format!("stop {SERVICE_NAME}: {e}"))?;
            Ok(Stopped::Disabled {
                was_running,
                start_before,
            })
        }

        fn restore_and_start(&self, start: &str) -> Result<Started, String> {
            let Some(s) = open()? else {
                return Ok(Started::NotInstalled);
            };
            let t = parse(start);
            s.set_start_type(t)
                .map_err(|e| format!("set {SERVICE_NAME} start type: {e}"))?;
            if t == StartType::Disabled {
                return Ok(Started::Restored { running: false });
            }
            if s.status()
                .map_err(|e| format!("query {SERVICE_NAME}: {e}"))?
                .state
                != State::Running
            {
                match s.start(&[]) {
                    Ok(()) => {}
                    Err(e) if e.code() == Some(dl_winsvc::codes::SERVICE_ALREADY_RUNNING) => {}
                    Err(e) => return Err(format!("start {SERVICE_NAME}: {e}")),
                }
                s.wait_for_state(State::Running, WAIT)
                    .map_err(|e| format!("start {SERVICE_NAME}: {e}"))?;
            }
            Ok(Started::Restored { running: true })
        }
    }
}

#[cfg(not(windows))]
impl LegacyService for Scm {
    fn exe_dir(&self) -> Option<PathBuf> {
        None
    }

    fn stop_and_disable(&self) -> Result<Stopped, String> {
        Ok(Stopped::NotWindows)
    }

    fn restore_and_start(&self, _start: &str) -> Result<Started, String> {
        Ok(Started::NotWindows)
    }
}
