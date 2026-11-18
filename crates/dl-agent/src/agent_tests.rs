use std::sync::Arc;

use dl_ed25519::SigningKey;
use dl_hash::{Digest, Sha256};

use super::*;
use crate::hub::tests_support::{FakeHub, identity};
use crate::store::testdir::TestDir;
use crate::supervisor::{Module, ModuleContext};

const HB_OK: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": false}"#;

fn agent(d: &TestDir, hub: &FakeHub, modules: Vec<Factory>) -> Agent {
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

fn urls(hub: &FakeHub) -> Vec<String> {
    hub.seen().into_iter().map(|s| s.url).collect()
}

fn signed_manifest(version: &str, bytes: &[u8], seed: u8) -> String {
    let text = format!(
        r#"{{"product":"dl-agent","channel":"canary","version":"{version}","published_at":10,
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

fn wait_running(a: &mut Agent) {
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
    assert!(ok(4) && out(4).contains("\"agent_id\""));
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
    assert_eq!(p.probation_until, 1000 + PROBATION_SECONDS);
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

struct Broken;

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
    assert_eq!(a.update.failed, [a.opts.version.clone()]);
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
    });
    assert_eq!(a.boot().unwrap(), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"good");
    assert!(
        UpdateState::load(&a.opts.data_dir)
            .unwrap()
            .failed
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
