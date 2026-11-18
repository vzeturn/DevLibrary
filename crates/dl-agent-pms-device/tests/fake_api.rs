//! The module against a fake PMS API on a local socket: claim, run on the simulated lock,
//! post the result; the 204/404/409 answers; a result the API never takes; API failures;
//! and a print job reaching a raw-TCP (port 9100 protocol) printer.

mod module_support;
mod support;

use std::io::Read;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use dl_agent::supervisor::ModuleState;
use dl_agent_pms_device::{Hooks, pms_device_module_with};
use dl_json::Value;
use module_support::{Install, PmsApi, Rig, TOKEN, WAIT, counted, eventually, rig};

const ISSUE: &str =
    r#"{"room":"101","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":2}"#;

/// The module (no process environment, quick down/recheck) running against `install`.
fn running(install: &Install, down_after: Duration) -> Rig {
    let hooks = Hooks {
        env: Some(Vec::new()),
        down_after: Some(down_after),
        recheck: Some(Duration::from_millis(500)),
        ..Hooks::default()
    };
    rig(pms_device_module_with(hooks), install.section())
}

#[test]
fn a_claimed_lock_issue_runs_on_the_simulated_lock_and_its_result_is_posted() {
    let api = PmsApi::start();
    let install = Install::new("issue", &api.base_url(), "simulated-lock", true);
    let mut r = running(&install, Duration::from_secs(30));
    // 204 (nothing to claim) is a working round trip: the module runs.
    let st = r.until("running", |s| s.state == ModuleState::Running);
    assert!(st.detail.starts_with("polling http://127.0.0.1:"), "{st:?}");
    assert!(api.snapshot().results.is_empty());

    let id = api.push("lock.issue", ISSUE);
    let seen = api.wait("the result", |s| !s.results.is_empty());
    let (rid, status, body) = &seen.results[0];
    assert_eq!((rid.as_str(), *status), (id.to_string().as_str(), 200));
    assert_eq!(body["outcome"], "done");
    let cards = body["result"]["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0]["room"], "101");
    assert_eq!(seen.bad_auth, 0, "every request carries the agent token");
    eventually("an empty outbox", || install.outbox_files() == 0);

    let st = r.until("counted", |s| counted(s, "ok", "lock.issue") == 1);
    assert_eq!(counted(&st, "claimed", "lock.issue"), 1);
    assert_eq!(counted(&st, "failed", "lock.issue"), 0);
    assert!(st.metrics.get("last_command_unix").and_then(Value::as_u64) > Some(0));
    let status = r.command("pms-device.status");
    assert!(status.ok, "{}", status.output);
    let v = dl_json::parse(&status.output).unwrap();
    assert_eq!(v["claimed"]["lock.issue"], 1u64);
    assert!(r.codes().is_empty(), "{:?}", r.codes());
    assert!(r.stop() < Duration::from_secs(5));
}

#[test]
fn refused_results_404_and_409_are_dropped_not_resent() {
    let api = PmsApi::start();
    let install = Install::new("stale", &api.base_url(), "simulated-lock", true);
    let mut r = running(&install, Duration::from_secs(30));
    r.until("running", |s| s.state == ModuleState::Running);

    api.with(|s| s.result_status = 404);
    api.push("lock.issue", ISSUE);
    api.wait("the 404", |s| s.results.len() == 1);
    eventually("the 404 result dropped", || install.outbox_files() == 0);

    api.with(|s| {
        s.result_status = 409;
        s.result_detail = "DEVICE_CLAIM_NOT_HELD: the lease expired".into();
    });
    api.push("lock.issue", ISSUE);
    api.wait("the 409", |s| s.results.len() == 2);
    api.with(|s| s.result_detail = "DEVICE_OUTCOME_CONFLICT: already done".into());
    api.push("lock.issue", ISSUE);
    api.wait("the conflict", |s| s.results.len() == 3);
    eventually("the 409 results dropped", || install.outbox_files() == 0);

    // A few more polls: nothing is sent again.
    let claims = api.snapshot().claims;
    api.wait("more polls", |s| s.claims >= claims + 3);
    assert_eq!(api.snapshot().results.len(), 3);
    assert_eq!(r.status().state, ModuleState::Running);
    assert!(r.stop() < Duration::from_secs(5));
}

#[test]
fn a_result_the_api_never_takes_stays_in_the_outbox() {
    let api = PmsApi::start();
    api.with(|s| {
        s.result_status = 503;
        s.lease_seconds = 2;
    });
    let install = Install::new("owed", &api.base_url(), "simulated-lock", true);
    let mut r = running(&install, Duration::from_millis(300));
    r.until("running", |s| s.state == ModuleState::Running);
    let id = api.push("lock.issue", ISSUE);

    api.wait("a refused send", |s| !s.results.is_empty());
    eventually("the outbox entry", || install.outbox_files() == 1);
    // While the result is owed the agent claims nothing more and turns degraded.
    let st = r.until("degraded", |s| s.state == ModuleState::Degraded);
    assert!(st.detail.starts_with("API failing for "), "{st:?}");
    assert!(r.command("pms-device.status").ok);
    let flush = r.command("pms-device.flush_outbox");
    assert!(!flush.ok, "{}", flush.output);
    let v = dl_json::parse(&flush.output).unwrap();
    assert_eq!(v["pending"], 1u64);
    assert!(r.stop() < Duration::from_secs(5));
    assert_eq!(install.outbox_files(), 1, "the result survives the stop");
    let seen = api.snapshot();
    assert!(
        seen.results
            .iter()
            .all(|(rid, _, _)| *rid == id.to_string())
    );
    for rep in r.reports() {
        assert!(!rep.message.contains(TOKEN), "{rep:?}");
    }
}

#[test]
fn api_failures_degrade_the_module_and_are_reported_once_without_the_token() {
    let api = PmsApi::start();
    api.with(|s| s.claim_status = Some(503));
    let install = Install::new("down", &api.base_url(), "simulated-lock", true);
    let mut r = running(&install, Duration::from_millis(300));
    let st = r.until("degraded", |s| s.state == ModuleState::Degraded);
    assert!(st.detail.contains("API answered HTTP 503"), "{st:?}");
    eventually("the report", || !r.codes().is_empty());
    assert_eq!(r.codes(), ["pms_device_api_failed"]);

    api.with(|s| s.claim_status = None);
    r.until("running again", |s| s.state == ModuleState::Running);
    assert!(r.stop() < Duration::from_secs(5));

    api.with(|s| s.claim_status = Some(401));
    let mut r = running(&install, Duration::from_secs(30));
    let st = r.until("auth degraded", |s| s.state == ModuleState::Degraded);
    assert!(st.detail.contains("HTTP 401"), "{st:?}");
    eventually("the auth report", || !r.codes().is_empty());
    assert_eq!(r.codes(), ["pms_device_auth_failed"]);
    for rep in r.reports() {
        assert!(!rep.message.contains(TOKEN), "{rep:?}");
        assert!(!dl_json::to_string(&Value::Object(rep.context.clone())).contains(TOKEN));
    }
    assert!(r.stop() < Duration::from_secs(5));
}

#[test]
fn a_print_job_reaches_a_raw_tcp_printer_with_the_cut() {
    // The raw (JetDirect, port 9100) protocol on an ephemeral port: the job's bytes, then
    // the connection closes. Port 9100 itself may be taken on a build machine.
    let printer = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = printer.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = printer.accept() {
            stream.set_read_timeout(Some(WAIT)).unwrap();
            let mut got = Vec::new();
            let read = stream.read_to_end(&mut got).map(|_| got);
            let _ = tx.send(read.map_err(|e| e.to_string()));
        }
    });

    let api = PmsApi::start();
    let install = Install::new("print", &api.base_url(), "none", true);
    let mut r = running(&install, Duration::from_secs(30));
    r.until("running", |s| s.state == ModuleState::Running);
    let payload = format!(
        r#"{{"target":{{"transport":"tcp","endpoint":"{endpoint}"}},"data":"G0BIZWxsbwo=","cut":true,"copies":1}}"#
    );
    api.push("pos.print", &payload);

    let bytes = rx
        .recv_timeout(WAIT)
        .expect("the printer got a job")
        .unwrap();
    assert_eq!(bytes, b"\x1b@Hello\n\x1d\x56\x42\x00");
    let seen = api.wait("the print result", |s| !s.results.is_empty());
    let body = &seen.results[0].2;
    assert_eq!(body["outcome"], "done", "{}", dl_json::to_string(body));
    assert_eq!(body["result"]["bytes"], 12u64);
    assert_eq!(body["result"]["transport"], "tcp");
    assert!(r.stop() < Duration::from_secs(5));
}
