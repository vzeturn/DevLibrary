//! The module under the real dl-agent supervisor: opt-in, the not-enrolled idle state,
//! the read-only guard, an enrolled run against a fake hub, the 401 gate, and a prompt stop.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::command::{Command, CommandResult, Outbox};
use dl_agent::config::RemoteConfig;
use dl_agent::report::Spool;
use dl_agent::supervisor::{Factory, Module, ModuleState, ModuleStatus, Supervisor};
use dl_agent_membership::config::{AgentConfig, ConfigStore, PlainSealer};
use dl_agent_membership::{MembershipModule, membership_module};
use dl_json::{Value, json};
use support::{FakeHub, HOTEL, TOKEN, TestDir};

fn plain_module() -> Factory {
    Arc::new(|| Box::new(MembershipModule::with_sealer(Arc::new(PlainSealer))) as Box<dyn Module>)
}

struct Rig {
    sup: Supervisor,
    outbox: Outbox,
    spool: Spool,
    cfg: RemoteConfig,
    _data: TestDir,
}

fn rig(factory: Factory, section: Value) -> Rig {
    let data = TestDir::new("data");
    let outbox = Outbox::default();
    let spool = Spool::in_memory();
    let mut sup = Supervisor::new(data.0.clone(), outbox.clone(), spool.clone());
    sup.add(factory);
    let cfg = RemoteConfig::from_json(&json!({
        "version": 2,
        "body": {"modules": {"membership": section}}
    }))
    .unwrap();
    Rig {
        sup,
        outbox,
        spool,
        cfg,
        _data: data,
    }
}

impl Rig {
    fn status(&self) -> ModuleStatus {
        self.sup.statuses()[0].clone()
    }

    fn tick_until(&mut self, ms: u64, done: impl Fn(&ModuleStatus) -> bool) -> ModuleStatus {
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            self.sup.tick(&self.cfg);
            let st = self.status();
            if done(&st) || Instant::now() >= end {
                return st;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn command(&mut self, kind: &str) -> CommandResult {
        let id = format!("c-{kind}");
        self.sup
            .deliver(Command {
                id: id.clone(),
                kind: kind.into(),
                module: Some("membership".into()),
                args: Value::Null,
                expires_at: u64::MAX,
            })
            .expect("module running");
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if let Some(r) = self.outbox.take().into_iter().find(|r| r.id == id) {
                return r;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no answer to {kind}");
    }

    fn codes(&self) -> Vec<String> {
        self.spool.peek(100).into_iter().map(|r| r.code).collect()
    }

    /// Stops the module and returns how long it took.
    fn stop(&mut self) -> Duration {
        let t = Instant::now();
        self.sup.stop_all(Duration::from_secs(10));
        let took = t.elapsed();
        assert_eq!(self.status().state, ModuleState::Stopped);
        took
    }
}

/// Writes a config for the fake hub with `smile` as the connection string.
fn enrolled(dir: &TestDir, hub: &FakeHub, smile: &str) {
    ConfigStore::new(dir.0.clone(), Arc::new(PlainSealer))
        .save(&AgentConfig {
            hub_base_url: hub.url(),
            agent_token: TOKEN.into(),
            hotel_id: HOTEL.parse().unwrap(),
            smile_connection_string: smile.into(),
            origin_code: "AZ".into(),
        })
        .unwrap();
}

#[test]
fn opt_in_disabled_without_a_section() {
    assert!(!MembershipModule::default().enabled_by_default());
    let mut r = rig(membership_module(), json!({}));
    r.cfg = RemoteConfig::default();
    let st = r.tick_until(200, |s| s.state == ModuleState::Disabled);
    assert_eq!(st.state, ModuleState::Disabled);
    let mut off = rig(membership_module(), json!({"enabled": false}));
    let st = off.tick_until(200, |s| s.state == ModuleState::Disabled);
    assert_eq!(st.state, ModuleState::Disabled);
}

#[test]
fn not_enrolled_idles_degraded_answers_status_and_stops_promptly() {
    let dir = TestDir::new("empty");
    let mut r = rig(
        plain_module(),
        json!({"enabled": true, "program_data_dir": dir.0.display().to_string()}),
    );
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert_eq!(
        st.detail,
        format!(
            "not enrolled: no config.dpapi in {}; run dl-agent membership-enroll",
            dir.0.display()
        )
    );
    let status = r.command("membership.status");
    assert!(status.ok);
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v.get("enrolled").and_then(Value::as_bool), Some(false));
    assert!(!r.command("membership.reboot").ok);
    assert!(r.codes().is_empty(), "not enrolled is not a problem report");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(r.tick_until(0, |_| true).restarts, 0);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn read_only_false_is_accepted_and_waits_for_enrolment_like_m1() {
    let dir = TestDir::new("rw");
    let mut r = rig(
        plain_module(),
        json!({"enabled": true, "read_only": false, "program_data_dir": dir.0.display().to_string()}),
    );
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("not enrolled"), "{}", st.detail);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn enrolled_with_smile_down_heartbeats_and_degrades_on_smile() {
    let hub = FakeHub::start();
    let dir = TestDir::new("smile-down");
    enrolled(&dir, &hub, "");
    let mut r = rig(
        plain_module(),
        json!({"enabled": true, "program_data_dir": dir.0.display().to_string()}),
    );
    r.tick_until(5000, |_| !hub.to("/api/agent/heartbeat").is_empty());
    assert!(!hub.to("/api/agent/heartbeat").is_empty(), "no heartbeat");
    let body = dl_json::parse(&hub.to("/api/agent/heartbeat")[0].body).unwrap();
    assert_eq!(body.get("SmileOk").and_then(Value::as_bool), Some(false));
    let st = r.tick_until(5000, |s| {
        s.state == ModuleState::Degraded && s.detail.starts_with("SMILE unreachable")
    });
    assert!(st.detail.starts_with("SMILE unreachable"), "{st:?}");
    let st = r.tick_until(3000, |s| s.metrics.get("smile.ok").is_some());
    assert_eq!(
        st.metrics.get("smile.ok").and_then(Value::as_bool),
        Some(false)
    );
    assert!(
        st.metrics.get("heartbeat.count").is_some(),
        "{:?}",
        st.metrics
    );
    let codes = r.codes();
    assert!(
        codes.iter().any(|c| c == "membership_smile_failed"),
        "{codes:?}"
    );
    for p in r.spool.peek(100) {
        assert!(!p.message.contains(TOKEN));
        assert!(!dl_json::to_string(&Value::Object(p.context)).contains(TOKEN));
    }
    let status = r.command("membership.status");
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v.get("enrolled").and_then(Value::as_bool), Some(true));
    assert!(!status.output.contains(TOKEN));
    // No scanner runs without SMILE, so nothing but heartbeat and sync-config reached the hub.
    assert!(
        hub.lines()
            .iter()
            .all(|l| l == "POST /api/agent/heartbeat" || l == "GET /api/agent/sync-config"),
        "{:?}",
        hub.lines()
    );
    assert!(r.stop() < Duration::from_secs(3));
}

#[test]
fn a_rejected_token_degrades_and_reports_auth() {
    let hub = FakeHub::start();
    hub.reply_with(
        "/api/agent/heartbeat",
        401,
        &[("X-MM-Agent-Auth-Error", "AGENT_TOKEN_REJECTED")],
        "",
    );
    let dir = TestDir::new("auth");
    enrolled(&dir, &hub, "");
    let mut r = rig(
        plain_module(),
        json!({"enabled": true, "program_data_dir": dir.0.display().to_string()}),
    );
    let st = r.tick_until(5000, |s| {
        s.detail.starts_with("hub rejected the agent token")
    });
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    let st = r.tick_until(3000, |_| false);
    assert_eq!(st.state, ModuleState::Degraded);
    let codes = r.codes();
    assert!(
        codes.iter().any(|c| c == "membership_auth_failed"),
        "{codes:?}"
    );
    assert_eq!(
        hub.to("/api/agent/heartbeat").len(),
        1,
        "the gate holds further calls"
    );
    assert!(r.stop() < Duration::from_secs(3));
}
