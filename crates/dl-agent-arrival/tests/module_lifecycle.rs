//! The module under the real dl-agent supervisor: opt-in, the not-enrolled and bad-config
//! states, commands, picking up an enrollment, a link to a fake cloud and a prompt stop.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::cloud::{FakeCloud, Reply};
use common::{TempDir, folios, new_tenant_key};
use dl_agent::command::{Command, CommandResult, Outbox};
use dl_agent::config::RemoteConfig;
use dl_agent::report::Spool;
use dl_agent::supervisor::{Module, ModuleState, ModuleStatus, Supervisor};
use dl_agent_arrival::agent::executor::FakeExecutor;
use dl_agent_arrival::agent::secrets::Identity;
use dl_agent_arrival::contracts::values::to_base64;
use dl_agent_arrival::module::{ArrivalModule, Hooks, NAME, arrival_module, arrival_module_with};
use dl_json::{Value, json};

struct Rig {
    sup: Supervisor,
    outbox: Outbox,
    spool: Spool,
    cfg: RemoteConfig,
    _data: TempDir,
}

fn rig(section: Value, hooks: Option<Hooks>) -> Rig {
    let data = TempDir::new("module-data");
    let outbox = Outbox::default();
    let spool = Spool::in_memory();
    let mut sup = Supervisor::new(data.path().to_path_buf(), outbox.clone(), spool.clone());
    sup.add(hooks.map_or_else(arrival_module, arrival_module_with));
    let cfg =
        RemoteConfig::from_json(&json!({"version": 2, "body": {"modules": {"arrival": section}}}))
            .expect("config");
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
                module: Some(NAME.into()),
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

    fn stop(&mut self) -> Duration {
        let t = Instant::now();
        self.sup.stop_all(Duration::from_secs(10));
        let took = t.elapsed();
        assert_eq!(self.status().state, ModuleState::Stopped);
        took
    }
}

fn identity() -> Identity {
    Identity {
        tenant_id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
        agent_id: "agt-module".into(),
        kid: "k-module".into(),
        secret: to_base64(&dl_rand::secure_bytes::<32>().expect("random")),
    }
}

/// What Install.ps1 leaves behind, pointing at `cloud_url`.
fn enroll(install: &Path, id: &Identity, cloud_url: &str) {
    let settings = json!({"Agent": {
        "CloudBaseUrl": cloud_url,
        "PmsConnectionString": "Server=127.0.0.1,1;Database=PMS;User ID=x;Password=y",
        "TenantKey": new_tenant_key(),
        "Watches": [],
    }});
    std::fs::write(
        install.join("appsettings.Production.json"),
        dl_json::to_string_pretty(&settings),
    )
    .expect("settings");
    id.save(&install.join("agent-identity.bin"))
        .expect("identity");
}

#[test]
fn opt_in_disabled_without_a_section() {
    assert!(!ArrivalModule::default().enabled_by_default());
    let mut r = rig(json!({}), None);
    r.cfg = RemoteConfig::default();
    assert_eq!(
        r.tick_until(200, |s| s.state == ModuleState::Disabled)
            .state,
        ModuleState::Disabled
    );
    let mut off = rig(json!({"enabled": false}), None);
    assert_eq!(
        off.tick_until(200, |s| s.state == ModuleState::Disabled)
            .state,
        ModuleState::Disabled
    );
}

#[test]
fn not_enrolled_idles_degraded_answers_commands_and_stops_promptly() {
    let install = TempDir::new("module-empty");
    let mut r = rig(
        json!({"enabled": true, "install_dir": install.path().display().to_string()}),
        None,
    );
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert!(st.detail.starts_with("not enrolled"), "{}", st.detail);
    let answer = r.command("arrival.status");
    assert!(!answer.ok);
    assert!(
        answer.output.contains("unknown command"),
        "{}",
        answer.output
    );
    assert!(
        r.spool.peek(100).is_empty(),
        "not being enrolled is a state, not a problem report"
    );
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn a_bad_section_is_degraded_until_the_config_changes() {
    let mut r = rig(
        json!({"enabled": true, "utc_offset_minutes": "seven"}),
        None,
    );
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("config invalid"), "{}", st.detail);
    assert_eq!(st.restarts, 0);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn picks_up_an_enrollment_links_up_and_stops_promptly() {
    let install = TempDir::new("module-install");
    let id = identity();
    let cloud = FakeCloud::start(&id, |_| Reply::Requests(Vec::new()));
    let pms = FakeExecutor::new(vec![Ok(vec![folios(&[(1, "A")])])]);
    let hooks = Hooks {
        executor: Some(pms),
        recheck: Some(Duration::from_millis(100)),
    };
    let section = json!({"enabled": true, "install_dir": install.path().display().to_string(), "utc_offset_minutes": 420});
    let mut r = rig(section, Some(hooks));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("not enrolled"), "{}", st.detail);

    enroll(install.path(), &id, &cloud.url);
    let st = r.tick_until(10_000, |s| s.state == ModuleState::Running);
    assert_eq!(st.state, ModuleState::Running, "{st:?}");
    let st = r.tick_until(3000, |s| s.metrics.contains_key("last_poll"));
    assert!(st.metrics.contains_key("requests_ok"), "{:?}", st.metrics);
    assert!(
        cloud
            .exchanges()
            .iter()
            .any(|e| e.body.get("hello").is_some_and(|h| !h.is_null()))
    );
    assert!(!r.command("arrival.status").ok);
    assert!(r.stop() < Duration::from_secs(4));
    let status =
        std::fs::read_to_string(install.path().join("link-status.json")).expect("link status");
    assert!(status.contains("\"state\": \"connected\""), "{status}");
}
