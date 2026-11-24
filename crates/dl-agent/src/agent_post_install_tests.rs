use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

use dl_json::Value;

use super::tests::agent;
use super::*;
use crate::hub::tests_support::FakeHub;
use crate::selftest::post_install;
use crate::store::testdir::TestDir;
use crate::sys_module::SelfTestHook;

const HB: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [], "update_check": false}"#;

fn hooked(d: &TestDir, hub: &FakeHub) -> (Agent, mpsc::Receiver<Value>) {
    let mut a = agent(d, hub, Vec::new());
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    a.opts.selftest = Some(SelfTestHook(Arc::new(move |args: &Value| {
        tx.lock().unwrap().send(args.clone()).unwrap();
        (true, String::new())
    })));
    (a, rx)
}

fn wait(rx: &mpsc::Receiver<Value>) -> Option<Value> {
    rx.recv_timeout(Duration::from_secs(3)).ok()
}

#[test]
fn first_heartbeat_of_a_new_version_runs_the_self_test_once() {
    let d = TestDir::new("agent-post-selftest");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, HB.into()));
    let (mut a, rx) = hooked(&d, &hub);
    assert_eq!(a.step(1000), None);
    let args = wait(&rx).expect("the self-test starts after the first accepted heartbeat");
    assert_eq!(args["trigger"], "update");
    // The next heartbeat of the same process does not run it again.
    assert_eq!(a.step(1060), None);
    assert!(wait(&rx).is_none());
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn a_version_already_reported_is_not_tested_again() {
    let d = TestDir::new("agent-post-selftest-done");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, HB.into()));
    let (mut a, rx) = hooked(&d, &hub);
    post_install::mark_done(&a.opts.data_dir, &a.opts.version.to_string());
    assert_eq!(a.step(1000), None);
    assert!(wait(&rx).is_none());
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn an_unreachable_hub_does_not_start_the_self_test() {
    let d = TestDir::new("agent-post-selftest-down");
    let hub = FakeHub::default();
    hub.push_down();
    let (mut a, rx) = hooked(&d, &hub);
    assert_eq!(a.step(1000), None);
    assert!(wait(&rx).is_none());
    a.supervisor.stop_all(STOP_WAIT);
}

#[test]
fn marker_follows_the_version() {
    let d = TestDir::new("post-selftest-marker");
    let dir = &d.0;
    std::fs::create_dir_all(dir).unwrap();
    use crate::selftest::Trigger;
    assert_eq!(post_install::due(dir, "1.3.44"), Some(Trigger::Update));
    post_install::mark_done(dir, "1.3.44");
    assert_eq!(post_install::due(dir, "1.3.44"), None);
    assert_eq!(post_install::due(dir, "1.3.45"), Some(Trigger::Update));
    assert!(post_install::is_post_install(Trigger::Update));
    assert!(!post_install::is_post_install(Trigger::Install));
    assert!(!post_install::is_post_install(Trigger::Command));
}
