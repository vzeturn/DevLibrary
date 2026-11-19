//! The hub host list from heartbeat answers: adopted, saved, and used in an outage.

use super::super::tests::{agent, urls};
use crate::hub::tests_support::FakeHub;
use crate::identity::Identity;
use crate::store::testdir::TestDir;

const HB_LIST: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60,
    "commands": [], "update_check": false,
    "hub_urls": ["https://hub", "https://fallback.test/"]}"#;
const HB_OK: &str = r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60,
    "commands": [], "update_check": false}"#;

#[test]
fn a_hub_list_is_kept_and_used_when_the_primary_is_down() {
    let d = TestDir::new("agent-hub-list");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, HB_OK.into()));
    hub.push(200, HB_LIST);
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), None);
    let want = ["https://hub", "https://fallback.test"];
    assert_eq!(a.hub.hub_urls(), want);
    // Saved for the next start, the sealed key untouched.
    let saved = Identity::load(&a.opts.data_dir).unwrap();
    assert_eq!(saved.hub_urls, want);
    assert_eq!(saved.public_key(), a.hub.identity().public_key());
    assert_eq!(a.diagnostics(1000)["hub_urls"][1].as_str(), Some(want[1]));
    // The primary goes down: the next heartbeat reaches the fallback and stays there.
    hub.push_down();
    assert_eq!(a.step(1061), None);
    assert!(a.hub_ok);
    let seen = urls(&hub);
    let n = seen.len();
    assert_eq!(
        seen[n - 2..],
        [
            "https://hub/v1/heartbeat".to_string(),
            "https://fallback.test/v1/heartbeat".to_string()
        ]
    );
    assert_eq!(a.hub.current_url().as_deref(), Some(want[1]));
}

#[test]
fn an_unusable_list_changes_nothing() {
    let d = TestDir::new("agent-hub-list-bad");
    let hub = FakeHub::default();
    *hub.default.lock().unwrap() = Some((200, HB_OK.into()));
    hub.push(
        200,
        r#"{"server_time": 0, "channel": "canary", "heartbeat_seconds": 60, "commands": [],
        "hub_urls": ["http://plain.test", "https://u:p@x.test", 7]}"#,
    );
    let mut a = agent(&d, &hub, Vec::new());
    assert_eq!(a.step(1000), None);
    assert_eq!(a.hub.hub_urls(), ["https://hub"]);
    assert_eq!(
        Identity::load(&a.opts.data_dir).unwrap().hub_urls,
        ["https://hub"]
    );
}
