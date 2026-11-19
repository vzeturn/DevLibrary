//! The module under the real dl-agent supervisor: opt-in, the not-configured idle state,
//! commands while idle, and a prompt stop. The configured paths (poll → running, 401 →
//! degraded, stop with a job in flight) run against a fake hub and repository in
//! `src/module/tests.rs`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dl_agent::command::{Command, CommandResult, Outbox};
use dl_agent::config::RemoteConfig;
use dl_agent::report::Spool;
use dl_agent::supervisor::{Module, ModuleState, ModuleStatus, Supervisor};
use dl_agent_hotellink::{HotellinkModule, hotellink_module};
use dl_json::{Value, json};

struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> TestDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("dl-hotellink-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).expect("temp dir");
        TestDir(p)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("leaving {}: {e}", self.0.display());
        }
    }
}

struct Rig {
    sup: Supervisor,
    outbox: Outbox,
    spool: Spool,
    cfg: RemoteConfig,
    _data: TestDir,
}

fn rig(section: Value) -> Rig {
    let data = TestDir::new("data");
    let outbox = Outbox::default();
    let spool = Spool::in_memory();
    let mut sup = Supervisor::new(data.0.clone(), outbox.clone(), spool.clone());
    sup.add(hotellink_module());
    let cfg = RemoteConfig::from_json(
        &json!({"version": 2, "body": {"modules": {"hotellink": section}}}),
    )
    .expect("remote config");
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
                module: Some("hotellink".into()),
                args: Value::Null,
                expires_at: u64::MAX,
            })
            .expect("module running");
        let end = Instant::now() + Duration::from_secs(3);
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

#[test]
fn opt_in_disabled_without_a_section() {
    assert!(!HotellinkModule.enabled_by_default());
    let mut r = rig(json!({}));
    r.cfg = RemoteConfig::default();
    assert_eq!(
        r.tick_until(200, |s| s.state == ModuleState::Disabled)
            .state,
        ModuleState::Disabled
    );
    let mut off = rig(json!({"enabled": false}));
    assert_eq!(
        off.tick_until(200, |s| s.state == ModuleState::Disabled)
            .state,
        ModuleState::Disabled
    );
}

#[test]
fn not_configured_idles_degraded_answers_commands_and_stops_promptly() {
    let dir = TestDir::new("empty");
    let path = dir.0.join("config.json");
    let mut r = rig(json!({"enabled": true, "config_path": path.display().to_string()}));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert_eq!(
        st.detail,
        format!("not configured: no config at {}", path.display())
    );
    let status = r.command("hl.status");
    assert!(status.ok);
    let v = dl_json::parse(&status.output).expect("status json");
    assert_eq!(v.get("configured").and_then(Value::as_bool), Some(false));
    let sync = r.command("hotellink.sync-mapping");
    assert!(!sync.ok);
    assert!(sync.output.starts_with("not configured"), "{}", sync.output);
    assert!(!r.command("hl.reboot").ok);
    assert!(
        r.spool.peek(100).is_empty(),
        "not configured is not a problem report"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(r.tick_until(0, |_| true).restarts, 0);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn config_without_token_is_not_configured() {
    let dir = TestDir::new("notoken");
    let path = dir.0.join("config.json");
    std::fs::write(
        &path,
        r#"{"hubUrl":"https://hl.example.vn","hotelId":"h1","token":"","sql":{"server":".","database":"SMILE_FO","trustedConnection":true}}"#,
    )
    .expect("write config");
    let mut r = rig(json!({"enabled": true, "config_path": path.display().to_string()}));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert!(
        st.detail.starts_with("not configured: config has no token"),
        "{}",
        st.detail
    );
    assert!(r.stop() < Duration::from_secs(2));
}
