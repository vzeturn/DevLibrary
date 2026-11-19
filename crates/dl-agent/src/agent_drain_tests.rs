//! Draining modules before an update swaps the binary.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::tests::{agent, signed_manifest_on};
use crate::Error;
use crate::agent::{Exit, STOP_WAIT};
use crate::hub::tests_support::FakeHub;
use crate::store::testdir::TestDir;
use crate::supervisor::{Factory, Module, ModuleContext};

/// Finishes its batch (150 ms) after the stop signal, then notes what binary is on disk.
struct Careful {
    exe: PathBuf,
    seen: Arc<Mutex<Option<Vec<u8>>>>,
}

impl Module for Careful {
    fn name(&self) -> &str {
        "careful"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        ctx.set_running("working");
        while !ctx.stop().wait_timeout(Duration::from_millis(10)) {}
        std::thread::sleep(Duration::from_millis(150));
        *self.seen.lock().unwrap() = Some(std::fs::read(&self.exe).unwrap_or_default());
        Ok(())
    }
}

/// Ignores the stop signal until released.
struct Stuck(Arc<AtomicBool>);

impl Module for Stuck {
    fn name(&self) -> &str {
        "stuck"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        ctx.set_running("busy");
        while !self.0.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

fn update_script(hub: &FakeHub, new: &[u8]) {
    hub.push(200, r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": true}"#)
        .push(200, &signed_manifest_on("canary", "99.0.0", new, 7))
        .push(200, std::str::from_utf8(new).unwrap());
}

fn start(a: &mut super::Agent) {
    let end = Instant::now() + Duration::from_secs(3);
    while !a.supervisor.all_running() && Instant::now() < end {
        a.supervisor.tick(&a.config.current);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(a.supervisor.all_running());
}

#[test]
fn in_flight_work_finishes_on_the_old_binary_before_the_swap() {
    let d = TestDir::new("agent-drain");
    let hub = FakeHub::default();
    let seen = Arc::new(Mutex::new(None));
    let (exe, s) = (d.0.join("bin").join("dl-agent"), Arc::clone(&seen));
    let f: Factory = Arc::new(move || {
        Box::new(Careful {
            exe: exe.clone(),
            seen: Arc::clone(&s),
        }) as Box<dyn Module>
    });
    let mut a = agent(&d, &hub, vec![f]);
    start(&mut a);
    update_script(&hub, b"new binary");
    assert_eq!(a.step(1000), Some(Exit::Restart));
    assert_eq!(seen.lock().unwrap().as_deref(), Some(b"old".as_slice()));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"new binary");
}

#[test]
fn a_stuck_module_is_reported_and_does_not_block_the_update() {
    let d = TestDir::new("agent-drain-stuck");
    let hub = FakeHub::default();
    let release = Arc::new(AtomicBool::new(false));
    let r = Arc::clone(&release);
    let f: Factory = Arc::new(move || Box::new(Stuck(Arc::clone(&r))) as Box<dyn Module>);
    let mut a = agent(&d, &hub, vec![f]);
    a.opts.drain_wait = Duration::from_millis(200);
    start(&mut a);
    update_script(&hub, b"new binary");
    let t = Instant::now();
    assert_eq!(a.step(1000), Some(Exit::Restart));
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"new binary");
    let codes: Vec<String> = a.spool().peek(50).into_iter().map(|r| r.code).collect();
    assert!(codes.iter().any(|c| c == "module_stuck"), "{codes:?}");
    release.store(true, Ordering::Relaxed);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn a_fault_build_rolls_back_at_the_end_of_probation_and_reports_it() {
    let d = TestDir::new("agent-fault");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, Vec::new());
    a.opts.fault = Some(crate::fault::PROBATION);
    a.hub_ok = true;
    std::fs::create_dir_all(d.0.join("bin/previous")).unwrap();
    std::fs::write(d.0.join("bin/previous/dl-agent"), b"good").unwrap();
    a.update.pending = Some(crate::update::Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 500,
        boots: 1,
        // The built-in module is not started in this test; probation ignores it.
        unhealthy_before: vec!["sys".into()],
    });
    // Healthy and linked, yet never committed before the end of probation...
    assert_eq!(a.watch_probation(499), None);
    assert!(a.update.pending.is_some());
    // ...and rolled back at the end, counted and reported.
    assert_eq!(a.watch_probation(500), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"good");
    assert_eq!(a.update.failed_versions(), [a.opts.version.clone()]);
    let r = a.spool().peek(50);
    let rolled = r.iter().find(|r| r.code == "update_rolled_back").unwrap();
    assert!(
        rolled.message.contains("DL_AGENT_FAULT=probation"),
        "{}",
        rolled.message
    );
    // A normal build in the same state commits.
    a.opts.fault = None;
    a.update.pending = Some(crate::update::Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 500,
        boots: 1,
        unhealthy_before: vec!["sys".into()],
    });
    assert_eq!(a.watch_probation(500), None);
    assert!(a.update.pending.is_none());
}
