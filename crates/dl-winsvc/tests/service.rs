//! Installs, runs, stops and removes a real service. Needs an elevated shell and the demo
//! service built first:
//!
//! ```text
//! cargo build -p dl-winsvc --example demo_service
//! $env:DL_WINSVC_DEMO_EXE = "<target>\debug\examples\demo_service.exe"
//! cargo test -p dl-winsvc --test service -- --ignored
//! ```
#![cfg(windows)]

use std::time::Duration;

use dl_winsvc::scm::{Manager, RecoveryAction, ServiceConfig, StartType, State};
use dl_winsvc::{codes, command_line};

const NAME: &str = "dl-winsvc-test";

/// Removes the test service even when an assertion fails half-way.
struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Ok(m) = Manager::connect()
            && let Ok(s) = m.open(NAME)
        {
            let _ = s.stop_and_wait(Duration::from_secs(20));
            let _ = s.delete();
        }
    }
}

fn line_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
#[ignore = "needs elevation and DL_WINSVC_DEMO_EXE"]
fn install_run_stop_delete() {
    let exe = std::env::var("DL_WINSVC_DEMO_EXE").expect("set DL_WINSVC_DEMO_EXE");
    let dir = std::env::temp_dir().join("dl winsvc test");
    std::fs::create_dir_all(&dir).unwrap();
    let beat = dir.join("heartbeat.txt");
    let _ = std::fs::remove_file(&beat);

    let m = Manager::connect().expect("elevated shell");
    let _cleanup = Cleanup;
    if let Ok(old) = m.open(NAME) {
        let _ = old.stop_and_wait(Duration::from_secs(20));
        old.delete().unwrap();
        drop(old);
        std::thread::sleep(Duration::from_millis(500));
    }

    let cfg = ServiceConfig::new(
        "dl-winsvc test service",
        StartType::Manual,
        &command_line(&exe, &[beat.to_str().unwrap()]),
    )
    .with_description("Temporary service created by the dl-winsvc tests.");
    let svc = m.create(NAME, &cfg).unwrap();
    assert_eq!(
        m.create(NAME, &cfg).err().and_then(|e| e.code()),
        Some(codes::SERVICE_EXISTS)
    );

    let read = svc.config().unwrap();
    assert_eq!(read.display_name, cfg.display_name);
    assert_eq!(read.description, cfg.description);
    assert_eq!(read.start_type, StartType::Manual);
    assert_eq!(read.binary_path_name, cfg.binary_path_name);
    assert!(
        read.service_start_name.eq_ignore_ascii_case("LocalSystem"),
        "{read:?}"
    );
    assert!(m.list_services().unwrap().iter().any(|s| s.name == NAME));

    svc.start(&[]).unwrap();
    svc.wait_for_state(State::Running, Duration::from_secs(30))
        .unwrap();
    assert!(svc.status().unwrap().accepts_stop);
    std::thread::sleep(Duration::from_millis(1600));
    let beats = line_count(&beat);
    assert!(beats >= 2, "only {beats} heartbeats");

    svc.stop().unwrap();
    svc.wait_for_state(State::Stopped, Duration::from_secs(30))
        .unwrap();
    let text = std::fs::read_to_string(&beat).unwrap();
    assert!(
        text.ends_with("stopped\n") || text.ends_with("stopped\r\n"),
        "{text}"
    );
    let st = svc.status().unwrap();
    assert_eq!(st.win32_exit_code, 0);
    assert_eq!(
        svc.stop().err().and_then(|e| e.code()),
        Some(codes::SERVICE_NOT_ACTIVE)
    );

    // A service that stops by itself with a service-specific code reports 1066 + code.
    let failing = ServiceConfig::new(
        &cfg.display_name,
        StartType::Manual,
        &command_line(&exe, &[beat.to_str().unwrap(), "exit=7"]),
    );
    svc.update_config(&failing).unwrap();
    svc.start(&[]).unwrap();
    svc.wait_for_state(State::Stopped, Duration::from_secs(30))
        .unwrap();
    let st = svc.status().unwrap();
    assert_eq!(
        (st.win32_exit_code, st.service_specific_exit_code),
        (1066, 7)
    );

    // Recovery settings round-trip (the agent's restart 5s/30s/60s, reset after 24h).
    let actions = [
        RecoveryAction::Restart(Duration::from_secs(5)),
        RecoveryAction::Restart(Duration::from_secs(30)),
        RecoveryAction::Restart(Duration::from_secs(60)),
    ];
    let day = Duration::from_secs(24 * 3600);
    svc.set_recovery_actions(&actions, day).unwrap();
    assert_eq!(svc.recovery_actions().unwrap(), (actions.to_vec(), day));
    assert!(!svc.recovery_on_non_crash_failures().unwrap());
    svc.set_recovery_on_non_crash_failures(true).unwrap();
    assert!(svc.recovery_on_non_crash_failures().unwrap());
    svc.set_recovery_actions(&[], Duration::ZERO).unwrap();
    assert_eq!(
        svc.recovery_actions().unwrap(),
        (Vec::new(), Duration::ZERO)
    );
    svc.set_start_type(StartType::Automatic).unwrap();
    assert!(!svc.delayed_auto_start().unwrap());
    svc.set_delayed_auto_start(true).unwrap();
    assert!(svc.delayed_auto_start().unwrap());

    let updated = cfg.clone().with_description("Updated description.");
    svc.update_config(&updated).unwrap();
    assert_eq!(svc.config().unwrap().description, "Updated description.");
    svc.set_start_type(StartType::Disabled).unwrap();
    assert_eq!(svc.config().unwrap().start_type, StartType::Disabled);

    svc.delete().unwrap();
    drop(svc);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        m.open(NAME).err().and_then(|e| e.code()),
        Some(codes::SERVICE_DOES_NOT_EXIST)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
