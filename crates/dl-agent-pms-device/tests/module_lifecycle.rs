//! The module under the real dl-agent supervisor: opt-in, degraded without a token or with
//! invalid settings (and recovering on the re-check), and a prompt stop with a device call
//! in flight whose result waits in the outbox for the next start.

mod module_support;
mod support;

use std::time::Duration;

use dl_agent::config::RemoteConfig;
use dl_agent::supervisor::{Module, ModuleState};
use dl_agent_pms_device::{Hooks, PmsDeviceModule, pms_device_module, pms_device_module_with};
use dl_json::{Value, json};
use module_support::{GateAdapter, Install, PmsApi, TOKEN, eventually, rig};

const ISSUE: &str =
    r#"{"room":"101","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":1}"#;

fn quick_hooks() -> Hooks {
    Hooks {
        env: Some(Vec::new()),
        recheck: Some(Duration::from_millis(200)),
        ..Hooks::default()
    }
}

#[test]
fn opt_in_disabled_without_a_section_or_with_enabled_false() {
    assert!(!PmsDeviceModule::default().enabled_by_default());
    let mut r = rig(pms_device_module(), json!({}));
    r.cfg = RemoteConfig::default();
    r.until("disabled", |s| s.state == ModuleState::Disabled);
    let mut off = rig(pms_device_module(), json!({"enabled": false}));
    off.until("disabled", |s| s.state == ModuleState::Disabled);
    let mut bare = rig(pms_device_module(), json!({"appsettings_dir": "/nowhere"}));
    bare.until("disabled", |s| s.state == ModuleState::Disabled);
}

#[test]
fn no_token_is_degraded_answers_commands_and_starts_once_the_token_appears() {
    let api = PmsApi::start();
    let install = Install::new("notoken", &api.base_url(), "simulated-lock", false);
    let mut r = rig(pms_device_module_with(quick_hooks()), install.section());
    let st = r.until("degraded", |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("token not readable: "), "{st:?}");
    assert!(st.detail.contains("agent.token"), "{st:?}");

    let status = r.command("pms-device.status");
    assert!(status.ok, "{}", status.output);
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(
        v.get("detail").and_then(Value::as_str),
        Some(st.detail.as_str())
    );
    let flush = r.command("pms-device.flush_outbox");
    assert!(!flush.ok);
    assert!(
        flush.output.starts_with("the agent is not running: "),
        "{}",
        flush.output
    );
    assert!(!r.command("pms-device.reboot").ok);
    assert!(
        r.codes().is_empty(),
        "a missing token is a state, not a problem report"
    );
    assert_eq!(
        api.snapshot().claims,
        0,
        "nothing is claimed without a token"
    );

    install.write_token();
    let st = r.until("running", |s| s.state == ModuleState::Running);
    assert!(st.detail.starts_with("polling "), "{st:?}");
    assert_eq!(st.restarts, 0, "waiting is not a failure");
    assert!(r.stop() < Duration::from_secs(5));
}

#[test]
fn invalid_settings_are_degraded_with_the_validator_message_and_no_token() {
    let install = Install::new("invalid", "ftp://pms.example/api", "simulated-lock", true);
    let mut r = rig(pms_device_module_with(quick_hooks()), install.section());
    let st = r.until("degraded", |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("config invalid: "), "{st:?}");
    assert!(st.detail.contains("Agent:ApiBaseUrl"), "{st:?}");
    assert!(!st.detail.contains(TOKEN), "{st:?}");

    // The hub's overrides fix it without touching the install folder.
    let api = PmsApi::start();
    let mut section = install.section();
    if let Value::Object(m) = &mut section {
        m.insert("overrides", json!({"ApiBaseUrl": api.base_url()}));
    }
    let mut fixed = rig(pms_device_module_with(quick_hooks()), section);
    fixed.until("running", |s| s.state == ModuleState::Running);
    assert!(r.stop() < Duration::from_secs(5));
    assert!(fixed.stop() < Duration::from_secs(5));
}

#[test]
fn stop_leaves_a_blocked_device_call_behind_and_its_result_is_sent_on_the_next_start() {
    let api = PmsApi::start();
    // The API takes no result while the first agent runs.
    api.with(|s| s.result_status = 503);
    let install = Install::new("inflight", &api.base_url(), "simulated-lock", true);
    let gate = GateAdapter::new();
    let hooks = Hooks {
        adapter: Some(gate.clone()),
        stop_grace: Some(Duration::from_millis(500)),
        ..quick_hooks()
    };
    let mut r = rig(pms_device_module_with(hooks), install.section());
    r.until("running", |s| s.state == ModuleState::Running);
    let id = api.push("lock.issue", ISSUE);
    eventually("the device call", || gate.entered() == 1);

    // The device ignores the cancellation; the stop does not wait for it. 500 ms grace
    // plus one tick in practice; the bound leaves room for a loaded machine.
    let took = r.stop();
    assert!(took < Duration::from_secs(5), "stop took {took:?}");
    assert!(api.snapshot().results.is_empty());

    // The call finishes on its own thread: the result is saved, one send is refused.
    gate.release();
    api.wait("the refused send", |s| s.results.len() == 1);
    eventually("the outbox entry", || install.outbox_files() == 1);

    // The next start sends it from the outbox before it claims anything.
    api.with(|s| s.result_status = 200);
    let mut again = rig(pms_device_module_with(quick_hooks()), install.section());
    // Running means a claim went through, and the outbox is flushed before every claim.
    again.until("running", |s| s.state == ModuleState::Running);
    let seen = api.wait("the resent result", |s| s.results.len() == 2);
    let (rid, status, body) = &seen.results[1];
    assert_eq!((rid.as_str(), *status), (id.to_string().as_str(), 200));
    assert_eq!(body["outcome"], "done");
    assert_eq!(body["result"]["cards"][0]["cardNumber"], "GATE-1");
    eventually("an empty outbox", || install.outbox_files() == 0);
    assert!(again.stop() < Duration::from_secs(5));
}
