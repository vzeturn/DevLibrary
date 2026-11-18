//! The module against a fake SMILE PMS cloud on a local socket: enroll, token, hello, an
//! op round trip, a reconnect after a close, terminal stops and `pms.reconnect`.

mod support;

use std::time::Duration;

use dl_agent::supervisor::ModuleState;
use dl_agent_pms::keystore::IDENTITY_FILE;
use dl_json::Value;
use support::{End, FakeCloud, FakeExec, Session, TestDir, metric, module, op_id, rig, section};

#[test]
fn enrolls_runs_ops_and_reconnects_after_a_close() {
    let dir = TestDir::new("e2e");
    let cloud = FakeCloud::start(
        "CODE-1",
        vec![
            Session::new(&[op_id(1)], End::Close(1011)),
            Session::new(&[op_id(2)], End::Hold),
        ],
    );
    let cfg = support::write_config(&dir, &cloud.base, Some("CODE-1"));
    let mut r = rig(module(FakeExec::new(Duration::ZERO)), section(&cfg));
    let st = r.tick_until(5000, |s| s.state == ModuleState::Running);
    assert_eq!(st.state, ModuleState::Running, "{st:?}");

    let seen = cloud.wait(10_000, |s| s.results.len() == 2);
    assert_eq!(
        seen.results,
        [
            (op_id(1), "applied".to_owned()),
            (op_id(2), "applied".to_owned())
        ]
    );
    assert_eq!(seen.enrolls, 1);
    assert!(seen.tokens >= 1);
    assert_eq!(seen.streams, 2);
    assert_eq!(seen.hellos.len(), 2);
    for h in &seen.hellos {
        assert!(h.starts_with("dl-agent-") && h.len() <= 32, "{h}");
    }
    assert!(seen.bad.is_empty(), "{:?}", seen.bad);

    // Enrolled for good: identity written, the one-time code scrubbed from the file.
    assert!(dir.path("state").join(IDENTITY_FILE).is_file());
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(!text.contains("enrollment_code"), "{text}");

    let st = r.tick_until(5000, |s| {
        s.state == ModuleState::Running
            && metric(s, "ops_ok") == Some(2)
            && metric(s, "reconnects") == Some(1)
    });
    assert_eq!(st.state, ModuleState::Running, "{st:?}");
    assert_eq!(st.detail, format!("connected to {}", cloud.base));
    assert_eq!(metric(&st, "ops_ok"), Some(2), "{:?}", st.metrics);
    assert_eq!(metric(&st, "reconnects"), Some(1), "{:?}", st.metrics);
    assert_eq!(metric(&st, "ops_inflight"), Some(0));
    assert!(metric(&st, "last_op_unix").unwrap_or(0) > 0);

    let status = r.command("pms.status");
    assert!(status.ok);
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v.get("connected").and_then(Value::as_bool), Some(true));
    assert_eq!(v.get("ops_ok").and_then(Value::as_u64), Some(2));
    // A close the agent can recover from is not a problem report.
    assert!(r.codes().is_empty(), "{:?}", r.codes());

    // pms.reconnect closes the stream normally and connects again.
    assert!(r.command("pms.reconnect").ok);
    let seen = cloud.wait(5000, |s| s.hellos.len() == 3);
    assert_eq!(seen.hellos.len(), 3);
    assert_eq!(seen.closes, [1000]);
    assert!(r.stop() < Duration::from_secs(3));
    assert_eq!(
        cloud.wait(2000, |s| s.closes.len() == 2).closes,
        [1000, 1000]
    );
}

#[test]
fn a_refused_code_parks_the_module_with_an_auth_report() {
    let dir = TestDir::new("refused");
    let cloud = FakeCloud::start("CODE-1", Vec::new());
    let cfg = support::write_config(&dir, &cloud.base, Some("WRONG"));
    let mut r = rig(module(FakeExec::new(Duration::ZERO)), section(&cfg));
    let st = r.tick_until(5000, |s| s.state == ModuleState::Degraded);
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert!(st.detail.starts_with("enrollment refused"), "{}", st.detail);
    assert!(st.detail.contains("pms-enroll --force"), "{}", st.detail);
    assert!(!st.detail.contains("WRONG"), "{}", st.detail);
    assert_eq!(r.codes(), ["pms_auth_failed"]);
    assert_eq!(cloud.seen().refused_enrolls, 1);
    assert!(!dir.path("state").join(IDENTITY_FILE).exists());
    assert_eq!(r.tick_until(0, |_| true).restarts, 0);
    assert!(r.stop() < Duration::from_secs(2));
}

#[test]
fn a_superseded_close_parks_until_pms_reconnect() {
    let dir = TestDir::new("superseded");
    let cloud = FakeCloud::start("CODE-1", vec![Session::new(&[], End::Close(4409))]);
    let cfg = support::write_config(&dir, &cloud.base, Some("CODE-1"));
    let mut r = rig(module(FakeExec::new(Duration::ZERO)), section(&cfg));
    let st = r.tick_until(5000, |s| {
        s.state == ModuleState::Degraded && s.detail.contains("pms.reconnect")
    });
    assert_eq!(st.state, ModuleState::Degraded, "{st:?}");
    assert!(st.detail.contains("SmilePmsAgent"), "{}", st.detail);
    assert_eq!(r.codes(), ["pms_stream_failed"]);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        cloud.seen().streams,
        1,
        "a terminal stop must not reconnect by itself"
    );

    assert!(r.command("pms.reconnect").ok);
    let st = r.tick_until(5000, |s| s.state == ModuleState::Running);
    assert_eq!(st.state, ModuleState::Running, "{st:?}");
    assert_eq!(cloud.seen().streams, 2);
    assert!(r.stop() < Duration::from_secs(3));
}
