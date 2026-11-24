//! The module against a fake SMILE PMS cloud on a local socket: enroll, token, hello (with
//! the ops this build runs), an op round trip, journal replays, a reconnect after a close,
//! retired and bad frames, terminal stops and `pms.reconnect`.

mod support;

use std::time::Duration;

use dl_agent::supervisor::ModuleState;
use dl_agent_pms::keystore::IDENTITY_FILE;
use dl_agent_pms::wire;
use dl_json::Value;
use support::{
    End, FakeCloud, FakeExec, Session, TestDir, custom_op, metric, module, op_id, rig, section,
};

const MIGRATE_FRAME: &str = include_str!("../testdata/agent-protocol/migrate.json");
const READ_FRAME: &str = include_str!("../testdata/agent-protocol/op-read.json");

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
            // The worker counts the op (ops_ok) before it ends (ops_inflight): wait for both.
            && metric(s, "ops_inflight") == Some(0)
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

/// The result frame for `id`.
fn result<'a>(frames: &'a [Value], id: &str) -> &'a Value {
    frames
        .iter()
        .find(|f| f.get("id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("no result for {id}: {frames:?}"))
}

fn err_code(v: &Value) -> Option<&str> {
    v.get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
}

#[test]
fn hello_names_the_ops_and_replays_are_answered_from_the_journal() {
    let dir = TestDir::new("replay");
    let keyed = custom_op(&op_id(1), &[("request_key", "rk-A".into())]);
    let replay_hit = custom_op(
        &op_id(2),
        &[("request_key", "rk-A".into()), ("replay", true.into())],
    );
    let replay_miss = custom_op(
        &op_id(3),
        &[("request_key", "rk-other".into()), ("replay", true.into())],
    );
    // A read carries `request_key: null` (the key stays, its value is null).
    let mut read = dl_json::parse(READ_FRAME).unwrap();
    if let Value::Object(o) = &mut read {
        o.insert("id", op_id(4));
    }
    let read = dl_json::to_string(&read);
    let mm_pms = custom_op(&op_id(5), &[("target_db", "MM_PMS".into())]);
    let cloud = FakeCloud::start(
        "CODE-1",
        vec![
            Session::frames(vec![keyed], End::Close(1011)),
            // A migrate frame from a cloud on the old contract is ignored, not fatal.
            Session::frames(
                vec![MIGRATE_FRAME.to_owned(), replay_hit, replay_miss, read],
                End::Hold,
            ),
            Session::frames(vec![mm_pms], End::Hold),
        ],
    );
    let cfg = support::write_config(&dir, &cloud.base, Some("CODE-1"));
    let exec = FakeExec::new(Duration::ZERO);
    let mut r = rig(module(exec.clone()), section(&cfg));
    let st = r.tick_until(5000, |s| s.state == ModuleState::Running);
    assert_eq!(st.state, ModuleState::Running, "{st:?}");
    let seen = cloud.wait(10_000, |s| s.results.len() == 4);
    assert!(seen.bad.is_empty(), "{:?}", seen.bad);
    assert_eq!(
        seen.results.len(),
        4,
        "closes {:?} results {:?}",
        seen.closes,
        seen.results
    );

    let hello = &seen.hello_frames[0];
    assert_eq!(hello.get("schema_version").and_then(Value::as_i64), Some(0));
    assert_eq!(hello.get("contract_max").and_then(Value::as_i64), Some(1));
    let ops: Vec<&str> = hello
        .get("ops")
        .and_then(Value::as_array)
        .expect("hello.ops")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        ops,
        [
            "fo.arrivals.expected@1",
            "fo.availability.chart@1",
            "fo.availability.contractStatus@1",
            "fo.availability.source@1",
            "fo.availability.sourceV2@1",
            "fo.clerk.login@1",
            "fo.clerk.rights.get@1",
            "fo.departures.expected@1",
            "fo.departures.onDate@1",
            "fo.hotelDate.get@1",
            "fo.inhouse.list@1",
            "fo.pos.post@1",
            "fo.reservation.list@1",
            "fo.reservation.listFigures@1",
            "pos.cashier.byInitial@1",
            "pos.cashier.login@1",
            "pos.check.close@1",
            "pos.check.detail@1",
            "pos.check.list@1",
            "pos.check.open@1",
            "pos.check.pay@1",
            "pos.check.postItem@1",
            "pos.check.send@1",
            "pos.clerk.permissions@1",
            "pos.menu.tree@1",
            "pos.outbox.claim@1",
            "pos.outbox.complete@1",
            "pos.outbox.source@1",
            "pos.outlets.list@1",
            "pos.shift.payments@1",
            "pos.tender.modes@1",
        ]
    );

    let f = &seen.result_frames;
    assert_eq!(f.len(), 4, "{f:?}");
    let first = result(f, &op_id(1));
    assert_eq!(first.get("status").and_then(Value::as_str), Some("applied"));
    assert_eq!(first.get("replayed"), None);
    let hit = result(f, &op_id(2));
    assert_eq!(hit.get("status").and_then(Value::as_str), Some("applied"));
    assert_eq!(hit.get("replayed").and_then(Value::as_bool), Some(true));
    let miss = result(f, &op_id(3));
    assert_eq!(miss.get("status").and_then(Value::as_str), Some("rejected"));
    assert_eq!(err_code(miss), Some(wire::ERR_REQUEST_KEY_UNKNOWN));
    let read = result(f, &op_id(4));
    assert_eq!(read.get("status").and_then(Value::as_str), Some("applied"));
    // Only the keyed write and the read ran; both replays were answered by the journal.
    assert_eq!(exec.started(), 2);

    let status = dl_json::parse(&r.command("pms.status").output).unwrap();
    let journal = status.get("journal").expect("journal");
    assert_eq!(journal.get("entries").and_then(Value::as_u64), Some(1));
    assert_eq!(journal.get("full").and_then(Value::as_bool), Some(false));
    let ops = status.get("ops").and_then(Value::as_array).expect("ops");
    assert_eq!(ops.len(), 31);

    // An op for a database outside SMILE_FO/POS/BO is a bad frame: the agent closes.
    assert!(r.command("pms.reconnect").ok);
    let seen = cloud.wait(5000, |s| s.closes.contains(&wire::CLOSE_BAD_FRAME));
    assert!(
        seen.closes.contains(&wire::CLOSE_BAD_FRAME),
        "{:?}",
        seen.closes
    );
    assert_eq!(seen.results.len(), 4);
    assert_eq!(exec.started(), 2);
    r.stop();
}
