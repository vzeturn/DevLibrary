use std::sync::Arc;

use dl_ed25519::SigningKey;
use dl_hash::{Digest, Sha256};

use super::*;
use crate::hub::tests_support::{FakeHub, identity};
use crate::manifest::Manifest;
use crate::store::testdir::TestDir;
use crate::supervisor::{Module, ModuleContext};
use crate::update::Pending;

const HB_OK: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": false}"#;

pub(super) fn agent(d: &TestDir, hub: &FakeHub, modules: Vec<Factory>) -> Agent {
    let exe = d.0.join("bin").join("dl-agent");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"old").unwrap();
    let mut opts = Options::new(d.0.join("data"), exe);
    let key = SigningKey::from_seed(&[7; 32]);
    let pk = dl_encoding::base64::STANDARD.encode(key.verifying_key().as_bytes());
    opts.trusted = TrustedKeys::new().with_key("rel-1", &pk).unwrap();
    let id = identity(&["https://hub"]);
    id.save(&opts.data_dir).unwrap();
    Agent::new(opts, id, Box::new(hub.clone()), modules).unwrap()
}

pub(super) fn urls(hub: &FakeHub) -> Vec<String> {
    hub.seen().into_iter().map(|s| s.url).collect()
}

fn signed_manifest(version: &str, bytes: &[u8], seed: u8) -> String {
    signed_manifest_on("canary", version, bytes, seed)
}

pub(super) fn signed_manifest_on(channel: &str, version: &str, bytes: &[u8], seed: u8) -> String {
    let text = format!(
        r#"{{"product":"dl-agent","channel":"{channel}","version":"{version}","published_at":10,
        "rollout_percent":100,"min_version":null,"allow_downgrade":false,"notes":"",
        "artifacts":[{{"target":"{}","url":"https://hub/v1/artifacts/k1","sha256":"{}","size":{}}}],
        "key_id":"rel-1","signature":"{}"}}"#,
        current_target(),
        dl_encoding::hex::encode(Sha256::digest(bytes)),
        bytes.len(),
        dl_encoding::base64::STANDARD.encode([0u8; 64]),
    );
    let mut m = Manifest::parse(&text).unwrap();
    m.sign("rel-1", &SigningKey::from_seed(&[seed; 32]));
    m.to_json()
}

pub(super) fn wait_running(a: &mut Agent) {
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while !a.supervisor.all_running() && std::time::Instant::now() < end {
        a.supervisor.tick(&a.config.current);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(a.supervisor.all_running());
}

#[test]
fn heartbeat_applies_config_and_answers_commands() {
    let d = TestDir::new("agent-hb");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, "{}".into()));
    hub.push(
        200,
        r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 30,
        "config": {"version": 4, "body": {"log_level": "warn", "modules": {"sys": {"x": 1}}}},
        "commands": [
            {"id": "c1", "kind": "set_log_level", "args": {"level": "debug"}},
            {"id": "c2", "kind": "frobnicate"},
            {"id": "c3", "kind": "restart_module", "module": "sys", "expires_at": 1},
            {"id": "c4", "kind": "other.thing", "module": "nope"},
            {"id": "c5", "kind": "collect_diagnostics"}
        ], "update_check": false}"#,
    );
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), None);
    assert_eq!(a.channel, "canary");
    assert_eq!(a.next_heartbeat, 1030);
    assert_eq!(a.config().version, 4);
    assert!(a.config.trial_since.is_some());
    let seen = hub.seen();
    let hb = dl_json::parse_slice(&seen[0].body).unwrap();
    assert_eq!(hb.get("agent_id").and_then(Value::as_str), Some("agt_1"));
    assert_eq!(
        hb.get("update")
            .and_then(|u| u.get("state"))
            .and_then(Value::as_str),
        Some("idle")
    );
    let results: Vec<(String, Value)> = seen
        .iter()
        .filter(|s| s.url.contains("/v1/commands/"))
        .map(|s| (s.url.clone(), dl_json::parse_slice(&s.body).unwrap()))
        .collect();
    assert_eq!(results.len(), 5);
    let ok = |i: usize| results[i].1.get("ok").and_then(Value::as_bool).unwrap();
    let out = |i: usize| {
        results[i]
            .1
            .get("output")
            .and_then(Value::as_str)
            .unwrap()
            .to_string()
    };
    assert!(ok(0) && out(0) == "log level debug");
    assert!(!ok(1) && out(1).contains("unknown command"));
    assert!(!ok(2) && out(2) == "expired");
    assert!(!ok(3) && out(3).contains("nope is not running"));
    // The fake hub's `{}` is no upload answer: the command fails but carries the diagnostics.
    assert!(!ok(4) && out(4).contains("\"agent_id\"") && out(4).contains("upload_error"));
    // No step before the next heartbeat is due calls the hub.
    let n = hub.seen().len();
    assert_eq!(a.step(1029), None);
    assert_eq!(hub.seen().len(), n);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn unreachable_hub_is_spooled_and_retried() {
    let d = TestDir::new("agent-down");
    let hub = FakeHub::default();
    hub.push_down();
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), None);
    assert_eq!(a.next_heartbeat, 1000 + RETRY_SECONDS);
    assert!(
        a.spool()
            .peek(10)
            .iter()
            .any(|r| r.code == "hub_unreachable")
    );
    hub.push(200, HB_OK).push(200, r#"{"accepted": 1}"#);
    assert_eq!(a.step(1000 + RETRY_SECONDS), None);
    assert!(a.spool().is_empty(), "spool flushed once the hub answers");
    assert!(urls(&hub).iter().any(|u| u.ends_with("/v1/reports")));
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn revoked_agent_wipes_its_identity() {
    let d = TestDir::new("agent-revoked");
    let hub = FakeHub::default();
    hub.push(401, r#"{"error": "revoked"}"#);
    let mut a = agent(&d, &hub, Vec::new());
    assert!(Identity::exists(&d.0.join("data")));
    assert_eq!(a.step(1000), Some(Exit::Revoked));
    assert!(!Identity::exists(&d.0.join("data")));
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn signed_update_is_installed_and_put_on_probation() {
    let d = TestDir::new("agent-update");
    let hub = FakeHub::default();
    let new = b"new binary";
    hub.push(200, r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": true}"#)
        .push(200, &signed_manifest("99.0.0", new, 7))
        .push(200, std::str::from_utf8(new).unwrap());
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), new);
    assert_eq!(
        std::fs::read(d.0.join("bin/previous/dl-agent")).unwrap(),
        b"old"
    );
    let state = UpdateState::load(&a.opts.data_dir).unwrap();
    let p = state.pending.unwrap();
    assert_eq!(p.version.to_string(), "99.0.0");
    assert_eq!(p.probation_until, 1000 + CANARY_PROBATION_SECONDS);
    let u = urls(&hub);
    assert_eq!(u[1], "https://hub/v1/releases/dl-agent/canary");
    assert_eq!(u[2], "https://hub/v1/artifacts/k1");
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn badly_signed_release_is_refused_and_reported() {
    let d = TestDir::new("agent-badsig");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 1}"#.into()));
    hub.push(200, r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": true}"#)
        .push(200, &signed_manifest("99.0.0", b"evil", 8));
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), None);
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"old");
    let reports = hub
        .seen()
        .into_iter()
        .find(|s| s.url.ends_with("/v1/reports"))
        .expect("reports sent");
    assert!(String::from_utf8_lossy(&reports.body).contains("update_failed"));
    assert!(
        !urls(&hub).iter().any(|u| u.contains("/artifacts/")),
        "nothing downloaded"
    );
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn healthy_probation_commits() {
    let d = TestDir::new("agent-commit");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, Vec::new());
    a.update.pending = Some(Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 500,
        boots: 1,
        unhealthy_before: Vec::new(),
    });
    wait_running(&mut a);
    a.hub_ok = true;
    assert_eq!(a.watch_probation(1000), None);
    assert!(a.update.pending.is_none());
    assert!(
        UpdateState::load(&a.opts.data_dir)
            .unwrap()
            .pending
            .is_none()
    );
    assert_eq!(a.update_status.0, "idle");
    a.supervisor.stop_all(STOP_WAIT);
}

pub(super) struct Broken;

impl Module for Broken {
    fn name(&self) -> &str {
        "broken"
    }

    fn run(&mut self, _: &ModuleContext) -> Result<(), Error> {
        Err(Error::Module("cannot open the POS database".into()))
    }
}

#[test]
fn unhealthy_probation_rolls_back() {
    let d = TestDir::new("agent-rollback");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, vec![Arc::new(|| Box::new(Broken))]);
    a.opts.probation_seconds = 0;
    std::fs::create_dir_all(d.0.join("bin/previous")).unwrap();
    std::fs::write(d.0.join("bin/previous/dl-agent"), b"good").unwrap();
    a.update.pending = Some(Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 500,
        boots: 1,
        unhealthy_before: Vec::new(),
    });
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while a.supervisor.restart_counts().iter().all(|(_, n)| *n == 0)
        && std::time::Instant::now() < end
    {
        a.supervisor.tick(&a.config.current);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(a.watch_probation(now() + 1), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"good");
    assert_eq!(a.update.failed_versions(), [a.opts.version.clone()]);
    assert!(
        a.spool()
            .peek(50)
            .iter()
            .any(|r| r.code == "update_rolled_back")
    );
    assert_eq!(a.update_status.0, "rolled_back");
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn module_unhealthy_before_the_update_does_not_roll_it_back() {
    let d = TestDir::new("agent-baseline");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, vec![Arc::new(|| Box::new(Broken))]);
    a.opts.probation_seconds = 0;
    std::fs::create_dir_all(d.0.join("bin/previous")).unwrap();
    std::fs::write(d.0.join("bin/previous/dl-agent"), b"good").unwrap();
    let exe = std::fs::read(&a.opts.exe).unwrap();
    a.update.pending = Some(Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 500,
        boots: 1,
        unhealthy_before: vec!["broken".into()],
    });
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while a.supervisor.restart_counts().iter().all(|(_, n)| *n == 0)
        && std::time::Instant::now() < end
    {
        a.supervisor.tick(&a.config.current);
        std::thread::sleep(Duration::from_millis(10));
    }
    a.hub_ok = true;
    assert_eq!(a.watch_probation(now() + 1), None);
    assert!(
        a.update.pending.is_none(),
        "committed despite the known-bad module"
    );
    assert!(a.update.failed_versions().is_empty());
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), exe);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn boot_rolls_back_after_too_many_probation_starts() {
    let d = TestDir::new("agent-boot");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, Vec::new());
    std::fs::create_dir_all(d.0.join("bin/previous")).unwrap();
    std::fs::write(d.0.join("bin/previous/dl-agent"), b"good").unwrap();
    a.update.pending = Some(Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 0,
        boots: crate::update::MAX_PROBATION_BOOTS,
        unhealthy_before: Vec::new(),
    });
    assert_eq!(a.boot().unwrap(), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"good");
    assert!(
        UpdateState::load(&a.opts.data_dir)
            .unwrap()
            .failed_versions()
            .contains(&a.opts.version.clone())
    );
}

#[test]
fn config_that_breaks_a_module_is_reverted() {
    let d = TestDir::new("agent-revert");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, vec![Arc::new(|| Box::new(Broken))]);
    // `broken` is disabled in the known-good config, enabled by config 2.
    let good = RemoteConfig::from_json(
        &dl_json::json!({"version": 1, "body": {"modules": {"broken": {"enabled": false}}}}),
    )
    .unwrap();
    a.config.apply(good, 0).unwrap();
    a.config.confirm_if_due(1000).unwrap();
    wait_running(&mut a);
    let t = now();
    let bad = RemoteConfig::from_json(
        &dl_json::json!({"version": 2, "body": {"modules": {"broken": {"enabled": true}}}}),
    )
    .unwrap();
    a.apply_config(bad, t);
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while a.config.trial_since.is_some() && std::time::Instant::now() < end {
        a.supervisor.tick(&a.config.current);
        a.watch_config(t + 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(a.config.trial_since.is_none(), "reverted");
    assert_eq!(
        a.config().version,
        2,
        "keeps the bad version so the hub does not resend it"
    );
    assert!(!a.config().module_enabled("broken"));
    assert!(
        a.spool()
            .peek(50)
            .iter()
            .any(|r| r.code == "config_reverted")
    );
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn local_status_and_log_tail_work_offline() {
    let d = TestDir::new("agent-status");
    let hub = FakeHub::default();
    let a = agent(&d, &hub, Vec::new());
    let s = local_status(&a.opts.data_dir, &a.opts.product, &a.opts.version);
    assert_eq!(
        s.get("identity")
            .and_then(|i| i.get("agent_id"))
            .and_then(Value::as_str),
        Some("agt_1")
    );
    let logs = d.0.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("agent.log"), "a\nb\nc\n").unwrap();
    assert_eq!(log_tail(&logs, 2), ["b", "c"]);
    assert!(log_tail(&d.0.join("none"), 2).is_empty());
}

#[test]
fn product_agents_report_their_own_release_line() {
    let d = TestDir::new("agent-product");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"server_time":0,"channel":"stable","heartbeat_seconds":60,"commands":[],"update_check":false}"#.into()));
    let mut a = agent(&d, &hub, Vec::new());
    a.opts.product = "azolo-agent".into();
    a.opts.version = "3.1.0".parse().unwrap();
    assert_eq!(a.step(1000), None);
    let hb = dl_json::parse_slice(&hub.seen()[0].body).unwrap();
    assert_eq!(
        hb.get("product").and_then(Value::as_str),
        Some("azolo-agent")
    );
    assert_eq!(hb.get("version").and_then(Value::as_str), Some("3.1.0"));
    let s = local_status(&a.opts.data_dir, &a.opts.product, &a.opts.version);
    assert_eq!(
        s.get("product").and_then(Value::as_str),
        Some("azolo-agent")
    );
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn declared_databases_start_the_sql_module_and_a_bad_file_is_reported() {
    let d = TestDir::new("agent-sql");
    let hub = FakeHub::default();
    std::fs::create_dir_all(d.0.join("data")).unwrap();
    // Port 1 refuses at once: the module comes up degraded, the agent keeps running.
    let sql = r#"{"databases": {"SMILE_FO": {"connection": "Server=127.0.0.1,1;Database=SMILE_FO;User Id=u;Password=p;Encrypt=disable"}}}"#;
    std::fs::write(d.0.join("data").join(crate::sql::SQL_FILE), sql).unwrap();
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.supervisor.names(), ["sys", "sql"]);
    let end = std::time::Instant::now() + Duration::from_secs(5);
    let sql_state = |a: &Agent| {
        let st = a.supervisor.statuses();
        st.iter()
            .find(|s| s.name == "sql")
            .map(|s| s.state.as_str())
    };
    while sql_state(&a) != Some("degraded") && std::time::Instant::now() < end {
        a.supervisor.tick(&a.config.current);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(sql_state(&a), Some("degraded"));
    assert!(
        a.supervisor
            .statuses()
            .iter()
            .any(|s| s.detail.contains("SMILE_FO"))
    );
    a.supervisor.stop_all(Duration::from_secs(2));

    std::fs::write(
        d.0.join("data").join(crate::sql::SQL_FILE),
        "{\"databases\": {\"X\": {}}}",
    )
    .unwrap();
    let a = agent(&d, &hub, Vec::new());
    assert_eq!(a.supervisor.names(), ["sys"]);
    let codes: Vec<String> = a.spool().peek(10).into_iter().map(|r| r.code).collect();
    assert!(
        codes.contains(&"sql_config_invalid".to_string()),
        "{codes:?}"
    );
}

#[test]
fn heartbeat_carries_the_survey_only_when_it_changed_or_is_a_day_old() {
    let d = TestDir::new("agent-survey");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, vec![]);
    let bodies = |hub: &FakeHub| -> Vec<Value> {
        hub.seen()
            .into_iter()
            .filter(|s| s.url.ends_with("/heartbeat"))
            .map(|s| dl_json::parse_slice(&s.body).unwrap())
            .collect()
    };
    for t in [1000, 1060, 1000 + SURVEY_RESEND_SECONDS] {
        hub.push(200, HB_OK);
        assert!(a.heartbeat(t).is_none());
    }
    let sent: Vec<bool> = bodies(&hub)
        .iter()
        .map(|b| b.get("survey").is_some())
        .collect();
    assert_eq!(sent, [true, false, true]);
    let first = &bodies(&hub)[0]["survey"];
    assert_eq!(
        first["host"]["target"].as_str(),
        Some(current_target().as_str())
    );

    // A new database survey is sent at once; a failed heartbeat does not count as sent.
    a.sql
        .pools
        .set_survey("SMILE_FO", json!({"warnings": ["engine_pre_2012"]}));
    hub.push(503, "{}");
    let _ = a.heartbeat(1000 + SURVEY_RESEND_SECONDS + 60);
    hub.push(200, HB_OK);
    assert!(a.heartbeat(1000 + SURVEY_RESEND_SECONDS + 120).is_none());
    let b = bodies(&hub);
    assert!(b[4]["survey"]["databases"]["SMILE_FO"].is_object());
}

const HB_UPDATE: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": true}"#;

#[test]
fn update_window_holds_the_check_and_update_now_overrides_it() {
    let d = TestDir::new("agent-window");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let new = b"new binary";
    // The window holds stable agents; canary ones ignore it (agent_lane_tests.rs).
    hub.push(200, r#"{"server_time": 0, "channel": "stable", "heartbeat_seconds": 60, "commands": [], "update_check": true}"#)
        .push(200, &signed_manifest_on("stable", "99.0.0", new, 7));
    let mut a = agent(&d, &hub, Vec::new());
    // 1000 s after the epoch is 07:16 in UTC+7, outside 22:00–05:00.
    a.config.current.body =
        json!({"modules": {}, "update": {"window": {"from": "22:00", "to": "05:00"}}});
    assert_eq!(a.step(1000), None);
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"old");
    assert_eq!(a.update_status.0, "deferred");
    assert!(a.update_status.1.contains("22:00–05:00"));
    // The hub knows no `deferred` state (RT-0818): the heartbeat says idle and carries the reason as detail.
    let beat = a.heartbeat_body(1000);
    assert_eq!(beat["update"]["state"], "idle");
    assert_eq!(beat["update"]["version"], a.opts.version.to_string());
    assert!(
        beat["update"]["detail"]
            .as_str()
            .unwrap()
            .contains("22:00–05:00")
    );
    assert!(!urls(&hub).iter().any(|u| u.contains("/artifacts/")));

    hub.push(
        200,
        r#"{"server_time": 0, "channel": "stable", "heartbeat_seconds": 60, "update_check": false,
        "commands": [{"id": "u1", "kind": "update_now"}]}"#,
    )
    .push(200, &signed_manifest_on("stable", "99.0.0", new, 7))
    .push(200, std::str::from_utf8(new).unwrap());
    assert_eq!(a.step(1060), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), new);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn corrupt_downloads_count_towards_the_cap_and_retry_update_clears_it() {
    let d = TestDir::new("agent-cap");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let mut a = agent(&d, &hub, Vec::new());
    for i in 0..3 {
        hub.push(200, HB_UPDATE)
            .push(200, &signed_manifest("99.0.0", b"real", 7))
            .push(200, "stale cdn copy");
        let at = 1000 + i * 400;
        assert_eq!(a.step(at), None, "attempt {i}");
        a.next_heartbeat = 0;
    }
    assert_eq!(
        a.update.failures_of(&"99.0.0".parse().unwrap(), 2000),
        (3, 3)
    );
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"old");
    // The fourth check inside 24 h does not download at all.
    let downloads = |h: &FakeHub| urls(h).iter().filter(|u| u.contains("/artifacts/")).count();
    let before = downloads(&hub);
    hub.push(200, HB_UPDATE)
        .push(200, &signed_manifest("99.0.0", b"real", 7));
    assert_eq!(a.step(2400), None);
    assert_eq!(downloads(&hub), before);
    assert_eq!(a.update_status.0, "deferred");

    hub.push(
        200,
        r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "update_check": false,
        "commands": [{"id": "r1", "kind": "retry_update", "args": {"version": "99.0.0"}}]}"#,
    );
    a.next_heartbeat = 0;
    assert_eq!(a.step(2500), None);
    assert!(a.update.failures.is_empty());
    assert_eq!(a.update_status.0, "idle");
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn a_failed_version_that_runs_again_boots_idle_not_rolled_back() {
    let d = TestDir::new("agent-reboot-failed");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, Vec::new());
    let running = a.opts.version.clone();
    let other: crate::Version = "0.0.1".parse().unwrap();
    // A different failed version still reads as a rollback of that version.
    a.update.record_failure(&other, 10);
    a.update.save(&a.opts.data_dir).unwrap();
    drop(a);
    let a = agent(&d, &hub, Vec::new());
    assert_eq!(a.update_status, ("rolled_back", "0.0.1".to_string()));
    // The failed version is the one running now: nothing to report.
    let mut a = a;
    a.update.record_failure(&running, 20);
    a.update.save(&a.opts.data_dir).unwrap();
    drop(a);
    let a = agent(&d, &hub, Vec::new());
    assert_eq!(a.update_status, ("idle", running.to_string()));
}

#[test]
fn heartbeat_file_tracks_the_loop_and_the_hub_link() {
    let d = TestDir::new("agent-beat");
    let hub = FakeHub::default();
    hub.push_down();
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), None);
    let b = watchdog::read_beat(&a.opts.data_dir).unwrap();
    assert_eq!((b.at, b.hub_at, b.pid), (1000, 0, std::process::id()));
    // Rewritten once a minute, not every second.
    a.step(1030);
    assert_eq!(watchdog::read_beat(&a.opts.data_dir).unwrap().at, 1000);
    hub.push(200, HB_OK);
    a.next_heartbeat = 0;
    a.step(1031);
    let b = watchdog::read_beat(&a.opts.data_dir).unwrap();
    assert_eq!(b.hub_at, 1031, "a hub success is written at once");
    a.supervisor.stop_all(STOP_WAIT);
}

struct Linked;

impl Module for Linked {
    fn name(&self) -> &str {
        "linked"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        ctx.set_cloud_link("https://checkin.test/api/poll", 900);
        ctx.set_running("polling");
        while !ctx.stop().wait_timeout(Duration::from_millis(10)) {}
        Ok(())
    }
}

#[test]
fn heartbeat_file_carries_the_running_modules_cloud_links() {
    let d = TestDir::new("agent-beat-links");
    let hub = FakeHub::default();
    hub.push_down();
    let mut a = agent(&d, &hub, vec![Arc::new(|| Box::new(Linked))]);
    wait_running(&mut a);
    a.step(1000);
    let links = watchdog::read_beat(&a.opts.data_dir).unwrap().links;
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(
        (
            links[0].module.as_str(),
            links[0].url.as_str(),
            links[0].ok_at
        ),
        ("linked", "https://checkin.test", 900)
    );
    a.supervisor.stop_all(STOP_WAIT);
    a.step(2000);
    assert!(
        watchdog::read_beat(&a.opts.data_dir)
            .unwrap()
            .links
            .is_empty(),
        "a stopped module's link is not watched"
    );
}

#[test]
fn update_checks_resume_once_probation_commits() {
    let d = TestDir::new("agent-resume");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, Vec::new());
    a.update.pending = Some(Pending {
        version: a.opts.version.clone(),
        previous_version: "0.0.1".parse().unwrap(),
        probation_until: 500,
        boots: 1,
        unhealthy_before: Vec::new(),
    });
    assert!(matches!(
        a.try_update(400, false),
        Ok(update_flow::Checked::Held(_))
    ));
    assert!(urls(&hub).is_empty(), "no manifest fetch during probation");
    wait_running(&mut a);
    a.hub_ok = true;
    assert_eq!(a.watch_probation(1000), None);
    hub.push(404, r#"{"error":"no_release"}"#);
    assert!(matches!(
        a.try_update(1001, false),
        Ok(update_flow::Checked::Nothing(_))
    ));
    assert_eq!(urls(&hub), ["https://hub/v1/releases/dl-agent/stable"]);
    a.supervisor.stop_all(STOP_WAIT);
}
