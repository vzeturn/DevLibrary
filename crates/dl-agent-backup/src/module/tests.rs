//! Module lifecycle: opt-in, not enrolled, running after the first poll, degraded on a
//! rejected agent, a prompt stop with a backup in flight; and the `backup.*` commands.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dl_agent::supervisor::ModuleState;
use dl_json::json;

use super::*;
use crate::enroll::enroll;
use crate::fakes::{FakeDest, FakeSql, SqlState};
use crate::state::now_ms;
use crate::state::queue::Queue;
use crate::state::state_dir;
use crate::testhub::{FakeHub, Rig, default_config, eventually, text};
use crate::testutil::temp_dir;

fn module(sql: SqlState) -> (BackupModule, FakeSql) {
    let fake = FakeSql::new(sql);
    (
        BackupModule::new(Arc::new(fake.clone()), Arc::new(FakeDest::default())),
        fake,
    )
}

/// A context over a fresh directory; `config` gets `backup_dir` under it.
fn context(tag: &str, mut config: Value) -> (ModuleContext, PathBuf) {
    let dir = temp_dir(tag);
    let bak = dir.join("bak");
    std::fs::create_dir_all(&bak).unwrap();
    if let Value::Object(m) = &mut config {
        m.insert("backup_dir", bak.display().to_string());
    }
    (ModuleContext::standalone(NAME, config, dir.clone()), dir)
}

fn start(mut m: BackupModule, ctx: &ModuleContext) -> JoinHandle<Result<(), AgentError>> {
    let ctx = ctx.clone();
    std::thread::spawn(move || m.run(&ctx))
}

fn stop_within(ctx: &ModuleContext, h: JoinHandle<Result<(), AgentError>>, limit: Duration) {
    let t0 = Instant::now();
    ctx.stop().stop();
    h.join().unwrap().unwrap();
    assert!(t0.elapsed() < limit, "stopped in {:?}", t0.elapsed());
}

fn command(kind: &str, args: Value) -> Command {
    Command {
        id: "cmd-1".into(),
        kind: kind.into(),
        module: Some(NAME.into()),
        args,
        expires_at: 0,
    }
}

#[test]
fn the_module_is_opt_in() {
    let m = backup_module()();
    assert_eq!(m.name(), "backup");
    assert!(!m.enabled_by_default());
}

#[test]
fn not_enrolled_is_degraded_and_stops_promptly() {
    let (ctx, _) = context("mod-unenrolled", json!({}));
    let h = start(module(SqlState::default()).0, &ctx);
    eventually("degraded", || {
        let s = ctx.status();
        s.state == ModuleState::Degraded && s.detail == "not enrolled"
    });
    stop_within(&ctx, h, Duration::from_secs(1));
}

#[test]
fn an_enrolled_module_runs_after_the_first_poll_and_publishes_metrics() {
    let hub = FakeHub::start(default_config());
    let (ctx, dir) = context("mod-run", json!({"poll_interval_seconds": 1}));
    enroll(&dir, &hub.url(), "tenant-enroll-key", "SQL-HOST", false).unwrap();
    let h = start(module(SqlState::default()).0, &ctx);
    eventually("running", || ctx.status().state == ModuleState::Running);
    eventually("the backup report", || {
        hub.reports().iter().any(|r| text(r, "type") == "backup")
    });
    eventually("metrics", || {
        ctx.status().metrics.get("jobs_ok").and_then(Value::as_u64) == Some(1)
    });
    let m = ctx.status().metrics;
    for key in [
        "queue_depth",
        "outbox_depth",
        "jobs_failed",
        "dead_lettered",
        "bytes_uploaded",
        "last_success",
    ] {
        assert!(m.get(key).is_some(), "metric {key}");
    }
    stop_within(&ctx, h, Duration::from_secs(2));
}

#[test]
fn a_rejected_agent_is_degraded() {
    let hub = FakeHub::start(default_config());
    hub.set(|s| s.status = 401);
    let (ctx, dir) = context("mod-auth", json!({}));
    enroll(&dir, &hub.url(), "tenant-enroll-key", "SQL-HOST", false).unwrap();
    let h = start(module(SqlState::default()).0, &ctx);
    eventually("degraded", || {
        let s = ctx.status();
        s.state == ModuleState::Degraded && s.detail.contains("backup-enroll --force")
    });
    stop_within(&ctx, h, Duration::from_secs(2));
}

#[test]
fn stop_cancels_a_running_backup_and_keeps_it_queued() {
    let hub = FakeHub::start(default_config());
    let (ctx, dir) = context("mod-stop", json!({"stop_grace_seconds": 0}));
    enroll(&dir, &hub.url(), "tenant-enroll-key", "SQL-HOST", false).unwrap();
    let (m, sql) = module(SqlState {
        hang: true,
        ..SqlState::default()
    });
    let h = start(m, &ctx);
    eventually("the backup statement", || {
        sql.state()
            .statements
            .iter()
            .any(|(q, _)| q.starts_with("BACKUP"))
    });
    stop_within(&ctx, h, Duration::from_secs(3));

    let queue = Queue::open(state_dir(&dir).join("queue.enc")).unwrap();
    let pending = queue.pending();
    assert_eq!(pending.len(), 1, "the job stays queued");
    assert_eq!(pending[0].attempts, 0, "a cancelled run spends no attempt");
    assert!(hub.reports().iter().all(|r| text(r, "type") != "backup"));
}

#[test]
fn status_answers_while_not_enrolled() {
    let (ok, out) = answer(None, &command("backup.status", Value::Null), "not enrolled");
    assert!(ok);
    let v: Value = dl_json::from_str(&out).unwrap();
    assert_eq!(v.get("enrolled"), Some(&Value::from(false)));
    assert_eq!(text(&v, "detail"), "not enrolled");
    let (ok, out) = answer(
        None,
        &command("backup.run-now", json!("sc1")),
        "not enrolled",
    );
    assert!(!ok);
    assert_eq!(out, "not enrolled");
}

#[test]
fn run_now_checks_the_schedule_and_queues_it_once() {
    let rig = Rig::new("mod-run-now", default_config());
    let run = |args: Value| answer(Some(&rig.core), &command("backup.run-now", args), "running");
    let (ok, out) = run(json!({"scheduleId": "sc1"}));
    assert!(!ok && out.contains("no config"), "{out}");

    rig.sync().poll(now_ms()).unwrap();
    let (ok, out) = run(json!({"scheduleId": "nope"}));
    assert!(!ok && out.contains("unknown schedule"), "{out}");
    let (ok, out) = run(json!({}));
    assert!(!ok && out.contains("needs a scheduleId"), "{out}");
    assert!(run(json!({"scheduleId": "sc1"})).0);
    assert!(run(json!("sc1")).0);
    assert_eq!(*lock(&rig.core.run_now), ["sc1"]);

    let (ok, out) = answer(
        Some(&rig.core),
        &command("backup.status", Value::Null),
        "running",
    );
    assert!(ok);
    let v: Value = dl_json::from_str(&out).unwrap();
    assert_eq!(v.get("enrolled"), Some(&Value::from(true)));
    assert_eq!(v.get("queue_depth").and_then(Value::as_u64), Some(1));

    let (ok, out) = answer(
        Some(&rig.core),
        &command("backup.explode", Value::Null),
        "running",
    );
    assert!(!ok && out.contains("unknown backup command"), "{out}");
}
