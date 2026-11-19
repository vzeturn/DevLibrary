//! The sync loop against the fake hub: Go `syncloop` tests plus the enroll → config → job →
//! report path, master key rotation, `credentialError` and the outbox while the hub is down.

use dl_json::{Value, json};

use super::*;
use crate::enroll::{Enrollment, enroll};
use crate::executor::{Gate, execute};
use crate::fakes::SqlState;
use crate::hub::HubClient;
use crate::state::history::BackupRecord;
use crate::state::outbox::Outbox;
use crate::state::state_dir;
use crate::testhub::{
    FakeHub, PASSWORD, Rig, TOKEN, default_config, put, schedule, seal, seal_with, server, steps,
    text, tokens,
};
use crate::testutil::{dead_url, temp_dir};

fn of_type(reports: &[Value], kind: &str) -> Vec<Value> {
    reports
        .iter()
        .filter(|r| text(r, "type") == kind)
        .cloned()
        .collect()
}

fn messages(rig: &Rig, code: &str) -> Vec<String> {
    let problems = rig.rec.problems.lock().unwrap();
    problems
        .iter()
        .filter(|(c, _)| c == code)
        .map(|(_, m)| m.clone())
        .collect()
}

#[test]
fn enrolled_agent_polls_config_runs_the_job_and_reports_it() {
    let hub = FakeHub::start(default_config());
    let dir = temp_dir("sync-e2e");
    let module = dir.join("module");
    enroll(
        &module,
        &hub.url(),
        "tenant-enroll-key",
        "SQL-HOST",
        false,
        &crate::enroll::goagent::GoSource::none(),
    )
    .unwrap();
    let e = Enrollment::load(&module).unwrap().unwrap();
    assert_eq!(
        hub.bearers("/self-register"),
        vec!["Bearer tenant-enroll-key".to_owned()]
    );
    let rig = Rig::open(
        hub,
        HubClient::new(&e.hub_url, &e.token),
        dir,
        SqlState::default(),
    );

    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.bearers("/config"), vec![format!("Bearer {TOKEN}")]);
    assert_eq!(rig.hub.count("/master-key"), 1);
    let job = rig.next_job();
    assert_eq!(job.schedule_id, "sc1");
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);

    let backups = of_type(&rig.hub.reports(), "backup");
    assert_eq!(backups.len(), 1);
    let r = &backups[0];
    assert_eq!(
        (
            text(r, "status"),
            text(r, "historyId"),
            text(r, "scheduleId")
        ),
        ("Succeeded", job.id.as_str(), "sc1")
    );
    assert!(text(r, "cloudFilePath").starts_with("fake://s3/srbackup_Sales_"));
    assert_eq!(text(r, "checksum").len(), 64);
    let dump = dl_json::to_string(r);
    assert!(
        !dump.contains(PASSWORD) && !dump.contains("s3-secret"),
        "{dump}"
    );
    let target = rig
        .sql
        .state()
        .connects
        .iter()
        .find(|t| t.host == "sql1")
        .cloned()
        .unwrap();
    assert_eq!(
        (target.username.as_str(), target.password.as_str()),
        ("sa", PASSWORD)
    );

    assert_eq!(lock(&rig.core.queue).depth(), 0);
    assert!(lock(&rig.core.history).find(&job.id).is_some());
    assert!(lock(&rig.core.runs).last_run("sc1").is_some());
    assert_eq!(lock(&rig.core.stats).jobs_ok, 1);
    assert!(!rig.core.in_outbox(&format!("backup:{}", job.id)));

    // Not due again until the next cron time.
    sync.poll(now_ms()).unwrap();
    assert_eq!(lock(&rig.core.queue).depth(), 0);
    assert_eq!(rig.hub.count("/master-key"), 1, "the key is cached");
}

#[test]
fn a_rotated_master_key_is_fetched_again_once() {
    let rig = Rig::new("sync-rotate", default_config());
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.count("/master-key"), 1);

    let cmd = |id: &str, sealed: &str| {
        json!({"id": id, "type": "TestConnection", "serverId": "s1", "host": "sql1", "port": 1433,
            "encryptedPassword": sealed, "credentialError": false, "authType": "sql"})
    };
    rig.hub.set(|s| {
        s.key = "agent-master-key-2".into();
        put(
            &mut s.config,
            "/commands",
            json!([cmd("c1", &seal_with(PASSWORD, "agent-master-key-2"))]),
        );
    });
    sync.poll(now_ms()).unwrap();
    assert_eq!(
        rig.hub.count("/master-key"),
        2,
        "one re-fetch after the decrypt failure"
    );
    let cmds = of_type(&rig.hub.reports(), "command");
    assert_eq!(text(&cmds[0], "status"), "Succeeded");

    // The new key is cached: no more fetches for credentials sealed under it.
    rig.hub.set(|s| {
        put(
            &mut s.config,
            "/commands",
            json!([cmd("c2", &seal_with(PASSWORD, "agent-master-key-2"))]),
        );
    });
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.count("/master-key"), 2);

    // Sealed under a key the hub no longer has: one re-fetch, then a clear failure.
    rig.hub.set(|s| {
        put(
            &mut s.config,
            "/commands",
            json!([cmd("c3", &seal_with(PASSWORD, "some-other-key"))]),
        );
    });
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.count("/master-key"), 3);
    let cmds = of_type(&rig.hub.reports(), "command");
    let c3 = cmds.iter().find(|c| text(c, "commandId") == "c3").unwrap();
    assert_eq!(text(c3, "status"), "Failed");
    assert!(rig.codes().contains(&"backup_credential_error".to_owned()));
}

#[test]
fn credential_error_fails_that_work_with_a_reason_and_the_rest_runs() {
    let mut s1 = server("s1", "sql1", "d1", "Sales", "");
    put(&mut s1, "/credentialError", json!(true));
    let mut sc3 = schedule("sc3", "d2", "");
    put(&mut sc3, "/cloudDestination/credentialError", json!(true));
    let cfg = json!({
        "servers": [s1, server("s2", "sql2", "d2", "Stock", &seal(PASSWORD))],
        "schedules": [schedule("sc1", "d1", &tokens()), schedule("sc2", "d2", &tokens()), sc3],
        "restoreRequests": [{"id": "r1", "backupHistoryId": "h1", "targetServerHost": "sql2",
            "targetDatabase": "Stock_copy", "targetPasswordEncrypted": "", "credentialError": true}],
        "commands": [{"id": "c1", "type": "TestConnection", "serverId": "s1", "host": "sql1",
            "port": 1433, "encryptedPassword": "", "credentialError": true, "authType": "sql"}],
    });
    let rig = Rig::new("sync-credential", cfg);
    lock(&rig.core.history)
        .record(BackupRecord {
            schedule_id: "sc2".into(),
            hub_history_id: "h1".into(),
            database_name: "Stock".into(),
            local_file_path: String::new(),
            cloud_file_path: "fake://s3/h1.bak".into(),
            provider: "s3".into(),
            created_at: now_ms(),
        })
        .unwrap();
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();

    let pending = lock(&rig.core.queue).pending();
    assert_eq!(
        pending
            .iter()
            .map(|j| j.schedule_id.as_str())
            .collect::<Vec<_>>(),
        ["sc2"]
    );

    let reports = rig.hub.reports();
    let backups = of_type(&reports, "backup");
    let by_schedule = |id: &str| backups.iter().find(|r| text(r, "scheduleId") == id);
    for id in ["sc1", "sc3"] {
        let r = by_schedule(id).unwrap_or_else(|| panic!("no report for {id}"));
        assert_eq!(text(r, "status"), "Failed");
        let s = steps(r);
        assert_eq!((s[0].0.as_str(), s[0].1.as_str()), ("credentials", "error"));
        assert!(s[0].2.contains("enter it again in the hub"), "{}", s[0].2);
    }
    let restore = &of_type(&reports, "restore")[0];
    assert_eq!(text(restore, "status"), "Failed");
    assert!(text(restore, "error").contains("enter it again in the hub"));
    let command = &of_type(&reports, "command")[0];
    assert_eq!(text(command, "status"), "Failed");
    assert!(rig.codes().contains(&"backup_credential_error".to_owned()));
    assert_eq!(lock(&rig.core.stats).jobs_failed, 2);

    // The failed schedules count as run; the restore and command are not run again.
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.reports().len(), reports.len());
}

#[test]
fn reports_wait_in_the_outbox_while_the_hub_is_down() {
    let rig = Rig::new("sync-down", default_config());
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    let job = rig.next_job();
    rig.hub.set(|s| s.status = 503);
    execute(&rig.core, &Gate::default(), &StopSignal::new(), &job);
    let key = format!("backup:{}", job.id);
    assert!(rig.core.in_outbox(&key));
    assert!(rig.hub.reports().is_empty());
    let on_disk = Outbox::open(state_dir(&rig.dir.join("module")).join("outbox.enc")).unwrap();
    assert_eq!(on_disk.len(), 1, "the outbox survives a restart");

    // One failure only backs off; still failing after the back-off degrades.
    assert!(matches!(sync.poll(now_ms()), Err(Error::Hub(_))));
    assert_eq!(lock(&rig.core.health).problem, None);
    assert!(sync.poll(now_ms()).is_err());
    let problem = lock(&rig.core.health).problem.clone().unwrap();
    assert!(problem.starts_with("backup hub unreachable"), "{problem}");
    assert!(rig.codes().contains(&"backup_hub_failed".to_owned()));

    rig.hub.set(|s| s.status = 200);
    sync.poll(now_ms()).unwrap();
    assert!(!rig.core.in_outbox(&key));
    let backups = of_type(&rig.hub.reports(), "backup");
    assert_eq!(
        (text(&backups[0], "historyId"), text(&backups[0], "status")),
        (job.id.as_str(), "Succeeded")
    );
    assert_eq!(
        *lock(&rig.core.health),
        Health {
            polled: true,
            problem: None
        }
    );
}

#[test]
fn an_unreachable_hub_degrades_after_one_back_off() {
    let hub = FakeHub::start(default_config());
    let rig = Rig::open(
        hub,
        HubClient::new(&dead_url(), TOKEN),
        temp_dir("sync-dead"),
        SqlState::default(),
    );
    let mut sync = rig.sync();
    assert!(sync.poll(now_ms()).is_err());
    assert!(lock(&rig.core.health).problem.is_none());
    assert!(sync.poll(now_ms()).is_err());
    assert!(lock(&rig.core.health).problem.is_some());
    assert!(!lock(&rig.core.health).polled);
}

#[test]
fn a_rejected_agent_is_degraded_with_the_re_enroll_hint() {
    let rig = Rig::new("sync-auth", default_config());
    rig.hub.set(|s| s.status = 401);
    let mut sync = rig.sync();
    assert!(matches!(sync.poll(now_ms()), Err(Error::Auth(_))));
    let problem = lock(&rig.core.health).problem.clone().unwrap();
    assert!(problem.contains("backup-enroll --force"), "{problem}");
    assert_eq!(rig.codes(), ["backup_auth_failed"]);
}

#[test]
fn a_restore_runs_once_and_is_reported() {
    let mut cfg = default_config();
    put(
        &mut cfg,
        "/restoreRequests",
        json!([
            {"id": "r1", "backupHistoryId": "h1", "targetServerHost": "sql1", "targetDatabase": "Sales_copy",
                "targetPasswordEncrypted": seal("pw-restore"), "credentialError": false},
            {"id": "r2", "backupHistoryId": "h9", "targetServerHost": "sql1", "targetDatabase": "X",
                "targetPasswordEncrypted": seal("pw-restore"), "credentialError": false},
        ]),
    );
    let rig = Rig::new("sync-restore", cfg);
    let file = rig.dir.join("h1.bak");
    std::fs::write(&file, b"backup").unwrap();
    lock(&rig.core.history)
        .record(BackupRecord {
            schedule_id: "sc1".into(),
            hub_history_id: "h1".into(),
            database_name: "Sales".into(),
            local_file_path: file.display().to_string(),
            cloud_file_path: String::new(),
            provider: String::new(),
            created_at: now_ms(),
        })
        .unwrap();
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    sync.poll(now_ms()).unwrap();

    let restores = of_type(&rig.hub.reports(), "restore");
    assert_eq!(restores.len(), 2, "each request is reported once");
    let r1 = restores
        .iter()
        .find(|r| text(r, "requestId") == "r1")
        .unwrap();
    assert_eq!(text(r1, "status"), "Succeeded");
    let r2 = restores
        .iter()
        .find(|r| text(r, "requestId") == "r2")
        .unwrap();
    assert_eq!(text(r2, "status"), "Failed");
    assert!(text(r2, "error").contains("no local record of backup history h9"));
    let s = rig.sql.state();
    let runs: Vec<_> = s
        .statements
        .iter()
        .filter(|(q, _)| q.starts_with("RESTORE DATABASE"))
        .collect();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].0.contains("REPLACE"));
    assert!(
        s.connects
            .iter()
            .any(|t| t.password == "pw-restore" && t.port == 1433)
    );
}

#[test]
fn commands_are_run_once_with_their_results() {
    let mut cfg = default_config();
    let cmd = |id: &str, kind: &str| {
        json!({"id": id, "type": kind, "serverId": "s1", "host": "sql1", "port": 1433,
            "encryptedPassword": seal(PASSWORD), "credentialError": false, "authType": "sql"})
    };
    put(
        &mut cfg,
        "/commands",
        json!([
            cmd("c1", "TestConnection"),
            cmd("c2", "DiscoverDatabases"),
            cmd("c3", "Reboot")
        ]),
    );
    let rig = Rig::new("sync-commands", cfg);
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    sync.poll(now_ms()).unwrap();

    let cmds = of_type(&rig.hub.reports(), "command");
    assert_eq!(cmds.len(), 3);
    let get = |id: &str| cmds.iter().find(|c| text(c, "commandId") == id).unwrap();
    assert_eq!(text(get("c1"), "status"), "Succeeded");
    assert!(
        get("c1")
            .get("result")
            .and_then(|r| r.get("latencyMs"))
            .is_some()
    );
    assert_eq!(
        get("c2").get("result").and_then(|r| r.get("databases")),
        Some(&json!(["Sales"]))
    );
    assert_eq!(text(get("c3"), "status"), "Failed");
    let err = get("c3")
        .get("result")
        .map(|r| text(r, "error").to_owned())
        .unwrap();
    assert!(err.contains("unknown command type"), "{err}");
}

#[test]
fn discovery_runs_on_the_first_poll_then_every_tenth() {
    let rig = Rig::new("sync-discovery", default_config());
    let mut sync = rig.sync();
    for _ in 0..DISCOVERY_EVERY {
        sync.poll(now_ms()).unwrap();
    }
    let found = rig.hub.discovered();
    assert_eq!(found.len(), 1);
    assert_eq!(text(&found[0], "serverId"), "s1");
    assert_eq!(
        found[0].get("databases"),
        Some(&json!([{"databaseName": "Sales"}]))
    );
    sync.poll(now_ms()).unwrap();
    assert_eq!(rig.hub.discovered().len(), 2);
}

#[test]
fn retention_removes_expired_backups_and_keeps_failed_deletes() {
    let mut cfg = default_config();
    put(&mut cfg, "/schedules/0/retainCount", json!(1));
    let rig = Rig::new("sync-retention", cfg);
    let old_file = rig.dir.join("old.bak");
    std::fs::write(&old_file, b"x").unwrap();
    for (i, id) in ["h1", "h2", "h3"].iter().enumerate() {
        let local = if i == 0 {
            old_file.display().to_string()
        } else {
            String::new()
        };
        lock(&rig.core.history)
            .record(BackupRecord {
                schedule_id: "sc1".into(),
                hub_history_id: (*id).into(),
                database_name: "Sales".into(),
                local_file_path: local,
                cloud_file_path: format!("fake://s3/{id}.bak"),
                provider: "s3".into(),
                created_at: 1_000 * (i as u64 + 1),
            })
            .unwrap();
    }
    rig.dest.state().delete_error = Some(Error::Upload("bucket offline".into()));
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    assert_eq!(
        lock(&rig.core.history).for_schedule("sc1").len(),
        3,
        "kept for the next poll"
    );
    assert!(rig.codes().contains(&"backup_upload_failed".to_owned()));

    rig.dest.state().delete_error = None;
    sync.poll(now_ms()).unwrap();
    let left = lock(&rig.core.history).for_schedule("sc1");
    assert_eq!(
        left.iter()
            .map(|r| r.hub_history_id.as_str())
            .collect::<Vec<_>>(),
        ["h3"]
    );
    let mut deleted = rig.dest.state().deleted.clone();
    deleted.sort();
    assert_eq!(deleted, ["fake://s3/h1.bak", "fake://s3/h2.bak"]);
    assert!(!old_file.exists());
}

#[test]
fn run_now_queues_a_schedule_that_is_not_due() {
    let rig = Rig::new("sync-run-now", default_config());
    let mut sync = rig.sync();
    sync.poll(now_ms()).unwrap();
    let job = rig.next_job();
    lock(&rig.core.queue).complete(&job.id).unwrap();
    sync.poll(now_ms()).unwrap();
    assert_eq!(lock(&rig.core.queue).depth(), 0, "not due");

    lock(&rig.core.run_now).push("sc1".into());
    sync.poll(now_ms()).unwrap();
    assert_eq!(lock(&rig.core.queue).depth(), 1);
    assert!(lock(&rig.core.run_now).is_empty());
}

#[test]
fn a_broken_schedule_is_reported_and_the_others_run() {
    let mut cfg = default_config();
    let mut bad_cron = schedule("sc2", "d1", &tokens());
    put(&mut bad_cron, "/cronExpression", json!("not a cron"));
    put(
        &mut cfg,
        "/schedules",
        json!([
            bad_cron,
            schedule("sc3", "d404", &tokens()),
            schedule("sc1", "d1", &tokens())
        ]),
    );
    let rig = Rig::new("sync-broken", cfg);
    rig.sync().poll(now_ms()).unwrap();
    let pending = lock(&rig.core.queue).pending();
    assert_eq!(
        pending
            .iter()
            .map(|j| j.schedule_id.as_str())
            .collect::<Vec<_>>(),
        ["sc1"]
    );
    assert_eq!(
        rig.codes()
            .iter()
            .filter(|c| *c == "backup_schedule_invalid")
            .count(),
        2
    );
    assert!(
        messages(&rig, "backup_schedule_invalid")
            .iter()
            .any(|m| m.contains("d404"))
    );
    assert!(
        lock(&rig.core.runs).last_run("sc3").is_none(),
        "retried next poll"
    );
}

#[test]
fn next_interval_doubles_up_to_five_minutes() {
    let base = Duration::from_secs(60);
    assert_eq!(next_interval(base, base), Duration::from_secs(120));
    assert_eq!(next_interval(Duration::from_secs(240), base), MAX_INTERVAL);
    assert_eq!(next_interval(MAX_INTERVAL, base), MAX_INTERVAL);
    let slow = Duration::from_secs(600);
    assert_eq!(next_interval(slow, slow), slow);
}

#[test]
fn scrubbed_steps_keep_timings_and_drop_secrets() {
    let mut s = Steps::new();
    s.set(
        "connect",
        "error",
        "login failed: Server=x;Password=hunter2;",
        None,
    );
    let out = scrub_steps(s.list());
    let dump = dl_json::to_string(&Value::Array(out.clone()));
    assert!(!dump.contains("hunter2"), "{dump}");
    assert!(out[0].get("durationMs").is_some());
}

#[test]
fn new_ids_are_unique_uuids() {
    let (a, b) = (new_id().unwrap(), new_id().unwrap());
    assert_ne!(a, b);
    assert_eq!(a.len(), 36);
}
