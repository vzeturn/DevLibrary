//! Running the current process as a Windows service.

use core::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::raw::*;
use crate::wide::to_wide;
use crate::{Error, sys};

/// Tells service work that it should stop. Cheap to clone; all clones share one flag.
#[derive(Clone, Debug, Default)]
pub struct StopSignal {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl StopSignal {
    /// A signal that is not stopped yet. Useful when the same work runs as a console app.
    pub fn new() -> StopSignal {
        StopSignal::default()
    }

    /// Requests a stop and wakes every waiter.
    pub fn stop(&self) {
        let (flag, cv) = &*self.inner;
        *flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cv.notify_all();
    }

    /// Whether a stop was requested.
    pub fn is_stopped(&self) -> bool {
        *self.inner.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Blocks until a stop is requested.
    pub fn wait(&self) {
        #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
        {
            let (flag, cv) = &*self.inner;
            let mut stopped = flag.lock().unwrap_or_else(PoisonError::into_inner);
            while !*stopped {
                stopped = cv.wait(stopped).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }

    /// Waits up to `timeout` for a stop; returns whether one was requested. Use it as the
    /// sleep between iterations of a polling loop so a stop interrupts the sleep.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        #[cfg(not(all(target_arch = "wasm32", not(target_feature = "atomics"))))]
        {
            let (flag, cv) = &*self.inner;
            let guard = flag.lock().unwrap_or_else(PoisonError::into_inner);
            let (stopped, _) = cv
                .wait_timeout_while(guard, timeout, |stopped| !*stopped)
                .unwrap_or_else(PoisonError::into_inner);
            *stopped
        }
        #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
        {
            let _ = timeout;
            self.is_stopped()
        }
    }
}

/// Whether the Service Control Manager started this process: the parent process is
/// `services.exe` in session 0 (the same check as Go's `svc.IsWindowsService` and .NET's
/// `WindowsServiceHelpers.IsWindowsService`). Always `Ok(false)` outside Windows.
pub fn is_service_process() -> Result<bool, Error> {
    sys::parent_is_services()
}

type Work = Box<dyn FnOnce(StopSignal) + Send>;

enum Event {
    Stop,
    Interrogate,
    WorkDone { panicked: bool },
}

struct Slot {
    name: Vec<u16>,
    work: Option<Work>,
    events: Option<Sender<Event>>,
    panicked: bool,
    failure: Option<Error>,
}

static STARTED: AtomicBool = AtomicBool::new(false);
static SLOT: Mutex<Slot> = Mutex::new(Slot {
    name: Vec::new(),
    work: None,
    events: None,
    panicked: false,
    failure: None,
});

fn slot() -> MutexGuard<'static, Slot> {
    SLOT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `work` as the Windows service `name` and blocks until the service has stopped.
///
/// The service reports Running at once (before `work` does anything, so slow start-up never
/// hits the SCM's 30-second limit) and accepts Stop and Shutdown. `work` runs on its own
/// thread; on Stop or Shutdown the service reports StopPending, signals the [`StopSignal`]
/// and waits for `work` to return, then reports Stopped. If `work` returns by itself the
/// service stops too. If `work` panics the service stops with a service-specific error and
/// this function returns [`Error::WorkPanicked`].
///
/// Called from a process the SCM did not start, it fails with code
/// [`codes::FAILED_SERVICE_CONTROLLER_CONNECT`](crate::codes::FAILED_SERVICE_CONTROLLER_CONNECT);
/// check [`is_service_process`] first to choose between service and console mode. Only one
/// service per process is supported: a second call fails with [`Error::AlreadyRunning`].
pub fn run_service<F>(name: &str, work: F) -> Result<(), Error>
where
    F: FnOnce(StopSignal) + Send + 'static,
{
    let wide = to_wide(name)?;
    if STARTED.swap(true, Ordering::SeqCst) {
        return Err(Error::AlreadyRunning);
    }
    {
        let mut s = slot();
        s.name = wide.clone();
        s.work = Some(Box::new(work));
        s.panicked = false;
        s.failure = None;
    }
    if let Err(e) = sys::start_dispatcher(&wide, service_main) {
        // Never connected: allow a later call (for example after a console fallback).
        slot().work = None;
        STARTED.store(false, Ordering::SeqCst);
        return Err(e);
    }
    let mut s = slot();
    if let Some(e) = s.failure.take() {
        return Err(e);
    }
    if s.panicked {
        Err(Error::WorkPanicked)
    } else {
        Ok(())
    }
}

extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    // A panic must not cross this FFI boundary (it would abort the process).
    if catch_unwind(service_body).is_err() {
        slot().panicked = true;
    }
}

extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    const NO_ERROR: u32 = 0;
    const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
    let event = match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => Event::Stop,
        SERVICE_CONTROL_INTERROGATE => Event::Interrogate,
        _ => return ERROR_CALL_NOT_IMPLEMENTED,
    };
    if let Some(tx) = slot().events.as_ref() {
        let _ = tx.send(event);
    }
    NO_ERROR
}

fn service_body() {
    let (tx, rx) = mpsc::channel();
    let (name, work) = {
        let mut s = slot();
        s.events = Some(tx.clone());
        (s.name.clone(), s.work.take())
    };
    let Some(work) = work else { return };
    let handle = match sys::register_handler(&name, control_handler) {
        Ok(h) => h,
        Err(e) => {
            slot().failure = Some(e);
            return;
        }
    };

    let mut status = ServiceStatus {
        service_type: SERVICE_WIN32_OWN_PROCESS,
        current_state: SERVICE_START_PENDING,
        check_point: 1,
        wait_hint: 5000,
        ..ServiceStatus::default()
    };
    let _ = handle.set(&status);
    status.current_state = SERVICE_RUNNING;
    status.controls_accepted = SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN;
    status.check_point = 0;
    status.wait_hint = 0;
    let _ = handle.set(&status);

    let signal = StopSignal::new();
    let worker_signal = signal.clone();
    let spawned = std::thread::Builder::new()
        .name("service-work".into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| work(worker_signal)));
            let _ = tx.send(Event::WorkDone {
                panicked: result.is_err(),
            });
        });
    let worker = match spawned {
        Ok(w) => w,
        Err(_) => {
            stop_with(&handle, &mut status, true);
            slot().panicked = true;
            return;
        }
    };

    let mut stopping = false;
    let mut panicked = false;
    loop {
        let event = if stopping {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(e) => e,
                Err(RecvTimeoutError::Timeout) => {
                    // Still shutting down: tell the SCM we are making progress.
                    status.check_point += 1;
                    let _ = handle.set(&status);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(e) => e,
                Err(_) => break,
            }
        };
        match event {
            Event::Interrogate => {
                let _ = handle.set(&status);
            }
            Event::Stop if !stopping => {
                stopping = true;
                status.current_state = SERVICE_STOP_PENDING;
                status.controls_accepted = 0;
                status.check_point = 1;
                status.wait_hint = 5000;
                let _ = handle.set(&status);
                signal.stop();
            }
            Event::Stop => {}
            Event::WorkDone { panicked: p } => {
                panicked = p;
                break;
            }
        }
    }
    let _ = worker.join();
    {
        let mut s = slot();
        s.events = None;
        s.panicked = panicked;
    }
    // The outcome is recorded before Stopped is reported: after that the dispatcher may
    // return and the process exit.
    stop_with(&handle, &mut status, panicked);
}

fn stop_with(handle: &sys::StatusHandle, status: &mut ServiceStatus, failed: bool) {
    const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;
    status.current_state = SERVICE_STOPPED;
    status.controls_accepted = 0;
    status.check_point = 0;
    status.wait_hint = 0;
    if failed {
        status.win32_exit_code = ERROR_SERVICE_SPECIFIC_ERROR;
        status.service_specific_exit_code = 1;
    }
    let _ = handle.set(status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn stop_signal_wakes_waiters() {
        let s = StopSignal::new();
        assert!(!s.is_stopped());
        let t0 = Instant::now();
        assert!(!s.wait_timeout(Duration::from_millis(30)));
        assert!(t0.elapsed() >= Duration::from_millis(25));
        let s2 = s.clone();
        let h = std::thread::spawn(move || {
            s2.wait();
            s2.is_stopped()
        });
        std::thread::sleep(Duration::from_millis(20));
        s.stop();
        assert!(h.join().unwrap());
        assert!(s.wait_timeout(Duration::from_secs(10)));
    }

    #[test]
    fn test_process_is_not_a_service() {
        assert!(!is_service_process().unwrap());
    }

    #[test]
    fn dispatcher_refuses_outside_the_scm() {
        let e = run_service("dl-winsvc-unit", |_| {}).unwrap_err();
        if cfg!(windows) {
            assert_eq!(
                e.code(),
                Some(crate::codes::FAILED_SERVICE_CONTROLLER_CONNECT),
                "{e}"
            );
        } else {
            assert_eq!(e, Error::Unsupported);
        }
        // The failed attempt does not block a later one.
        assert!(!matches!(
            run_service("dl-winsvc-unit", |_| {}),
            Err(Error::AlreadyRunning)
        ));
        assert!(matches!(
            run_service("bad\0name", |_| {}),
            Err(Error::InvalidInput(_))
        ));
    }
}
