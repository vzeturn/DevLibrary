use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::store::testdir::TestDir;

/// Panics on its first `fail` runs, then runs until stopped and answers `echo`.
struct Flaky {
    runs: Arc<AtomicU32>,
    fail: u32,
}

impl Module for Flaky {
    fn name(&self) -> &str {
        "flaky"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        assert!(n >= self.fail, "boom {n}");
        ctx.set_running("ok");
        while !ctx.stop().wait_timeout(Duration::from_millis(10)) {
            while let Some(c) = ctx.next_command() {
                ctx.complete(&c.id, true, &format!("echo {}", c.kind));
            }
        }
        Ok(())
    }
}

fn flaky(fail: u32) -> (Factory, Arc<AtomicU32>) {
    let runs = Arc::new(AtomicU32::new(0));
    let r = runs.clone();
    (
        Arc::new(move || {
            Box::new(Flaky {
                runs: r.clone(),
                fail,
            })
        }),
        runs,
    )
}

fn tick_until(
    sup: &mut Supervisor,
    cfg: &RemoteConfig,
    ms: u64,
    done: impl Fn(&Supervisor) -> bool,
) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        sup.tick(cfg);
        if done(sup) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn state(sup: &Supervisor) -> ModuleState {
    sup.statuses()[0].state
}

#[test]
fn backoff_doubles_caps_and_resets() {
    assert_eq!(next_backoff(Duration::ZERO, Duration::ZERO), BACKOFF_MIN);
    assert_eq!(
        next_backoff(Duration::from_secs(4), Duration::from_secs(1)),
        Duration::from_secs(8)
    );
    assert_eq!(
        next_backoff(Duration::from_secs(200), Duration::ZERO),
        BACKOFF_MAX
    );
    assert_eq!(
        next_backoff(Duration::from_secs(200), BACKOFF_RESET),
        BACKOFF_MIN
    );
}

#[test]
fn panicking_module_is_reported_and_restarted() {
    let d = TestDir::new("sup-panic");
    let spool = Spool::in_memory();
    let mut sup = Supervisor::new(d.0.clone(), Outbox::default(), spool.clone());
    let (f, runs) = flaky(1);
    sup.add(f);
    let cfg = RemoteConfig::default();
    assert!(tick_until(&mut sup, &cfg, 3000, |s| state(s) == ModuleState::Running));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let st = &sup.statuses()[0];
    assert_eq!(st.restarts, 1);
    assert!(st.last_error.contains("boom 0"), "{}", st.last_error);
    assert_eq!(st.degraded_since, 0, "set_running clears degraded_since");
    let reports = spool.peek(10);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].code, "module_failed");
    assert!(sup.all_running());
    assert!(d.0.join("modules/flaky").is_dir());
    sup.stop_all(Duration::from_secs(2));
    assert_eq!(state(&sup), ModuleState::Stopped);
}

#[test]
fn failing_module_counts_as_unhealthy() {
    let d = TestDir::new("sup-unhealthy");
    let mut sup = Supervisor::new(d.0.clone(), Outbox::default(), Spool::in_memory());
    let (f, _) = flaky(u32::MAX);
    sup.add(f);
    let cfg = RemoteConfig::default();
    assert!(tick_until(&mut sup, &cfg, 2000, |s| s.statuses()[0]
        .restarts
        >= 1));
    let since = sup.statuses()[0].degraded_since;
    assert!(since > 0);
    assert!(!sup.all_running());
    assert!(sup.any_unhealthy_for(60, since + 60));
    assert!(!sup.any_unhealthy_for(60, since + 59));
    sup.stop_all(Duration::from_secs(1));
}

#[test]
fn disabled_module_stops_and_does_not_count_against() {
    let d = TestDir::new("sup-disabled");
    let mut sup = Supervisor::new(d.0.clone(), Outbox::default(), Spool::in_memory());
    let (f, _) = flaky(0);
    sup.add(f);
    let on = RemoteConfig::default();
    assert!(tick_until(&mut sup, &on, 2000, |s| state(s) == ModuleState::Running));
    let off = RemoteConfig::from_json(
        &dl_json::json!({"version": 2, "body": {"modules": {"flaky": {"enabled": false}}}}),
    )
    .unwrap();
    assert!(tick_until(&mut sup, &off, 2000, |s| state(s) == ModuleState::Disabled));
    assert!(sup.all_running());
    assert!(!sup.any_unhealthy_for(0, u64::MAX));
}

/// Runs until stopped; only on sites whose config says `"enabled": true`.
struct OptIn;

impl Module for OptIn {
    fn name(&self) -> &str {
        "optin"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        ctx.set_running("ok");
        ctx.stop().wait();
        Ok(())
    }

    fn enabled_by_default(&self) -> bool {
        false
    }
}

#[test]
fn opt_in_module_waits_for_enabled_true() {
    let d = TestDir::new("sup-optin");
    let mut sup = Supervisor::new(d.0.clone(), Outbox::default(), Spool::in_memory());
    sup.add(Arc::new(|| Box::new(OptIn)));
    let unset = RemoteConfig::default();
    sup.tick(&unset);
    assert_eq!(state(&sup), ModuleState::Disabled);
    let on = RemoteConfig::from_json(
        &dl_json::json!({"version": 2, "body": {"modules": {"optin": {"enabled": true}}}}),
    )
    .unwrap();
    assert!(tick_until(&mut sup, &on, 2000, |s| state(s) == ModuleState::Running));
    assert!(tick_until(&mut sup, &unset, 2000, |s| state(s)
        == ModuleState::Disabled));
}

#[test]
fn commands_reach_running_modules_only() {
    let d = TestDir::new("sup-cmd");
    let outbox = Outbox::default();
    let mut sup = Supervisor::new(d.0.clone(), outbox.clone(), Spool::in_memory());
    let (f, _) = flaky(0);
    sup.add(f);
    let cmd = |m: &str| Command {
        id: "c1".into(),
        kind: "flaky.echo".into(),
        module: Some(m.into()),
        args: Value::Null,
        expires_at: u64::MAX,
    };
    assert!(sup.deliver(cmd("flaky")).is_err(), "not started yet");
    let cfg = RemoteConfig::default();
    assert!(tick_until(&mut sup, &cfg, 2000, |s| state(s) == ModuleState::Running));
    assert!(sup.deliver(cmd("nope")).is_err());
    sup.deliver(cmd("flaky")).unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    let mut got = Vec::new();
    while got.is_empty() && Instant::now() < end {
        got = outbox.take();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].output, "echo flaky.echo");
    assert!(sup.restart("flaky", Duration::from_secs(2)));
    assert!(!sup.restart("nope", Duration::from_secs(1)));
    assert!(tick_until(&mut sup, &cfg, 2000, |s| state(s) == ModuleState::Running));
    assert_eq!(
        sup.statuses()[0].restarts,
        0,
        "a manual restart is not a failure"
    );
    sup.stop_all(Duration::from_secs(2));
}

#[test]
fn heartbeat_json_clips_long_fields() {
    let ctx = ModuleContext::standalone("m", Value::Null, std::env::temp_dir());
    ctx.set_degraded(&"x".repeat(MAX_DETAIL + 10));
    let v = ctx.status().to_json();
    assert_eq!(
        v.get("detail")
            .and_then(Value::as_str)
            .unwrap()
            .chars()
            .count(),
        MAX_DETAIL
    );
    assert_eq!(v.get("state").and_then(Value::as_str), Some("degraded"));
    assert_eq!(clip("ệệệ", 2), "ệệ");
}
