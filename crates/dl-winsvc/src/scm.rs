//! Service Control Manager: install, configure, start, stop and remove services.
//!
//! Most calls need an elevated process. Without it they fail with [`Error::Win32`] and code
//! [`codes::ACCESS_DENIED`](crate::codes::ACCESS_DENIED).

use std::time::{Duration, Instant};

use crate::raw::*;
use crate::wide::to_wide;
use crate::{Error, sys};

/// When a service starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartType {
    /// At boot (`SERVICE_AUTO_START`).
    Automatic,
    /// When started by a user or program (`SERVICE_DEMAND_START`).
    Manual,
    /// Cannot be started (`SERVICE_DISABLED`).
    Disabled,
    /// Another raw value (boot or system drivers).
    Other(u32),
}

impl StartType {
    fn to_raw(self) -> u32 {
        match self {
            StartType::Automatic => 2,
            StartType::Manual => 3,
            StartType::Disabled => 4,
            StartType::Other(v) => v,
        }
    }

    fn from_raw(v: u32) -> StartType {
        match v {
            2 => StartType::Automatic,
            3 => StartType::Manual,
            4 => StartType::Disabled,
            v => StartType::Other(v),
        }
    }
}

/// The state of a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not running.
    Stopped,
    /// Starting.
    StartPending,
    /// Stopping.
    StopPending,
    /// Running.
    Running,
    /// Resuming from pause.
    ContinuePending,
    /// Pausing.
    PausePending,
    /// Paused.
    Paused,
    /// A value this crate does not know.
    Unknown(u32),
}

impl State {
    pub(crate) fn from_raw(v: u32) -> State {
        match v {
            SERVICE_STOPPED => State::Stopped,
            SERVICE_START_PENDING => State::StartPending,
            SERVICE_STOP_PENDING => State::StopPending,
            SERVICE_RUNNING => State::Running,
            SERVICE_CONTINUE_PENDING => State::ContinuePending,
            SERVICE_PAUSE_PENDING => State::PausePending,
            SERVICE_PAUSED => State::Paused,
            v => State::Unknown(v),
        }
    }
}

/// A service's current status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Current state.
    pub state: State,
    /// Whether the service accepts a stop request now.
    pub accepts_stop: bool,
    /// Win32 exit code (1066 means "see `service_specific_exit_code`").
    pub win32_exit_code: u32,
    /// Service-specific exit code.
    pub service_specific_exit_code: u32,
}

impl From<ServiceStatus> for Status {
    fn from(s: ServiceStatus) -> Status {
        Status {
            state: State::from_raw(s.current_state),
            accepts_stop: s.controls_accepted & SERVICE_ACCEPT_STOP != 0,
            win32_exit_code: s.win32_exit_code,
            service_specific_exit_code: s.service_specific_exit_code,
        }
    }
}

/// The configuration of an own-process service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConfig {
    /// Name shown in the Services console.
    pub display_name: String,
    /// Description shown in the Services console. Empty means none.
    pub description: String,
    /// When the service starts.
    pub start_type: StartType,
    /// The command line the SCM runs, already quoted; build it with
    /// [`command_line`](crate::command_line).
    pub binary_path_name: String,
    /// The account the service runs as. Read-only here: [`Manager::create`] and
    /// [`Service::update_config`] keep LocalSystem / the current account.
    pub service_start_name: String,
}

impl ServiceConfig {
    /// A configuration with an empty description that runs `binary_path_name`.
    pub fn new(display_name: &str, start_type: StartType, binary_path_name: &str) -> ServiceConfig {
        ServiceConfig {
            display_name: display_name.to_owned(),
            description: String::new(),
            start_type,
            binary_path_name: binary_path_name.to_owned(),
            service_start_name: String::new(),
        }
    }

    /// Sets the description.
    pub fn with_description(mut self, description: &str) -> ServiceConfig {
        self.description = description.to_owned();
        self
    }
}

/// One installed service, as listed by [`Manager::list_services`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEntry {
    /// Service (key) name.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// Current status.
    pub status: Status,
}

/// A connection to the local Service Control Manager.
pub struct Manager {
    h: sys::ScHandle,
}

impl Manager {
    /// Connects with full access (needs elevation), like Go's `mgr.Connect`.
    pub fn connect() -> Result<Manager, Error> {
        Ok(Manager {
            h: sys::open_sc_manager(SC_MANAGER_ALL_ACCESS)?,
        })
    }

    /// Connects with the rights to list and query services only (no elevation needed).
    pub fn connect_read_only() -> Result<Manager, Error> {
        Ok(Manager {
            h: sys::open_sc_manager(SC_MANAGER_CONNECT | SC_MANAGER_ENUMERATE_SERVICE)?,
        })
    }

    /// Every Win32 service installed on the machine.
    pub fn list_services(&self) -> Result<Vec<ServiceEntry>, Error> {
        Ok(sys::enum_services(&self.h)?
            .into_iter()
            .map(|s| ServiceEntry {
                name: s.name,
                display_name: s.display_name,
                status: s.status.into(),
            })
            .collect())
    }

    /// Opens a service with full access. A missing service fails with code
    /// [`codes::SERVICE_DOES_NOT_EXIST`](crate::codes::SERVICE_DOES_NOT_EXIST).
    pub fn open(&self, name: &str) -> Result<Service, Error> {
        self.open_with(name, SERVICE_ALL_ACCESS)
    }

    /// Opens a service with query rights only.
    pub fn open_read_only(&self, name: &str) -> Result<Service, Error> {
        self.open_with(name, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS)
    }

    fn open_with(&self, name: &str, access: u32) -> Result<Service, Error> {
        let h = sys::open_service(&self.h, &to_wide(name)?, access)?;
        Ok(Service {
            h,
            name: name.to_owned(),
        })
    }

    /// Creates an own-process service running as LocalSystem. An existing name fails with
    /// code [`codes::SERVICE_EXISTS`](crate::codes::SERVICE_EXISTS). If the description
    /// cannot be set the new service is deleted again.
    pub fn create(&self, name: &str, config: &ServiceConfig) -> Result<Service, Error> {
        let h = sys::create_service(
            &self.h,
            &to_wide(name)?,
            &to_wide(&config.display_name)?,
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            config.start_type.to_raw(),
            SERVICE_ERROR_NORMAL,
            &to_wide(&config.binary_path_name)?,
        )?;
        let service = Service {
            h,
            name: name.to_owned(),
        };
        if !config.description.is_empty()
            && let Err(e) = sys::set_description(&service.h, &to_wide(&config.description)?)
        {
            let _ = service.delete();
            return Err(e);
        }
        Ok(service)
    }
}

/// An open service.
pub struct Service {
    h: sys::ScHandle,
    name: String,
}

impl Service {
    /// The service name it was opened with.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current status.
    pub fn status(&self) -> Result<Status, Error> {
        Ok(sys::query_status(&self.h)?.into())
    }

    /// The current configuration, including the description.
    pub fn config(&self) -> Result<ServiceConfig, Error> {
        let raw = sys::query_config(&self.h)?;
        Ok(ServiceConfig {
            display_name: raw.display_name,
            description: sys::query_description(&self.h)?,
            start_type: StartType::from_raw(raw.start_type),
            binary_path_name: raw.binary_path_name,
            service_start_name: raw.start_name,
        })
    }

    /// Rewrites display name, start type, command line and description. The service type is
    /// always passed explicitly as own-process (`ChangeServiceConfigW` rejects 0).
    pub fn update_config(&self, config: &ServiceConfig) -> Result<(), Error> {
        sys::change_service_config(
            &self.h,
            SERVICE_WIN32_OWN_PROCESS,
            config.start_type.to_raw(),
            SERVICE_ERROR_NORMAL,
            Some(&to_wide(&config.binary_path_name)?),
            Some(&to_wide(&config.display_name)?),
        )?;
        sys::set_description(&self.h, &to_wide(&config.description)?)
    }

    /// Changes only the start type.
    pub fn set_start_type(&self, start_type: StartType) -> Result<(), Error> {
        sys::change_service_config(
            &self.h,
            sys::NO_CHANGE,
            start_type.to_raw(),
            sys::NO_CHANGE,
            None,
            None,
        )
    }

    /// Asks the SCM to start the service with `args` (passed to its service main, not to
    /// the process command line). Returns without waiting; see [`Service::wait_for_state`].
    pub fn start(&self, args: &[&str]) -> Result<(), Error> {
        let wide = args
            .iter()
            .map(|a| to_wide(a))
            .collect::<Result<Vec<_>, _>>()?;
        sys::start_service(&self.h, &wide)
    }

    /// Sends a stop request and returns the status the SCM reports right after it. A service
    /// that is not running fails with code
    /// [`codes::SERVICE_NOT_ACTIVE`](crate::codes::SERVICE_NOT_ACTIVE).
    pub fn stop(&self) -> Result<Status, Error> {
        Ok(sys::control_service(&self.h, SERVICE_CONTROL_STOP)?.into())
    }

    /// Stops the service if it is not already stopped and waits up to `timeout` for it.
    pub fn stop_and_wait(&self, timeout: Duration) -> Result<(), Error> {
        if self.status()?.state == State::Stopped {
            return Ok(());
        }
        match self.stop() {
            Ok(_) => {}
            Err(Error::Win32 {
                code: crate::codes::SERVICE_NOT_ACTIVE,
                ..
            }) => return Ok(()),
            Err(e) => return Err(e),
        }
        self.wait_for_state(State::Stopped, timeout)
    }

    /// Marks the service for deletion. It disappears once every handle to it is closed and
    /// it has stopped.
    pub fn delete(&self) -> Result<(), Error> {
        sys::delete_service(&self.h)
    }

    /// Polls every 300 ms until the service reaches `want`, or fails with
    /// [`Error::Timeout`] after `timeout`.
    pub fn wait_for_state(&self, want: State, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.status()?.state == want {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout {
                    op: state_name(want),
                });
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(300)));
        }
    }
}

/// What the SCM does after a service failure (`SC_ACTION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Nothing.
    None,
    /// Restart the service after the delay.
    Restart(Duration),
    /// Reboot the machine after the delay.
    Reboot(Duration),
    /// Run the configured failure command after the delay.
    RunCommand(Duration),
}

impl RecoveryAction {
    fn to_raw(self) -> (u32, u32) {
        let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
        match self {
            RecoveryAction::None => (0, 0),
            RecoveryAction::Restart(d) => (1, ms(d)),
            RecoveryAction::Reboot(d) => (2, ms(d)),
            RecoveryAction::RunCommand(d) => (3, ms(d)),
        }
    }

    fn from_raw(kind: u32, delay_ms: u32) -> RecoveryAction {
        let d = Duration::from_millis(u64::from(delay_ms));
        match kind {
            1 => RecoveryAction::Restart(d),
            2 => RecoveryAction::Reboot(d),
            3 => RecoveryAction::RunCommand(d),
            _ => RecoveryAction::None,
        }
    }
}

const SERVICE_CONFIG_DELAYED_AUTO_START_INFO: u32 = 3;
const SERVICE_CONFIG_FAILURE_ACTIONS_FLAG: u32 = 4;

impl Service {
    /// Sets the recovery actions (`sc failure`): the first failure runs `actions[0]`, the
    /// second `actions[1]`, and every later one the last action; the failure count goes
    /// back to zero after `reset_after` without a failure. An empty list removes every
    /// action. Like Go's `SetRecoveryActions`.
    pub fn set_recovery_actions(
        &self,
        actions: &[RecoveryAction],
        reset_after: Duration,
    ) -> Result<(), Error> {
        let raw: Vec<(u32, u32)> = actions.iter().map(|a| a.to_raw()).collect();
        let reset = u32::try_from(reset_after.as_secs()).unwrap_or(u32::MAX);
        sys::set_failure_actions(&self.h, reset, &raw)
    }

    /// The recovery actions and the reset period (`sc qfailure`).
    pub fn recovery_actions(&self) -> Result<(Vec<RecoveryAction>, Duration), Error> {
        let (reset, raw) = sys::query_failure_actions(&self.h)?;
        Ok((
            raw.into_iter()
                .map(|(k, d)| RecoveryAction::from_raw(k, d))
                .collect(),
            Duration::from_secs(u64::from(reset)),
        ))
    }

    /// Whether a stop with a non-zero exit code counts as a failure (`sc failureflag`);
    /// by default only crashes do.
    pub fn set_recovery_on_non_crash_failures(&self, on: bool) -> Result<(), Error> {
        sys::set_bool_config(&self.h, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG, on)
    }

    /// See [`Service::set_recovery_on_non_crash_failures`].
    pub fn recovery_on_non_crash_failures(&self) -> Result<bool, Error> {
        sys::query_bool_config(&self.h, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG)
    }

    /// Delays an automatic start until shortly after boot ("Automatic (Delayed Start)").
    pub fn set_delayed_auto_start(&self, on: bool) -> Result<(), Error> {
        sys::set_bool_config(&self.h, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, on)
    }

    /// See [`Service::set_delayed_auto_start`].
    pub fn delayed_auto_start(&self) -> Result<bool, Error> {
        sys::query_bool_config(&self.h, SERVICE_CONFIG_DELAYED_AUTO_START_INFO)
    }
}

fn state_name(s: State) -> &'static str {
    match s {
        State::Stopped => "service state Stopped",
        State::StartPending => "service state StartPending",
        State::StopPending => "service state StopPending",
        State::Running => "service state Running",
        State::ContinuePending => "service state ContinuePending",
        State::PausePending => "service state PausePending",
        State::Paused => "service state Paused",
        State::Unknown(_) => "service state",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values_round_trip() {
        for t in [
            StartType::Automatic,
            StartType::Manual,
            StartType::Disabled,
            StartType::Other(0),
        ] {
            assert_eq!(StartType::from_raw(t.to_raw()), t);
        }
        for v in 1..=7 {
            assert!(!matches!(State::from_raw(v), State::Unknown(_)));
        }
        assert_eq!(State::from_raw(99), State::Unknown(99));
        for a in [
            RecoveryAction::None,
            RecoveryAction::Restart(Duration::from_secs(5)),
            RecoveryAction::Reboot(Duration::from_millis(1)),
            RecoveryAction::RunCommand(Duration::ZERO),
        ] {
            let (k, d) = a.to_raw();
            assert_eq!(RecoveryAction::from_raw(k, d), a);
        }
        let st = Status::from(ServiceStatus {
            current_state: SERVICE_RUNNING,
            controls_accepted: SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            ..ServiceStatus::default()
        });
        assert_eq!(st.state, State::Running);
        assert!(st.accepts_stop);
    }

    #[cfg(windows)]
    #[test]
    fn lists_and_queries_a_builtin_service() {
        let m = Manager::connect_read_only().unwrap();
        let all = m.list_services().unwrap();
        assert!(all.len() > 20, "only {} services", all.len());
        let rpc = all
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case("RpcSs"))
            .expect("RpcSs");
        assert_eq!(rpc.status.state, State::Running);
        let s = m.open_read_only("RpcSs").unwrap();
        assert_eq!(s.status().unwrap().state, State::Running);
        let cfg = s.config().unwrap();
        assert!(
            cfg.binary_path_name
                .to_ascii_lowercase()
                .contains("svchost"),
            "{cfg:?}"
        );
        assert!(!cfg.display_name.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn missing_service_reports_its_code() {
        let m = Manager::connect_read_only().unwrap();
        let e = m
            .open_read_only("dl-winsvc-no-such-service-7f3a")
            .err()
            .unwrap();
        assert_eq!(e.code(), Some(crate::codes::SERVICE_DOES_NOT_EXIST));
        assert!(matches!(
            m.open_read_only("a\0b"),
            Err(Error::InvalidInput(_))
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_elsewhere() {
        assert!(matches!(Manager::connect(), Err(Error::Unsupported)));
    }
}
