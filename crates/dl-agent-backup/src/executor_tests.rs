//! Go `executor` tests: success, retry and dead letter, the instance gate, plus the
//! permanent-failure and stop/cancel behaviour.

use std::time::Duration;

use dl_json::json;

use super::*;
use crate::fakes::SqlState;
use crate::state::queue::DEFAULT_MAX_ATTEMPTS;
use crate::sync::SyncLoop;
use crate::testhub::{Rig, default_config, eventually, put, steps, text};

fn queued(tag: &str, cfg: dl_json::Value, sql: SqlState) -> Rig {
    let hub = crate::testhub::FakeHub::start(cfg);
    let client = hub.client();
    let rig = Rig::open(hub, client, crate::testutil::temp_dir(tag), sql);
    SyncLoop::new(rig.core.clone()).poll(now_ms()).unwrap();
    rig
}

fn backups(rig: &Rig) -> Vec<dl_json::Value> {
    rig.hub
        .reports()
        .into_iter()
        .filter(|r| text(r, "type") == "backup")
        .collect()
}

#[test]
fn a_successful_job_is_recorded_counted_and_reported() {
    let rig = queued("exec-ok", default_config(), SqlState::default());
    let job = rig.next_job();
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);

    assert_eq!(lock(&rig.core.queue).depth(), 0);
    let rec = lock(&rig.core.history).find(&job.id).unwrap();
    assert!(rec.cloud_file_path.starts_with("fake://s3/"));
    assert_eq!(rec.provider, "s3");
    let stats = lock(&rig.core.stats).clone();
    assert_eq!((stats.jobs_ok, stats.jobs_failed), (1, 0));
    assert_eq!(stats.bytes_uploaded, b"fake backup bytes".len() as u64);
    assert!(stats.last_success.contains_key("Sales"));
    let recent = lock(&rig.core.history).recent(5);
    assert!(recent[0].ok);
    let r = &backups(&rig)[0];
    let names: Vec<String> = steps(r).into_iter().map(|s| s.0).collect();
    assert_eq!(
        names,
        [
            "connect", "backup", "checksum", "encrypt", "upload", "cleanup"
        ]
    );
    assert!(rig.codes().is_empty());
}

#[test]
fn a_backup_kept_on_the_server_counts_no_uploaded_bytes() {
    let mut cfg = default_config();
    put(
        &mut cfg,
        "/schedules/0/cloudDestination/encryptedTokens",
        json!(""),
    );
    let rig = queued("exec-local", cfg, SqlState::default());
    let job = rig.next_job();
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);
    assert_eq!(text(&backups(&rig)[0], "status"), "Succeeded");
    assert_eq!(text(&backups(&rig)[0], "cloudFilePath"), "");
    assert_eq!(lock(&rig.core.stats).bytes_uploaded, 0);
}

#[test]
fn a_log_backup_on_simple_recovery_fails_once_with_its_code() {
    let mut cfg = default_config();
    put(&mut cfg, "/schedules/0/backupType", json!("Log"));
    let mut sql = SqlState::default();
    sql.recovery.insert("Sales".into(), "SIMPLE".into());
    let rig = queued("exec-simple", cfg, sql);
    let job = rig.next_job();
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);

    assert_eq!(lock(&rig.core.queue).depth(), 0, "no attempt can fix it");
    assert_eq!(rig.codes(), ["backup_recovery_model"]);
    let r = &backups(&rig)[0];
    assert_eq!(text(r, "status"), "Failed");
    assert!(
        steps(r)
            .iter()
            .any(|s| s.1 == "error" && s.2.contains("SIMPLE")),
        "{:?}",
        steps(r)
    );
    assert_eq!(lock(&rig.core.stats).jobs_failed, 1);
    assert!(
        rig.sql
            .state()
            .statements
            .iter()
            .all(|(q, _)| !q.starts_with("BACKUP"))
    );
}

#[test]
fn a_transient_failure_is_retried_then_abandoned_once() {
    let sql = SqlState {
        run_error: Some(Error::Sql("disk full".into())),
        ..SqlState::default()
    };
    let rig = queued("exec-retry", default_config(), sql);
    let gate = Gate::default();
    let stop = StopSignal::new();
    for attempt in 1..=DEFAULT_MAX_ATTEMPTS {
        let job = rig.next_job();
        execute(&rig.core, &gate, &stop, &job);
        let q = lock(&rig.core.queue);
        let j = q
            .pending()
            .into_iter()
            .chain(q.dead())
            .find(|j| j.id == job.id)
            .unwrap();
        assert_eq!(j.attempts, attempt);
        assert!(j.last_error.contains("disk full"));
    }
    assert_eq!(lock(&rig.core.queue).dead().len(), 1);
    let history_id = lock(&rig.core.queue).dead()[0].id.clone();
    drain_dead(&rig.core);

    assert_eq!(lock(&rig.core.queue).depth(), 0);
    assert!(lock(&rig.core.queue).dead().is_empty());
    let stats = lock(&rig.core.stats).clone();
    assert_eq!((stats.jobs_failed, stats.dead_lettered), (3, 1));
    let reports = backups(&rig);
    assert!(
        reports.iter().all(|r| text(r, "historyId") == history_id),
        "one hub history row"
    );
    let last = steps(reports.last().unwrap());
    assert_eq!(last[0].0, "queue");
    assert!(
        last[0].2.starts_with("abandoned after 3/3 attempts: "),
        "{}",
        last[0].2
    );
    assert!(rig.codes().contains(&"backup_sql_failed".to_owned()));
}

#[test]
fn a_cancelled_job_goes_back_to_the_queue_without_an_attempt() {
    let sql = SqlState {
        hang: true,
        ..SqlState::default()
    };
    let rig = queued("exec-cancel", default_config(), sql);
    let job = rig.next_job();
    let core = rig.core.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        core.cancel.cancel();
    });
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);
    canceller.join().unwrap();

    let pending = lock(&rig.core.queue).pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 0);
    assert!(pending[0].lease_until.is_none());
    assert!(backups(&rig).is_empty());
    assert_eq!(lock(&rig.core.stats).jobs_failed, 0);
}

#[test]
fn a_stopped_worker_does_not_start_a_job() {
    let rig = queued("exec-stopped", default_config(), SqlState::default());
    let job = rig.next_job();
    let stop = StopSignal::new();
    stop.stop();
    execute(&rig.core, &Gate::default(), &stop, &job);
    let statements = rig.sql.state().statements.clone();
    assert!(
        statements.iter().all(|(q, _)| !q.starts_with("BACKUP")),
        "{statements:?}"
    );
    assert_eq!(lock(&rig.core.queue).pending()[0].attempts, 0);
    assert!(backups(&rig).is_empty());
}

#[test]
fn a_job_with_unreadable_parameters_is_dropped() {
    let rig = Rig::new("exec-unreadable", default_config());
    lock(&rig.core.queue)
        .enqueue(Job::new("j1", "sc1", json!({"bogus": true}), now_ms()))
        .unwrap();
    let job = rig.next_job();
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);
    assert_eq!(lock(&rig.core.queue).depth(), 0);
    assert_eq!(rig.codes(), ["backup_schedule_invalid"]);
}

#[test]
fn the_gate_is_one_lock_per_instance() {
    let gate = Gate::default();
    assert!(Arc::ptr_eq(
        &gate.lock_for("sql1:1433"),
        &gate.lock_for("sql1:1433")
    ));
    assert!(!Arc::ptr_eq(
        &gate.lock_for("sql1:1433"),
        &gate.lock_for("sql2:1433")
    ));
}

#[test]
fn permanent_errors_are_the_ones_no_attempt_fixes() {
    assert!(is_permanent(&Error::RecoveryModel("x".into())));
    assert!(is_permanent(&Error::Unsupported("x".into())));
    assert!(is_permanent(&Error::Credential("x".into())));
    assert!(is_permanent(&Error::Schedule("x".into())));
    assert!(!is_permanent(&Error::Sql("x".into())));
    assert!(!is_permanent(&Error::Upload("x".into())));
    assert!(!is_permanent(&Error::Hub("x".into())));
}

#[test]
fn a_worker_picks_up_new_work_and_stops_promptly() {
    let rig = Rig::new("exec-work", default_config());
    let stop = StopSignal::new();
    let (core, s) = (rig.core.clone(), stop.clone());
    let worker = std::thread::spawn(move || work(&core, &Gate::default(), &s));
    SyncLoop::new(rig.core.clone()).poll(now_ms()).unwrap();
    eventually("the backup report", || !backups(&rig).is_empty());
    let t0 = std::time::Instant::now();
    stop.stop();
    worker.join().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(text(&backups(&rig)[0], "status"), "Succeeded");
}
