use super::*;
use crate::agent::PendingResultsError;
use crate::config::DotnetError;
use crate::contracts::DeviceError;
use crate::queue::DeviceApiError;
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;

#[derive(Default)]
struct List(Mutex<Vec<(Level, String, String, Map)>>);

impl Reporter for List {
    fn report(&self, level: Level, code: &str, message: &str, context: Map) {
        lock(&self.0).push((level, code.to_owned(), message.to_owned(), context));
    }
}

fn command(kind: &str) -> DeviceCommand {
    DeviceCommand {
        id: Uuid::NIL,
        kind: kind.to_owned(),
        payload: Value::Null,
        claim_token: Uuid::NIL,
        attempt: 1,
        lease_until: OffsetDateTime::now_utc(),
    }
}

fn failed(code: &str, message: &str) -> CommandOutcome {
    CommandOutcome {
        result: None,
        error: Some(DeviceError {
            code: code.to_owned(),
            message: message.to_owned(),
        }),
    }
}

fn tel() -> (Telemetry, Arc<List>) {
    let list = Arc::new(List::default());
    (Telemetry::new(Some(list.clone())), list)
}

#[test]
fn counts_by_kind_with_unknown_kinds_under_other() {
    let (t, _) = tel();
    for k in ["lock.issue", "lock.issue", "pos.print", "evil.kind"] {
        t.claimed(&command(k));
    }
    t.finished(&command("lock.issue"), &CommandOutcome::done(Value::Null));
    t.finished(&command("evil.kind"), &failed(codes::UNSUPPORTED_KIND, "x"));
    let m = t.to_map();
    assert_eq!(
        dl_json::to_string(m.get("claimed").unwrap()),
        r#"{"lock.issue":2,"other":1,"pos.print":1}"#
    );
    assert_eq!(
        dl_json::to_string(m.get("ok").unwrap()),
        r#"{"lock.issue":1}"#
    );
    assert_eq!(
        dl_json::to_string(m.get("failed").unwrap()),
        r#"{"other":1}"#
    );
    assert!(m.get("last_command_unix").and_then(Value::as_i64).unwrap() > 0);
    assert_eq!(t.claimed_total(), 4);
}

#[test]
fn adapter_and_sdk_failures_are_reported_without_device_text() {
    let (t, list) = tel();
    t.finished(
        &command("lock.read"),
        &failed(codes::ADAPTER_FAILED, "IOException: card 0412AB55 room 101"),
    );
    t.finished(
        &command("lock.issue"),
        &failed(
            codes::SDK_NOT_INSTALLED,
            "lock SDK not installed for HUNE2013",
        ),
    );
    t.finished(
        &command("pos.print"),
        &failed(codes::NOT_CONNECTED, "absent"),
    );
    let got = lock(&list.0);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].1, CODE_ADAPTER_FAILED);
    assert_eq!(got[0].2, "lock.read failed in the adapter (IOException)");
    assert!(!dl_json::to_string(&Value::Object(got[0].3.clone())).contains("0412AB55"));
    assert_eq!(got[1].1, CODE_SDK_MISSING);
    assert_eq!(got[1].0, Level::Warning);
    assert!(got[1].2.contains("HUNE2013"));
}

#[test]
fn reports_are_rate_limited_per_code_and_count_what_was_held_back() {
    let (t, list) = tel();
    for _ in 0..3 {
        t.report(Level::Error, CODE_API_FAILED, "down", Map::new());
    }
    t.report(Level::Error, CODE_AUTH_FAILED, "401", Map::new());
    assert_eq!(lock(&list.0).len(), 2);
    // Pretend the window passed: the next report says two were held back.
    if let Some((last, _)) = lock(&t.held).get_mut(CODE_API_FAILED) {
        *last -= REPORT_EVERY;
    }
    t.report(Level::Error, CODE_API_FAILED, "down", Map::new());
    let got = lock(&list.0);
    assert_eq!(got.len(), 3);
    assert_eq!(got[2].3.get("held_back").and_then(Value::as_u64), Some(2));
}

#[test]
fn health_goes_up_after_a_poll_and_down_after_the_grace_with_one_report() {
    let (t, list) = tel();
    assert_eq!(t.health(Duration::ZERO), Health::Pending);
    t.polled(Ok(false));
    assert_eq!(t.health(Duration::ZERO), Health::Up);
    let api = AgentError::Queue(QueueError::Api(DeviceApiError::new(
        503,
        "Device agent API claim returned 503: secret body",
    )));
    t.polled(Err(&api));
    // Within the grace the API still counts as up.
    assert_eq!(t.health(Duration::from_secs(3600)), Health::Up);
    let Health::Down { reason, .. } = t.health(Duration::ZERO) else {
        panic!("expected down");
    };
    assert_eq!(reason, "API answered HTTP 503");
    t.health(Duration::ZERO);
    assert_eq!(lock(&list.0).len(), 1);
    assert_eq!(lock(&list.0)[0].1, CODE_API_FAILED);
    t.polled(Ok(true));
    assert_eq!(t.health(Duration::ZERO), Health::Up);
}

#[test]
fn a_refused_token_is_down_at_once_as_auth() {
    let (t, list) = tel();
    let e = AgentError::Queue(QueueError::Api(DeviceApiError::new(401, "x")));
    t.polled(Err(&e));
    assert!(matches!(
        t.health(Duration::from_secs(3600)),
        Health::Down { .. }
    ));
    assert_eq!(lock(&list.0)[0].1, CODE_AUTH_FAILED);
    t.reset_api();
    assert_eq!(t.health(Duration::ZERO), Health::Pending);
}

#[test]
fn failures_are_described_without_secrets() {
    let pending = AgentError::PendingResults(PendingResultsError { pending: 2 });
    assert!(
        describe(&pending)
            .unwrap()
            .0
            .starts_with("2 device command result(s)")
    );
    assert_eq!(describe(&AgentError::Stopped), None);
    let other = AgentError::Queue(QueueError::Other(DotnetError::new(
        "System.InvalidOperationException",
        "Device agent token file '/x/agent.token' was not found",
    )));
    assert_eq!(
        describe(&other).unwrap().0,
        "Device agent token file '/x/agent.token' was not found"
    );
    assert_eq!(
        describe(&AgentError::Queue(QueueError::Timeout)).unwrap(),
        ("API did not answer within 15 s".to_owned(), false)
    );
}
