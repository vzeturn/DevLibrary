//! `backup-enroll` adopting the Go agent, against a fake hub that knows the Go agent's key:
//! adopt (no self-register), rejected key, another origin and an existing dl enrollment.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use dl_json::json;

use super::goagent::GoSource;
use super::goagent_tests::{
    FIXTURE_MACHINE, GO_AGENT_ID, GO_API_KEY, SEALED, source, write_config,
};
use super::tool::{KEY_ENV, Machine, run_args};
use super::*;
use crate::testutil::{Reply, Server, temp_dir};

const ENROLL_KEY: &str = "tenant-enroll-key";
const NEW_TOKEN: &str = "fresh-agent-token";

/// A hub where the Go agent's key answers `config_status` on `/config`.
struct Hub {
    server: Server,
    config_status: Arc<AtomicU16>,
}

impl Hub {
    fn start() -> Hub {
        let config_status = Arc::new(AtomicU16::new(200));
        let st = config_status.clone();
        let server = Server::start(move |r| match r.path() {
            "/api/agent/self-register" => {
                Reply::json(201, &json!({"agentId": "agent-new", "token": NEW_TOKEN}))
            }
            "/api/agent/config" => {
                if r.header("authorization") != Some(&format!("Bearer {GO_API_KEY}")) {
                    return Reply::new(401, "unknown agent");
                }
                let code = st.load(Ordering::SeqCst);
                if code == 200 {
                    Reply::json(200, &json!({"servers": [], "schedules": []}))
                } else {
                    Reply::new(code, "no")
                }
            }
            _ => Reply::new(404, "not found"),
        });
        Hub {
            server,
            config_status,
        }
    }

    fn url(&self) -> String {
        self.server.url()
    }

    fn count(&self, path: &str) -> usize {
        let full = format!("/api/agent{path}");
        self.server
            .requests()
            .iter()
            .filter(|r| r.path() == full)
            .count()
    }
}

fn run(dir: &Path, hub: &str, force: bool, go: &GoSource) -> Result<String, Error> {
    let env = |k: &str| (k == KEY_ENV).then(|| ENROLL_KEY.to_owned());
    let mut input = Cursor::new(Vec::new());
    let mut out = Vec::new();
    let machine = Machine {
        name: "SQL-HOST",
        go,
    };
    run_args(dir, hub, force, &env, &mut input, &mut out, &machine)?;
    Ok(String::from_utf8(out).unwrap())
}

fn assert_no_secret(text: &str) {
    for s in [GO_API_KEY, FIXTURE_MACHINE, ENROLL_KEY, NEW_TOKEN] {
        assert!(!text.contains(s), "{text}");
    }
}

#[test]
fn the_go_agent_is_adopted_without_self_register_and_its_file_is_untouched() {
    let hub = Hub::start();
    let cfg = write_config("adopt-ok", SEALED, &hub.url());
    let before = std::fs::read(&cfg).unwrap();
    let dir = temp_dir("adopt-ok-dl");

    let out = run(&dir, &hub.url(), false, &source(&[&cfg], FIXTURE_MACHINE)).unwrap();
    assert_eq!(
        out,
        format!("adopted agent {GO_AGENT_ID} from the Go agent\n")
    );
    assert_no_secret(&out);
    assert_eq!(hub.count("/self-register"), 0);
    assert_eq!(hub.count("/config"), 1);

    let e = Enrollment::load(&dir).unwrap().unwrap();
    assert_eq!(
        (e.hub_url.as_str(), e.agent_id.as_str(), e.token.as_str()),
        (hub.url().as_str(), GO_AGENT_ID, GO_API_KEY)
    );
    assert!(!e.hwid.is_empty());
    assert_eq!(
        std::fs::read(&cfg).unwrap(),
        before,
        "agent.json is never written"
    );
}

#[test]
fn a_rejected_key_falls_back_to_self_register() {
    let hub = Hub::start();
    hub.config_status.store(401, Ordering::SeqCst);
    let cfg = write_config("adopt-401", SEALED, &hub.url());
    let dir = temp_dir("adopt-401-dl");

    let out = run(&dir, &hub.url(), false, &source(&[&cfg], FIXTURE_MACHINE)).unwrap();
    assert!(
        out.starts_with("not adopting the Go agent: the hub rejected the Go agent"),
        "{out}"
    );
    assert!(out.ends_with("enrolled as agent agent-new\n"), "{out}");
    assert_no_secret(&out);
    assert_eq!(hub.count("/self-register"), 1);
    assert_eq!(Enrollment::load(&dir).unwrap().unwrap().token, NEW_TOKEN);
}

#[test]
fn a_go_agent_of_another_hub_falls_back_to_self_register() {
    let hub = Hub::start();
    let cfg = write_config("adopt-other", SEALED, "https://other-hub.example.test");
    let dir = temp_dir("adopt-other-dl");

    let out = run(&dir, &hub.url(), false, &source(&[&cfg], FIXTURE_MACHINE)).unwrap();
    assert!(out.contains("belongs to another hub"), "{out}");
    assert!(out.contains("other-hub.example.test"), "{out}");
    assert_no_secret(&out);
    assert_eq!(hub.count("/config"), 0, "its key never goes to another hub");
    assert_eq!(hub.count("/self-register"), 1);
}

#[test]
fn a_config_sealed_on_another_machine_falls_back_cleanly() {
    let hub = Hub::start();
    let cfg = write_config("adopt-wrong-id", SEALED, &hub.url());
    let dir = temp_dir("adopt-wrong-id-dl");

    let out = run(&dir, &hub.url(), false, &source(&[&cfg], "other-machine")).unwrap();
    assert!(
        out.contains("did not open") && out.contains("different machine"),
        "{out}"
    );
    assert_no_secret(&out);
    assert_eq!(hub.count("/config"), 0);
    assert_eq!(
        Enrollment::load(&dir).unwrap().unwrap().agent_id,
        "agent-new"
    );
}

#[test]
fn an_existing_dl_enrollment_is_untouched_without_force() {
    let hub = Hub::start();
    let cfg = write_config("adopt-existing", SEALED, &hub.url());
    let dir = temp_dir("adopt-existing-dl");
    let go = source(&[&cfg], FIXTURE_MACHINE);
    run(&dir, &hub.url(), false, &super::tests::no_go()).unwrap();
    let first = Enrollment::load(&dir).unwrap().unwrap();

    let out = run(&dir, &hub.url(), false, &go).unwrap();
    assert!(out.contains("already enrolled as agent agent-new"), "{out}");
    assert_eq!(hub.count("/config"), 0);
    assert_eq!(Enrollment::load(&dir).unwrap().unwrap(), first);

    // --force adopts and keeps the machine id the hub already saw from this install.
    let out = run(&dir, &hub.url(), true, &go).unwrap();
    assert!(out.starts_with("adopted agent "), "{out}");
    let again = Enrollment::load(&dir).unwrap().unwrap();
    assert_eq!(
        (again.agent_id.as_str(), again.hwid.as_str()),
        (GO_AGENT_ID, first.hwid.as_str())
    );
    assert_eq!(hub.count("/self-register"), 1);
}

#[test]
fn a_hub_failure_while_checking_changes_nothing() {
    let hub = Hub::start();
    hub.config_status.store(503, Ordering::SeqCst);
    let cfg = write_config("adopt-503", SEALED, &hub.url());
    let dir = temp_dir("adopt-503-dl");

    let err = run(&dir, &hub.url(), false, &source(&[&cfg], FIXTURE_MACHINE)).unwrap_err();
    assert!(err.to_string().contains("nothing changed"), "{err}");
    assert_no_secret(&err.to_string());
    assert_eq!(hub.count("/self-register"), 0, "no orphaning new agent");
    assert!(Enrollment::load(&dir).unwrap().is_none());
}
