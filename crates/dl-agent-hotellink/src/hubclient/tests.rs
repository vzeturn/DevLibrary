//! Port of Go `hubclient_fallback_test.go` plus the headers, the error kinds and the
//! request bodies.

use super::*;
use crate::testhub::{FakeHub, dead_url};

fn ok_hub() -> FakeHub {
    FakeHub::start(|r| {
        match r.path.as_str() {
        "/api/agent/config" => (
            200,
            r#"{"enabled":true,"availabilityIntervalMin":5,"availabilityDays":30,"policy":{"onCancel":{"allowWhenRoomAssigned":true}}}"#.into(),
        ),
        _ => (200, "{}".into()),
    }
    })
}

#[test]
fn falls_back_on_network_error_and_sticks() {
    let good = ok_hub();
    let hc = HubClient::new(&dead_url(), "h1", "tok", "dl-agent-1.0.0", "");
    hc.set_fallbacks(&[dead_url(), good.url.clone()]);
    for i in 0..3 {
        hc.get_config().unwrap_or_else(|e| panic!("call {i}: {e}"));
    }
    assert_eq!(good.hits("/").len(), 3);
    assert_eq!(hc.bases()[0], good.url, "the working base stays active");
}

#[test]
fn http_error_does_not_fall_back() {
    let primary = FakeHub::start(|_| (500, "boom".into()));
    let fb = ok_hub();
    let hc = HubClient::new(&primary.url, "h1", "tok", "v", "");
    hc.set_fallbacks(std::slice::from_ref(&fb.url));
    let e = hc.get_config().expect_err("HTTP 500");
    assert_eq!(e.kind(), Kind::Hub);
    assert_eq!(e.message(), "hub GET /api/agent/config: HTTP 500: boom");
    assert!(fb.hits("/").is_empty(), "fallback must not be called");
}

#[test]
fn all_down_returns_error() {
    let hc = HubClient::new(&dead_url(), "h1", "tok", "v", "");
    hc.set_fallbacks(&[dead_url()]);
    assert_eq!(hc.get_config().expect_err("down").kind(), Kind::Hub);
}

#[test]
fn status_kinds() {
    let hub = FakeHub::start(|r| match r.path.as_str() {
        "/api/agent/config" => (401, "bad token".into()),
        _ => (403, "superseded".into()),
    });
    let hc = HubClient::new(&hub.url, "h1", "tok", "v", "i");
    assert_eq!(hc.get_config().expect_err("401").kind(), Kind::Unauthorized);
    assert_eq!(hc.poll_jobs(0).expect_err("403").kind(), Kind::Superseded);
}

#[test]
fn headers_and_fallback_dedup() {
    let hub = ok_hub();
    let hc = HubClient::new(
        &format!("{}/", hub.url),
        "h1",
        "tok",
        "dl-agent-9.9.9",
        "inst-7",
    );
    hc.set_fallbacks(&[format!("{}/", hub.url), String::new(), " ".into()]);
    assert_eq!(hc.bases().len(), 1, "main URL and blanks are dropped");
    hc.get_config().expect("config");
    hc.set_mapping_hash("abc");
    hc.push_availability_disabled().expect("disabled");
    let reqs = hub.hits("/");
    let r = &reqs[0];
    assert_eq!(r.header("authorization"), Some("Bearer tok"));
    assert_eq!(r.header("x-hotel-id"), Some("h1"));
    assert_eq!(r.header("x-agent-version"), Some("dl-agent-9.9.9"));
    assert_eq!(r.header("x-instance-id"), Some("inst-7"));
    assert_eq!(r.header("x-mapping-hash"), None);
    let r = &reqs[1];
    assert_eq!(r.header("x-mapping-hash"), Some("abc"));
    assert_eq!(r.header("x-inventory-sync"), Some("0"));
    assert_eq!(r.body, r#"{"rows":[]}"#);

    let legacy = HubClient::new(&hub.url, "h1", "tok", "v", "");
    legacy.heartbeat(&Heartbeat::default()).expect("hb");
    let last = hub.hits("/api/agent/heartbeat").pop().expect("hb hit");
    assert_eq!(
        last.header("x-instance-id"),
        None,
        "legacy installs send none"
    );
}

#[test]
fn config_decodes_policy() {
    let hub = ok_hub();
    let hc = HubClient::new(&hub.url, "h1", "tok", "v", "");
    let c = hc.get_config().expect("config");
    assert!(c.enabled);
    assert_eq!((c.availability_interval_min, c.availability_days), (5, 30));
    let p = c.policy.expect("policy");
    assert!(p.on_cancel.allow_when_room_assigned);
}

#[test]
fn bodies_match_go() {
    let hub = ok_hub();
    let hc = HubClient::new(&hub.url, "h1", "tok", "dl-agent-1", "");
    hc.ack("J/1", true, "", &[], None).expect("ack");
    let res = PmsResult {
        action: "save".into(),
        completed: true,
        folios: vec![],
        synced_at: "2026-06-10T08:00:00Z".into(),
        ..PmsResult::default()
    };
    hc.ack("J2", false, "boom", &["e1".into()], Some(&res))
        .expect("ack");
    hc.ack_mapping_cmd("c1", true, "").expect("ack cmd");
    hc.push_mapping(&[], "h").expect("mapping");
    hc.heartbeat(&Heartbeat {
        pms_ok: true,
        breaker_state: "closed".into(),
        queue_depth: 2,
        loops_stalled: vec!["booking".into()],
        ..Heartbeat::default()
    })
    .expect("hb");
    let reqs = hub.hits("/");
    assert_eq!(reqs[0].path, "/api/agent/jobs/J%2F1/ack");
    assert_eq!(
        reqs[0].body,
        r#"{"error":"","ok":true,"recentErrors":null}"#
    );
    assert_eq!(
        reqs[1].body,
        r#"{"error":"boom","ok":false,"pmsResult":{"action":"save","completed":true,"folios":[],"syncedAt":"2026-06-10T08:00:00Z"},"recentErrors":["e1"]}"#
    );
    assert_eq!(reqs[2].path, "/api/agent/mapping-cmds/c1/ack");
    assert_eq!(reqs[2].body, r#"{"error":"","ok":true}"#);
    assert_eq!(reqs[3].body, r#"{"hash":"h","rows":[]}"#);
    assert_eq!(
        reqs[4].body,
        r#"{"version":"dl-agent-1","pmsOk":true,"breakerState":"closed","queueDepth":2,"recentErrors":null,"capabilities":["hub-policy","actions-v2"],"loopsStalled":["booking"]}"#
    );
}

#[test]
fn truncates_long_bodies() {
    let long = "é".repeat(400);
    let t = truncate(&long);
    assert!(t.ends_with('…') && t.len() <= 303, "{}", t.len());
    assert_eq!(truncate("short"), "short");
}
