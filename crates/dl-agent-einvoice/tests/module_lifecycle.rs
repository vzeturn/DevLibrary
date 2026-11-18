//! The module under the real dl-agent supervisor: opt-in, the not-enrolled idle state,
//! commands, a cycle against a fake platform on a local socket, and a prompt stop.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_agent::command::{Command, CommandResult, Outbox};
use dl_agent::config::RemoteConfig;
use dl_agent::report::Spool;
use dl_agent::supervisor::{ModuleState, ModuleStatus, Supervisor};
use dl_agent_einvoice::agent_config::AgentConfig;
use dl_agent_einvoice::{EinvoiceModule, einvoice_module};
use dl_json::{Value, json};

/// A fresh directory under the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> TestDir {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p =
            std::env::temp_dir().join(format!("dl-einvoice-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
    sup.add(einvoice_module());
    let cfg = RemoteConfig::from_json(&json!({
        "version": 2,
        "body": {"modules": {"einvoice": section}}
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
                module: Some("einvoice".into()),
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

#[test]
fn opt_in_disabled_without_a_section() {
    use dl_agent::supervisor::Module;
    assert!(!EinvoiceModule.enabled_by_default());
    let mut r = rig(json!({}));
    r.cfg = RemoteConfig::default();
    let st = r.tick_until(200, |s| s.state == ModuleState::Disabled);
    assert_eq!(st.state, ModuleState::Disabled);
    let mut off = rig(json!({"enabled": false}));
    let st = off.tick_until(200, |s| s.state == ModuleState::Disabled);
    assert_eq!(st.state, ModuleState::Disabled);
}

#[test]
fn not_enrolled_idles_degraded_answers_commands_and_stops_promptly() {
    let dir = TestDir::new("empty");
    let mut r = rig(json!({"enabled": true, "config_dir": dir.0.display().to_string()}));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert_eq!(
        st.detail,
        format!("not enrolled: no config.enc in {}", dir.0.display())
    );
    let status = r.command("einvoice.status");
    assert!(status.ok);
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v.get("enrolled").and_then(Value::as_bool), Some(false));
    let collect = r.command("einvoice.collect_now");
    assert!(!collect.ok);
    assert!(
        collect.output.starts_with("not enrolled"),
        "{}",
        collect.output
    );
    let unknown = r.command("einvoice.reboot");
    assert!(!unknown.ok);
    assert!(r.codes().is_empty(), "not enrolled is not a problem report");
    // Stays degraded, never restarted as a failure.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(r.tick_until(0, |_| true).restarts, 0);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn an_unreadable_config_is_reported_once() {
    let dir = TestDir::new("garbage");
    std::fs::write(dir.0.join("config.enc"), b"not a sealed config").unwrap();
    let mut r = rig(json!({"enabled": true, "config_dir": dir.0.display().to_string()}));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert!(
        st.detail.starts_with("config.enc unreadable: "),
        "{}",
        st.detail
    );
    assert_eq!(r.codes(), ["einvoice_config_unreadable"]);
    assert!(r.stop() < Duration::from_secs(2));
}

/// Every request the fake platform saw: (request line, body).
type Seen = Arc<Mutex<Vec<(String, String)>>>;

/// A fake platform: answers the agent API and records every request line and body.
fn fake_platform() -> (String, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let rec = seen.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut rd = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if rd.read_line(&mut line).is_err() {
                continue;
            }
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if rd.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; len];
            let _ = rd.read_exact(&mut body);
            let line = line.trim_end().to_owned();
            let (code, out) = if line.contains("/relay/next") {
                // The real hub holds the poll open; do not let the worker spin.
                std::thread::sleep(Duration::from_millis(100));
                (204, "")
            } else if line.starts_with("GET /api/v1/agent/config ") {
                (200, "{}")
            } else if line.starts_with("POST /api/v1/agent/poll ") {
                (200, r#"{"commands":[]}"#)
            } else {
                (204, "")
            };
            rec.lock()
                .unwrap()
                .push((line, String::from_utf8_lossy(&body).into_owned()));
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                out.len()
            );
        }
    });
    (base, seen)
}

#[test]
fn enrolled_runs_both_workers_reports_sql_failure_and_stops_promptly() {
    let (base, seen) = fake_platform();
    let dir = TestDir::new("enrolled");
    let cfg = AgentConfig::from_json(
        &dl_json::to_string(&json!({
            "platform_base_url": base,
            "api_key": "agent-key",
            "device_id": "6f9619ff-8b86-d011-b42d-00c04fc964ff",
            // Nothing listens on port 1: the SQL read fails at once.
            "sql_dsn": "sqlserver://einvoice@127.0.0.1:1?database=SMILE_FO",
            "pos_sql_dsn": "sqlserver://einvoice@127.0.0.1:1?database=SMILE_POS",
        })),
        &dir.0,
    )
    .unwrap();
    cfg.save().unwrap();
    let mut r = rig(json!({
        "enabled": true,
        "config_dir": dir.0.display().to_string(),
        "poll_interval_seconds": 3600,
    }));
    let st = r.tick_until(10_000, |s| {
        s.state == ModuleState::Degraded && !s.detail.starts_with("not enrolled")
    });
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert!(
        r.codes().contains(&"einvoice_sql_failed".to_owned()),
        "{:?}",
        r.codes()
    );
    let st = r.tick_until(3000, |s| {
        s.metrics.get("cycles").and_then(Value::as_u64) >= Some(1)
    });
    assert_eq!(st.metrics.get("cycles").and_then(Value::as_u64), Some(1));
    for k in [
        "outbox_pending",
        "pushed_total",
        "last_push_at",
        "relay_calls",
        "relay_failures",
    ] {
        assert!(
            st.metrics.get(k).is_some(),
            "metric {k} missing: {:?}",
            st.metrics
        );
    }

    let status = r.command("einvoice.status");
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v.get("enrolled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        v.get("detail").and_then(Value::as_str),
        Some(st.detail.as_str())
    );
    let collect = r.command("einvoice.collect_now");
    assert_eq!(
        (collect.ok, collect.output.as_str()),
        (true, "collect cycle queued")
    );
    let st = r.tick_until(5000, |s| {
        s.metrics.get("cycles").and_then(Value::as_u64) >= Some(2)
    });
    assert_eq!(st.metrics.get("cycles").and_then(Value::as_u64), Some(2));

    let took = r.stop();
    assert!(took < Duration::from_secs(3), "stop took {took:?}");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|(l, _)| l.starts_with("POST /api/v1/agent/relay/next "))
    );
    let beat = seen
        .iter()
        .find(|(l, _)| l.starts_with("POST /api/v1/agent/heartbeat "))
        .expect("a heartbeat");
    let hb = dl_json::parse(&beat.1).unwrap();
    let version = hb.get("agent_version").and_then(Value::as_str).unwrap();
    assert_eq!(version, format!("dl-agent-{}", dl_agent::agent_version()));
    assert!(
        r.sup.statuses()[0].restarts == 0,
        "a failing SQL server is not a module crash"
    );
}
