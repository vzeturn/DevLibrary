//! The rollback trial end to end (PROTOCOL.md §6.6), as three process lives sharing one
//! install: the old build installs a `--fault probation` release, the fault build boots on
//! probation and is rolled back when probation ends, and the restored old build reports the
//! rollback to the hub. Only the service manager's restart between lives is not exercised
//! here (Windows SCM; see the crate README).

use dl_ed25519::SigningKey;

use super::tests::{agent, signed_manifest_on};
use super::*;
use crate::hub::tests_support::FakeHub;
use crate::identity::Identity;
use crate::store::testdir::TestDir;

const HB: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": false}"#;

/// The same install started again as `version`, with `fault` compiled in.
fn restart_as(d: &TestDir, hub: &FakeHub, version: &str, fault: Option<&'static str>) -> Agent {
    let mut opts = Options::new(d.0.join("data"), d.0.join("bin").join("dl-agent"));
    let key = SigningKey::from_seed(&[7; 32]);
    let pk = dl_encoding::base64::STANDARD.encode(key.verifying_key().as_bytes());
    opts.trusted = TrustedKeys::new().with_key("rel-1", &pk).unwrap();
    opts.version = version.parse().unwrap();
    opts.fault = fault;
    let id = Identity::load(&opts.data_dir).unwrap();
    Agent::new(opts, id, Box::new(hub.clone()), Vec::new()).unwrap()
}

fn reports_seen(hub: &FakeHub) -> Vec<Value> {
    hub.seen()
        .into_iter()
        .filter(|s| s.url.ends_with("/v1/reports"))
        .flat_map(|s| {
            let v = dl_json::parse_slice(&s.body).unwrap();
            v["reports"].as_array().cloned().unwrap_or_default()
        })
        .collect()
}

#[test]
fn a_fault_release_is_installed_rolled_back_at_the_end_of_probation_and_reported() {
    let d = TestDir::new("rollback-trial");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, HB.into()));
    let fault_build = b"dl-agent 99.0.0 (fault build: probation)";

    // Life 1: the old build takes the release from the canary channel and restarts.
    let mut old = agent(&d, &hub, Vec::new());
    let old_version = old.opts.version.to_string();
    old.opts.fault = None;
    assert_eq!(old.boot().unwrap(), None);
    hub.push(200, &HB.replace("false}", "true}"))
        .push(200, &signed_manifest_on("canary", "99.0.0", fault_build, 7))
        .push(200, std::str::from_utf8(fault_build).unwrap());
    assert_eq!(old.step(1000), Some(Exit::Restart));
    old.supervisor.stop_all(STOP_WAIT);
    drop(old);
    let exe = d.0.join("bin").join("dl-agent");
    assert_eq!(std::fs::read(&exe).unwrap(), fault_build);
    assert_eq!(
        std::fs::read(d.0.join("bin/previous/dl-agent")).unwrap(),
        b"old"
    );

    // Life 2: the fault build boots on probation, links to the hub, and is never committed.
    let mut new = restart_as(&d, &hub, "99.0.0", Some(crate::fault::PROBATION));
    assert_eq!(new.boot().unwrap(), None);
    let until = new.update.pending.as_ref().unwrap().probation_until;
    assert!(until > 1001, "{until}");
    assert_eq!(new.step(1001), None);
    assert!(new.hub_ok);
    assert_eq!(new.step(until - 1), None);
    assert!(new.update.pending.is_some(), "a fault build never commits");
    assert_eq!(new.step(until), Some(Exit::Restart));
    new.supervisor.stop_all(STOP_WAIT);
    drop(new);
    assert_eq!(
        std::fs::read(&exe).unwrap(),
        b"old",
        "previous binary restored"
    );

    // Life 3: the old build is back, forgets nothing and tells the hub.
    let mut back = restart_as(&d, &hub, &old_version, None);
    assert_eq!(back.boot().unwrap(), None);
    assert!(back.update.pending.is_none());
    assert_eq!(back.update.failed_versions(), ["99.0.0".parse().unwrap()]);
    let before = hub.seen().len();
    assert_eq!(back.step(until + 10), None);
    back.supervisor.stop_all(STOP_WAIT);
    let reports = reports_seen(&hub);
    let rolled = reports
        .iter()
        .find(|r| r["code"].as_str() == Some("update_rolled_back"))
        .unwrap_or_else(|| panic!("no rollback report in {reports:?}"));
    assert!(
        rolled["message"]
            .as_str()
            .unwrap()
            .contains("DL_AGENT_FAULT=probation"),
        "{rolled:?}"
    );
    let heartbeat = hub.seen()[before..]
        .iter()
        .find(|s| s.url.ends_with("/v1/heartbeat"))
        .map(|s| dl_json::parse_slice(&s.body).unwrap())
        .unwrap();
    assert_eq!(heartbeat["version"].as_str(), Some(old_version.as_str()));
    assert_eq!(heartbeat["update"]["state"].as_str(), Some("rolled_back"));
}
