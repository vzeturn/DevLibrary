//! Update lanes (PROTOCOL.md §6.4): canary takes the fast lane, stable keeps the update-check
//! throttle, the site policy and the full probation. Rollback works the same on both.

use std::sync::Arc;

use super::tests::{Broken, agent, signed_manifest_on, urls};
use super::*;
use crate::hub::tests_support::FakeHub;
use crate::store::testdir::TestDir;
use crate::update::Pending;

fn hb(channel: &str, update_check: bool) -> String {
    format!(
        r#"{{"server_time": 0, "channel": "{channel}", "heartbeat_seconds": 15, "commands": [], "update_check": {update_check}}}"#
    )
}

fn release_fetches(hub: &FakeHub) -> usize {
    urls(hub)
        .iter()
        .filter(|u| u.contains("/v1/releases/"))
        .count()
}

#[test]
fn canary_installs_on_the_first_heartbeat_after_a_publish() {
    let d = TestDir::new("lane-canary");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let new = b"new binary";
    let mut a = agent(&d, &hub, Vec::new());
    // A test machine with automatic updates off and a night window (1000 s is 07:16 in
    // UTC+7): neither holds the canary channel.
    a.config.current.body = json!({"modules": {}, "update": {"auto": false, "window": {"from": "22:00", "to": "05:00"}}});
    hub.push(200, &hb("canary", false));
    assert_eq!(a.step(1000), None);
    assert_eq!(a.next_heartbeat, 1015);
    // An update check just happened: canary has no 300 s throttle, so a release published
    // now is installed on the next heartbeat (15 s) plus the download.
    a.last_update_check = 1000;
    hub.push(200, &hb("canary", true))
        .push(200, &signed_manifest_on("canary", "99.0.0", new, 7))
        .push(200, std::str::from_utf8(new).unwrap());
    assert_eq!(a.step(1015), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), new);
    let p = UpdateState::load(&a.opts.data_dir)
        .unwrap()
        .pending
        .unwrap();
    assert_eq!(p.probation_until, 1015 + CANARY_PROBATION_SECONDS);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn stable_keeps_the_update_check_throttle_and_the_full_probation() {
    let d = TestDir::new("lane-stable");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let new = b"new binary";
    let mut a = agent(&d, &hub, Vec::new());
    hub.push(200, &hb("stable", true))
        .push(404, r#"{"error":"no_release"}"#);
    assert_eq!(a.step(1000), None);
    assert_eq!(release_fetches(&hub), 1);
    hub.push(200, &hb("stable", true));
    assert_eq!(a.step(1015), None);
    assert_eq!(
        release_fetches(&hub),
        1,
        "one check per UPDATE_CHECK_SECONDS"
    );
    let at = 1000 + UPDATE_CHECK_SECONDS;
    hub.push(200, &hb("stable", true))
        .push(200, &signed_manifest_on("stable", "99.0.0", new, 7))
        .push(200, std::str::from_utf8(new).unwrap());
    assert_eq!(a.step(at), Some(Exit::Restart));
    let p = UpdateState::load(&a.opts.data_dir)
        .unwrap()
        .pending
        .unwrap();
    assert_eq!(p.probation_until, at + PROBATION_SECONDS);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn canary_probation_is_short_and_still_rolls_a_bad_build_back() {
    let d = TestDir::new("lane-rollback");
    let hub = FakeHub::default();
    let mut a = agent(&d, &hub, vec![Arc::new(|| Box::new(Broken))]);
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
    let t = now();
    let after_canary_probation = t + CANARY_PROBATION_SECONDS + 1;
    a.channel = "stable".into();
    assert_eq!(
        a.watch_probation(after_canary_probation),
        None,
        "stable gives a degraded module the full probation"
    );
    a.channel = "canary".into();
    assert_eq!(a.watch_probation(t + CANARY_PROBATION_SECONDS - 10), None);
    assert_eq!(
        a.watch_probation(after_canary_probation),
        Some(Exit::Restart)
    );
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), b"good");
    assert_eq!(a.update.failed_versions(), [a.opts.version.clone()]);
    assert!(
        a.spool()
            .peek(50)
            .iter()
            .any(|r| r.code == "update_rolled_back")
    );
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn the_no_update_file_stops_update_check_but_not_update_now() {
    let d = TestDir::new("lane-no-update");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let new = b"new binary";
    let mut a = agent(&d, &hub, Vec::new());
    std::fs::write(a.opts.data_dir.join(crate::optout::FILE), b"").unwrap();
    // The heartbeat says so, and an update_check is ignored.
    assert_eq!(
        a.heartbeat_body(1000)["update_blocked"],
        dl_json::Value::from("local")
    );
    hub.push(200, &hb("stable", true));
    assert_eq!(a.step(1000), None);
    assert_eq!(
        release_fetches(&hub),
        0,
        "no manifest fetch while opted out"
    );
    // update_now still installs.
    let cmd = r#"{"server_time": 0, "channel": "stable", "heartbeat_seconds": 60, "update_check": false,
        "commands": [{"id": "c1", "kind": "update_now", "args": {}, "expires_at": 99999}]}"#;
    hub.push(200, cmd)
        .push(200, &signed_manifest_on("stable", "99.0.0", new, 7))
        .push(200, std::str::from_utf8(new).unwrap());
    assert_eq!(a.step(1060), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), new);
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn without_the_file_nothing_is_reported_blocked() {
    let d = TestDir::new("lane-no-block");
    let hub = FakeHub::default();
    let a = agent(&d, &hub, Vec::new());
    assert!(a.heartbeat_body(1000).get("update_blocked").is_none());
}

#[test]
fn a_local_update_request_installs_on_the_channel_chosen_here_and_answers() {
    let d = TestDir::new("lane-local-update");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let new = b"new binary";
    let mut a = agent(&d, &hub, Vec::new());
    crate::optout::set_channel(&a.opts.data_dir, Some("canary")).unwrap();
    crate::optout::request_update(&a.opts.data_dir, 999).unwrap();
    hub.push(200, &signed_manifest_on("canary", "99.0.0", new, 7))
        .push(200, std::str::from_utf8(new).unwrap());
    assert_eq!(a.step(1000), Some(Exit::Restart));
    assert_eq!(std::fs::read(&a.opts.exe).unwrap(), new);
    assert_eq!(urls(&hub)[0], "https://hub/v1/releases/dl-agent/canary");
    let r = crate::optout::read_update_result(&a.opts.data_dir)
        .unwrap()
        .unwrap();
    assert!(r.ok && r.installed);
    assert_eq!((r.to.as_str(), r.channel.as_str()), ("99.0.0", "canary"));
    assert_eq!(r.from, a.opts.version.to_string());
    // The request was taken once.
    assert!(!crate::optout::take_update_request(&a.opts.data_dir).unwrap());
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn a_pin_holds_update_check_and_the_heartbeat_reports_it_and_the_channel_choice() {
    let d = TestDir::new("lane-pin");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, r#"{"accepted": 0}"#.into()));
    let mut a = agent(&d, &hub, Vec::new());
    let v: crate::Version = "1.2.3".parse().unwrap();
    crate::optout::set_pin(&a.opts.data_dir, Some(&v)).unwrap();
    crate::optout::set_channel(&a.opts.data_dir, Some("canary")).unwrap();
    let body = a.heartbeat_body(1000);
    assert_eq!(body["update_blocked"], dl_json::Value::from("pinned"));
    assert_eq!(body["update_pin"], dl_json::Value::from("1.2.3"));
    assert_eq!(body["channel_choice"], dl_json::Value::from("canary"));
    hub.push(200, &hb("stable", true));
    assert_eq!(a.step(1000), None);
    assert_eq!(release_fetches(&hub), 0, "no manifest fetch while pinned");
    // The channel chosen here wins over the hub's.
    assert_eq!(a.channel, "canary");
    a.supervisor.stop_all(STOP_WAIT);
}
