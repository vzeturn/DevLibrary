//! The module under the real dl-agent supervisor: opt-in, the not-enrolled idle state,
//! commands, and a prompt stop with an op in flight.

mod support;

use std::time::Duration;

use dl_agent::supervisor::{Module, ModuleState};
use dl_agent_pms::PmsSiteModule;
use dl_json::{Value, json};
use support::{End, FakeCloud, FakeExec, Session, TestDir, module, op_id, rig, section};

#[test]
fn opt_in_disabled_without_a_section() {
    assert!(!PmsSiteModule::default().enabled_by_default());
    let exec = FakeExec::new(Duration::ZERO);
    let mut r = rig(module(exec.clone()), json!({}));
    r.cfg = dl_agent::config::RemoteConfig::default();
    let st = r.tick_until(200, |s| s.state == ModuleState::Disabled);
    assert_eq!(st.state, ModuleState::Disabled);
    let mut off = rig(module(exec), json!({"enabled": false}));
    let st = off.tick_until(200, |s| s.state == ModuleState::Disabled);
    assert_eq!(st.state, ModuleState::Disabled);
}

#[test]
fn not_enrolled_idles_degraded_answers_commands_and_stops_promptly() {
    let dir = TestDir::new("empty");
    let cfg = dir.path("config.json");
    let mut r = rig(module(FakeExec::new(Duration::ZERO)), section(&cfg));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert_eq!(
        st.detail,
        format!("not enrolled: no config file at {}", cfg.display())
    );
    let status = r.command("pms.status");
    assert!(status.ok);
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v.get("connected").and_then(Value::as_bool), Some(false));
    assert_eq!(
        v.get("detail").and_then(Value::as_str),
        Some(st.detail.as_str())
    );
    let unknown = r.command("pms.reboot");
    assert!(!unknown.ok);
    assert!(r.codes().is_empty(), "not enrolled is not a problem report");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(r.tick_until(0, |_| true).restarts, 0);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn an_invalid_config_is_degraded_and_reported_without_its_values() {
    let dir = TestDir::new("invalid");
    let cfg = dir.path("config.json");
    std::fs::write(&cfg, r#"{"cloud_url":"ftp://secret-host.example"}"#).unwrap();
    let mut r = rig(module(FakeExec::new(Duration::ZERO)), section(&cfg));
    let st = r.tick_until(3000, |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("config invalid: "), "{}", st.detail);
    assert!(!st.detail.contains("secret-host"), "{}", st.detail);
    assert_eq!(r.codes(), ["pms_config_invalid"]);
    assert!(r.stop() < Duration::from_secs(2));
}

/// Starts an enrolled module against a cloud that sends one op and holds the stream.
fn enrolled_with_one_op(
    delay: Duration,
) -> (TestDir, FakeCloud, std::sync::Arc<FakeExec>, support::Rig) {
    let dir = TestDir::new("inflight");
    let cloud = FakeCloud::start("CODE-1", vec![Session::new(&[op_id(1)], End::Hold)]);
    let cfg = support::write_config(&dir, &cloud.base, Some("CODE-1"));
    let exec = FakeExec::new(delay);
    let mut r = rig(module(exec.clone()), section(&cfg));
    let st = r.tick_until(5000, |s| s.state == ModuleState::Running);
    assert_eq!(st.state, ModuleState::Running, "{st:?}");
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while exec.started() == 0 && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(exec.started(), 1, "the op never reached the executor");
    (dir, cloud, exec, r)
}

#[test]
fn stop_lets_an_op_in_flight_finish_then_closes_normally() {
    let (_dir, cloud, _exec, mut r) = enrolled_with_one_op(Duration::from_millis(1200));
    let took = r.stop();
    assert!(took < Duration::from_secs(5), "stop took {took:?}");
    let seen = cloud.wait(2000, |s| !s.closes.is_empty());
    assert_eq!(seen.results, [(op_id(1), "applied".to_owned())]);
    assert_eq!(seen.closes, [1000]);
    assert!(seen.bad.is_empty(), "{:?}", seen.bad);
}

#[test]
fn stop_cancels_a_hung_op_after_the_grace() {
    let (_dir, cloud, _exec, mut r) = enrolled_with_one_op(Duration::from_secs(60));
    let took = r.stop();
    assert!(
        took >= Duration::from_secs(4) && took < Duration::from_secs(9),
        "stop took {took:?}"
    );
    let seen = cloud.wait(2000, |s| !s.closes.is_empty());
    assert_eq!(seen.closes, [1000]);
    assert!(seen.bad.is_empty(), "{:?}", seen.bad);
}
