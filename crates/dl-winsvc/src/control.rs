//! A small, mockable surface for the three remote service operations an agent carries out:
//! stop, start and change the start type of a named service.
//!
//! [`ServiceControl`] is the trait callers program against. [`system`] returns the real
//! implementation: the Service Control Manager on Windows ([`ScmControl`]), and on other
//! platforms one that fails every call with [`Error::Unsupported`]. [`FakeControl`] is an
//! in-memory implementation for tests that records every call.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::Error;
use crate::scm::{StartType, State};

/// Stop, start and reconfigure services by name.
pub trait ServiceControl: Send + Sync {
    /// The current state of `name`.
    fn state(&self, name: &str) -> Result<State, Error>;
    /// Stops `name` (no-op when already stopped) and waits up to `timeout` for it.
    fn stop(&self, name: &str, timeout: Duration) -> Result<(), Error>;
    /// Starts `name` (no-op when already running) and waits up to `timeout` for it.
    fn start(&self, name: &str, timeout: Duration) -> Result<(), Error>;
    /// Changes only the start type of `name`.
    fn set_start_type(&self, name: &str, start_type: StartType) -> Result<(), Error>;
}

/// A shared implementation, so a test can keep a handle on a [`FakeControl`] it hands out.
impl<T: ServiceControl + ?Sized> ServiceControl for Arc<T> {
    fn state(&self, name: &str) -> Result<State, Error> {
        (**self).state(name)
    }
    fn stop(&self, name: &str, timeout: Duration) -> Result<(), Error> {
        (**self).stop(name, timeout)
    }
    fn start(&self, name: &str, timeout: Duration) -> Result<(), Error> {
        (**self).start(name, timeout)
    }
    fn set_start_type(&self, name: &str, start_type: StartType) -> Result<(), Error> {
        (**self).set_start_type(name, start_type)
    }
}

/// The platform implementation:[`ScmControl`] on Windows, [`Unsupported`] elsewhere.
pub fn system() -> Box<dyn ServiceControl> {
    #[cfg(windows)]
    {
        Box::new(ScmControl)
    }
    #[cfg(not(windows))]
    {
        Box::new(Unsupported)
    }
}

/// The Service Control Manager (needs an elevated process, as the agent service is).
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct ScmControl;

#[cfg(windows)]
impl ServiceControl for ScmControl {
    fn state(&self, name: &str) -> Result<State, Error> {
        let m = crate::scm::Manager::connect_read_only()?;
        Ok(m.open_read_only(name)?.status()?.state)
    }

    fn stop(&self, name: &str, timeout: Duration) -> Result<(), Error> {
        crate::scm::Manager::connect()?
            .open(name)?
            .stop_and_wait(timeout)
    }

    fn start(&self, name: &str, timeout: Duration) -> Result<(), Error> {
        let svc = crate::scm::Manager::connect()?.open(name)?;
        if svc.status()?.state == State::Running {
            return Ok(());
        }
        match svc.start(&[]) {
            Ok(()) => {}
            Err(Error::Win32 {
                code: crate::codes::SERVICE_ALREADY_RUNNING,
                ..
            }) => {}
            Err(e) => return Err(e),
        }
        svc.wait_for_state(State::Running, timeout)
    }

    fn set_start_type(&self, name: &str, start_type: StartType) -> Result<(), Error> {
        crate::scm::Manager::connect()?
            .open(name)?
            .set_start_type(start_type)
    }
}

/// Fails every call with [`Error::Unsupported`] (platforms without Windows services).
#[derive(Debug, Default, Clone, Copy)]
pub struct Unsupported;

impl ServiceControl for Unsupported {
    fn state(&self, _: &str) -> Result<State, Error> {
        Err(Error::Unsupported)
    }
    fn stop(&self, _: &str, _: Duration) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    fn start(&self, _: &str, _: Duration) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    fn set_start_type(&self, _: &str, _: StartType) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

/// One call recorded by [`FakeControl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// [`ServiceControl::stop`].
    Stop(String),
    /// [`ServiceControl::start`].
    Start(String),
    /// [`ServiceControl::set_start_type`].
    SetStartType(String, StartType),
}

#[derive(Debug, Default)]
struct FakeState {
    services: BTreeMap<String, (State, StartType)>,
    calls: Vec<Call>,
    fail_next: Option<Error>,
}

/// An in-memory service table for tests. Unknown names fail like Windows does, with
/// [`codes::SERVICE_DOES_NOT_EXIST`](crate::codes::SERVICE_DOES_NOT_EXIST).
#[derive(Debug, Default)]
pub struct FakeControl(Mutex<FakeState>);

impl FakeControl {
    /// An empty table.
    pub fn new() -> FakeControl {
        FakeControl::default()
    }

    /// Adds (or replaces) a service.
    pub fn with_service(self, name: &str, state: State, start_type: StartType) -> FakeControl {
        self.lock()
            .services
            .insert(name.to_owned(), (state, start_type));
        self
    }

    /// Makes the next mutating call fail with `e`.
    pub fn fail_next(&self, e: Error) {
        self.lock().fail_next = Some(e);
    }

    /// Every successful mutating call so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// The `(state, start type)` of `name`.
    pub fn service(&self, name: &str) -> Option<(State, StartType)> {
        self.lock().services.get(name).copied()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn mutate(
        &self,
        name: &str,
        call: Call,
        f: impl FnOnce(&mut (State, StartType)) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut s = self.lock();
        if let Some(e) = s.fail_next.take() {
            return Err(e);
        }
        let entry = s.services.get_mut(name).ok_or(Error::Win32 {
            op: "OpenServiceW",
            code: crate::codes::SERVICE_DOES_NOT_EXIST,
        })?;
        f(entry)?;
        s.calls.push(call);
        Ok(())
    }
}

impl ServiceControl for FakeControl {
    fn state(&self, name: &str) -> Result<State, Error> {
        self.service(name).map(|(st, _)| st).ok_or(Error::Win32 {
            op: "OpenServiceW",
            code: crate::codes::SERVICE_DOES_NOT_EXIST,
        })
    }

    fn stop(&self, name: &str, _: Duration) -> Result<(), Error> {
        self.mutate(name, Call::Stop(name.to_owned()), |e| {
            e.0 = State::Stopped;
            Ok(())
        })
    }

    fn start(&self, name: &str, _: Duration) -> Result<(), Error> {
        self.mutate(name, Call::Start(name.to_owned()), |e| {
            if e.1 == StartType::Disabled {
                // ERROR_SERVICE_DISABLED
                return Err(Error::Win32 {
                    op: "StartServiceW",
                    code: 1058,
                });
            }
            e.0 = State::Running;
            Ok(())
        })
    }

    fn set_start_type(&self, name: &str, start_type: StartType) -> Result<(), Error> {
        self.mutate(name, Call::SetStartType(name.to_owned(), start_type), |e| {
            e.1 = start_type;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(1);

    #[test]
    fn fake_tracks_state_and_calls() {
        let f = FakeControl::new().with_service("a", State::Running, StartType::Automatic);
        f.stop("a", T).unwrap();
        assert_eq!(f.state("a").unwrap(), State::Stopped);
        f.set_start_type("a", StartType::Manual).unwrap();
        f.start("a", T).unwrap();
        assert_eq!(f.service("a"), Some((State::Running, StartType::Manual)));
        assert_eq!(
            f.calls(),
            [
                Call::Stop("a".into()),
                Call::SetStartType("a".into(), StartType::Manual),
                Call::Start("a".into())
            ]
        );
    }

    #[test]
    fn fake_fails_like_windows() {
        let f = FakeControl::new().with_service("a", State::Stopped, StartType::Disabled);
        assert_eq!(
            f.stop("nope", T).unwrap_err().code(),
            Some(crate::codes::SERVICE_DOES_NOT_EXIST)
        );
        assert_eq!(f.start("a", T).unwrap_err().code(), Some(1058));
        f.fail_next(Error::Timeout { op: "Stopped" });
        assert_eq!(f.stop("a", T), Err(Error::Timeout { op: "Stopped" }));
        f.stop("a", T).unwrap();
        assert_eq!(f.calls(), [Call::Stop("a".into())]);
    }

    #[cfg(not(windows))]
    #[test]
    fn system_is_unsupported_off_windows() {
        let s = system();
        assert_eq!(s.state("a"), Err(Error::Unsupported));
        assert_eq!(s.stop("a", T), Err(Error::Unsupported));
        assert_eq!(s.start("a", T), Err(Error::Unsupported));
        assert_eq!(
            s.set_start_type("a", StartType::Manual),
            Err(Error::Unsupported)
        );
    }
}
