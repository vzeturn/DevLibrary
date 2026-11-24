# dl-winsvc

Windows services and DPAPI without dependencies: run the current program as a service,
install and control services through the Service Control Manager, and seal secrets with
`CryptProtectData`. On Linux, macOS and WebAssembly the same API compiles and every call
returns `Error::Unsupported` (or `Ok(false)` for `is_service_process`), so one code base
can choose at run time.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| Go        | `golang.org/x/sys/windows/svc`, `svc/mgr`, hand-written `crypt32` DPAPI calls |
| .NET      | `Microsoft.Extensions.Hosting.WindowsServices`, `System.ServiceProcess.ServiceController`, `System.Security.Cryptography.ProtectedData` |
| Rust      | `windows-service`, `windows-sys` |

## Run as a service, with a console fallback

```rust,no_run
use std::time::Duration;
use dl_winsvc::{StopSignal, is_service_process, run_service};

fn work(stop: StopSignal) {
    // Poll loop: wait_timeout doubles as the sleep and returns early on Stop.
    while !stop.wait_timeout(Duration::from_secs(30)) {
        // one cycle of work
    }
}

fn main() -> Result<(), dl_winsvc::Error> {
    if is_service_process()? {
        // Reports Running at once, so slow start-up never hits the SCM's 30 s limit.
        run_service("My Agent", work)
    } else {
        let stop = StopSignal::new();
        // Hook Ctrl+C to `stop.stop()` here if you need a clean console shutdown.
        work(stop);
        Ok(())
    }
}
```

`run_service` blocks until the service stops. On Stop or Shutdown the service reports
StopPending, signals the `StopSignal`, keeps reporting progress every second while the
work finishes, then reports Stopped. A panic in the work is caught: the service stops with
a service-specific error (Win32 code 1066) and `run_service` returns `Error::WorkPanicked`.

## Install, update, start, stop, remove

```rust,no_run
use std::time::Duration;
use dl_winsvc::scm::{Manager, ServiceConfig, StartType, State};
use dl_winsvc::{codes, command_line};

let m = Manager::connect()?; // needs an elevated process
let cfg = ServiceConfig::new(
    "My Agent",
    StartType::Automatic,
    &command_line(r"C:\ProgramData\My Agent\agent.exe", &["-config", r"C:\ProgramData\My Agent"]),
)
.with_description("Collects invoices from the local SQL Server.");

let svc = match m.open("My Agent") {
    Ok(s) => {
        s.update_config(&cfg)?;
        s.stop_and_wait(Duration::from_secs(30))?;
        s
    }
    Err(e) if e.code() == Some(codes::SERVICE_DOES_NOT_EXIST) => m.create("My Agent", &cfg)?,
    Err(e) => return Err(e),
};
svc.start(&[])?;
svc.wait_for_state(State::Running, Duration::from_secs(30))?;

for s in m.list_services()? {
    if s.name.starts_with("My ") {
        println!("{} {:?}", s.name, s.status.state);
    }
}
# Ok::<(), dl_winsvc::Error>(())
```

`command_line` quotes the executable and each argument exactly like Go's
`syscall.EscapeArg`, so paths with spaces work. Services are created as
`SERVICE_WIN32_OWN_PROCESS` under LocalSystem with normal error control.

## Recovery, delayed start and exit codes

```rust,no_run
use std::time::Duration;
use dl_winsvc::scm::{Manager, RecoveryAction};

let svc = Manager::connect()?.open("My Agent")?;
// Restart after 5 s, 30 s, then 60 s for every later failure; forget failures after a day.
svc.set_recovery_actions(
    &[
        RecoveryAction::Restart(Duration::from_secs(5)),
        RecoveryAction::Restart(Duration::from_secs(30)),
        RecoveryAction::Restart(Duration::from_secs(60)),
    ],
    Duration::from_secs(24 * 3600),
)?;
// Count a stop with a non-zero exit code as a failure too, not only crashes.
svc.set_recovery_on_non_crash_failures(true)?;
svc.set_delayed_auto_start(true)?;
# Ok::<(), dl_winsvc::Error>(())
```

With `run_service_with_exit_code` the work returns a `u32`: 0 is a clean stop that
recovery never restarts, any other value stops the service with Win32 code 1066 and that
service-specific code, which triggers the recovery actions when the flag above is on.

## A mockable control surface

`control::ServiceControl` covers stop, start and set-start-type by service name, so code that
operates services remotely can be tested without Windows. `control::system()` is the Service
Control Manager on Windows and fails with `Error::Unsupported` elsewhere; `control::FakeControl`
is an in-memory table that records each call.

```rust
use std::time::Duration;
use dl_winsvc::control::{Call, FakeControl, ServiceControl};
use dl_winsvc::scm::{StartType, State};

let fake = FakeControl::new().with_service("MyAgent", State::Running, StartType::Automatic);
fake.stop("MyAgent", Duration::from_secs(30)).unwrap();
assert_eq!(fake.state("MyAgent").unwrap(), State::Stopped);
assert_eq!(fake.calls(), [Call::Stop("MyAgent".into())]);
```

## File and directory ACLs

`acl::restrict(path)` makes Administrators the owner and sets a protected DACL that grants
full control to SYSTEM and Administrators only (inherited by children of a directory).
`acl::read(path)` returns the owner, the protection flag and the ACEs as SID strings, and
`acl::check` reports what is wrong with a descriptor, so a program can repair a widened
state directory at start-up.

## DPAPI

```rust,no_run
use dl_winsvc::dpapi::{Scope, protect, unprotect};

let blob = protect(b"api key", Scope::LocalMachine)?;
assert_eq!(unprotect(&blob, Scope::LocalMachine)?, b"api key");
# Ok::<(), dl_winsvc::Error>(())
```

`Scope::LocalMachine` lets any account on the machine open the blob: an administrator can
seal a configuration that the LocalSystem service reads. `Scope::CurrentUser` limits it to
the sealing user. No description is used. `protect_with_entropy` / `unprotect_with_entropy`
bind the blob to an extra secret (`pOptionalEntropy`); opening it needs the same bytes.
Altered blobs fail to open.

Blobs written by the Go on-prem agent contain `BE32(len(plaintext)) || plaintext`, sealed
with `CRYPTPROTECT_LOCAL_MACHINE`. These functions do no framing:

```rust,no_run
use dl_winsvc::dpapi::{Scope, protect, unprotect};

fn seal_like_go(p: &[u8]) -> Result<Vec<u8>, dl_winsvc::Error> {
    let mut framed = (p.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(p);
    protect(&framed, Scope::LocalMachine)
}

fn open_like_go(blob: &[u8]) -> Option<Vec<u8>> {
    let plain = unprotect(blob, Scope::LocalMachine).ok()?;
    let (len, rest) = plain.split_at_checked(4)?;
    (u32::from_be_bytes(len.try_into().ok()?) as usize == rest.len()).then(|| rest.to_vec())
}
```

## Running processes

`process::list` returns every running process (id, parent id, session, image name) from one
`NtQuerySystemInformation` call, in place of running `tasklist` and parsing its localised CSV.
It needs no elevation; off Windows it returns `Error::Unsupported`.

```rust,no_run
use dl_winsvc::process;

// Is the old per-product agent still running? Case and ".exe" do not matter.
let old = process::named("MM.Arrival.Agent")?;
for p in &old {
    println!("{} pid {} (parent {}, session {})", p.name, p.pid, p.parent_pid, p.session_id);
}
# Ok::<(), dl_winsvc::Error>(())
```

## Detecting a service process

`is_service_process` checks that the parent process is `services.exe` in session 0, the
same rule as Go's `svc.IsWindowsService` and .NET's `WindowsServiceHelpers`. It reads the
process list with `NtQuerySystemInformation` and does not need elevation.

## Limits

- One service per process (`SERVICE_WIN32_OWN_PROCESS`); a second `run_service` call
  returns `Error::AlreadyRunning`.
- Pause/continue, custom control codes, reboot messages, failure commands and service
  accounts other than LocalSystem are not exposed.
- All `unsafe` code is in `src/sys_windows.rs`; buffers returned by Win32 are read with
  bounds checks.
