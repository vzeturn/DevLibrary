//! Port of ApiCommandQueueTests.cs against a std TcpListener API (tests/support).

mod support;

use std::sync::Arc;
use std::time::Duration;

use dl_agent_pms_device::agent::{TokenFile, add_duration};
use dl_agent_pms_device::config::AgentOptions;
use dl_agent_pms_device::contracts::{
    ClaimResponse, CommandOutcome, DeviceCommand, PendingResult, codes, kinds, parse_json,
};
use dl_agent_pms_device::queue::{ApiCommandQueue, CommandQueue, QueueError, StaleClaimReason};
use dl_json::Value;
use dl_time::OffsetDateTime;
use dl_uuid::Uuid;
use support::{FakeApi, ManualClock, temp_path};

const TOKEN: &str = "tok-fd01-0123456789abcdef0123456789abcdef";
/// The connection-refused code the OS reports (WSAECONNREFUSED on Windows; the errno
/// differs between Linux and the BSDs).
const ECONNREFUSED: i32 = if cfg!(windows) {
    10061
} else if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)) {
    61
} else {
    111
};
const CLAIM_RESPONSE: &str = include_str!("contract/claim.response.json");
const RESULT_REQUEST: &str = include_str!("contract/result.request.json");

struct Fixture {
    api: FakeApi,
    clock: Arc<ManualClock>,
    token_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let token_path = temp_path("agent-token");
        std::fs::write(&token_path, format!("{TOKEN}\n")).unwrap();
        Fixture {
            api: FakeApi::start(),
            clock: ManualClock::new(),
            token_path,
        }
    }

    fn queue(&self) -> ApiCommandQueue {
        let options = AgentOptions {
            api_base_url: format!("{}/api", self.api.base_url()),
            agent_id: "FD01".into(),
            token_file: self.token_path.to_string_lossy().into_owned(),
            ..AgentOptions::default()
        };
        let token = Arc::new(TokenFile::new(options.token_file.clone()));
        ApiCommandQueue::new(&options, token, self.clock.clone())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.token_path);
    }
}

fn json(text: &str) -> Value {
    parse_json(text.as_bytes()).unwrap()
}

fn claimed() -> DeviceCommand {
    let claimed_at = OffsetDateTime::parse_rfc3339("2026-10-14T07:00:00Z").unwrap();
    ClaimResponse::from_json(&json(CLAIM_RESPONSE))
        .unwrap()
        .unwrap()
        .to_command(claimed_at)
        .unwrap()
}

fn timed_out_result() -> PendingResult {
    let outcome = CommandOutcome::failed(codes::TIMEOUT, "Adapter did not answer within 30 s");
    PendingResult::from_command(&claimed(), &outcome)
}

fn lease_from(clock: &ManualClock, seconds: u64) -> OffsetDateTime {
    use dl_agent_pms_device::agent::Clock;
    add_duration(
        clock.now(),
        Duration::from_secs(seconds - ClaimResponse::LEASE_MARGIN_SECONDS as u64),
    )
}

#[test]
fn i0_h1_claim_sends_agent_headers_and_maps_200_to_a_command() {
    let f = Fixture::new();
    f.api.then(200, Some(CLAIM_RESPONSE));
    let cmd = f.queue().claim().unwrap().expect("a command");

    let sent = f.api.requests();
    assert_eq!(sent.len(), 1);
    let sent = &sent[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.target, "/api/device-agent/commands/claim");
    assert_eq!(
        sent.header("Authorization"),
        Some(format!("DeviceAgent {TOKEN}").as_str())
    );
    assert_eq!(sent.header("X-Device-Agent-Id"), Some("FD01"));
    // SFP-I1 review M1: the adapter name rides every poll; the default is "none".
    assert_eq!(sent.header("X-Device-Adapter"), Some("none"));
    // Like HttpClient: no User-Agent, an empty POST body has Content-Length 0.
    assert_eq!(sent.header("User-Agent"), None);
    assert_eq!(sent.header("Content-Length"), Some("0"));
    assert_eq!(
        cmd.id,
        Uuid::parse("6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11").unwrap()
    );
    assert_eq!(
        cmd.claim_token,
        Uuid::parse("0b8f6c1e-2c3d-4e5f-8a9b-1c2d3e4f5a6b").unwrap()
    );
    assert_eq!(cmd.kind, kinds::LOCK_ISSUE);
    assert_eq!(cmd.attempt, 1);
    // leaseSeconds 60 from the server, less the agent's safety margin, on the agent's clock.
    assert_eq!(cmd.lease_until, lease_from(&f.clock, 60));
}

#[test]
fn i0_h7_claim_response_with_an_unknown_member_is_accepted() {
    let f = Fixture::new();
    let mut body = json(CLAIM_RESPONSE);
    let map = body.as_object_mut().unwrap();
    map.insert("priority", "high");
    map.remove("leaseSeconds");
    f.api.then(200, Some(&dl_json::to_string(&body)));

    let cmd = f.queue().claim().unwrap().expect("a command");

    assert_eq!(
        cmd.id,
        Uuid::parse("6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11").unwrap()
    );
    assert_eq!(
        cmd.lease_until,
        lease_from(&f.clock, ClaimResponse::DEFAULT_LEASE_SECONDS as u64)
    );
}

#[test]
fn i0_h2_claim_maps_204_to_none() {
    let f = Fixture::new();
    f.api.then(204, None);
    assert_eq!(f.queue().claim().unwrap(), None);
}

#[test]
fn i0_h3_result_posts_the_contract_body() {
    let f = Fixture::new();
    f.api.then(200, Some("{}"));
    f.queue().send_result(&timed_out_result()).unwrap();

    let sent = f.api.requests();
    assert_eq!(sent.len(), 1);
    let sent = &sent[0];
    assert_eq!(
        sent.target,
        "/api/device-agent/commands/6f1c2b0e-8d1a-4c55-9a51-3f7d2b9e0a11/result"
    );
    assert_eq!(
        sent.header("Authorization"),
        Some(format!("DeviceAgent {TOKEN}").as_str())
    );
    assert_eq!(
        sent.header("Content-Type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(json(&sent.body), json(RESULT_REQUEST), "{}", sent.body);
}

#[test]
fn i0_h4_refused_result_is_a_stale_claim_with_the_reason() {
    let cases = [
        (
            409,
            "DEVICE_CLAIM_NOT_HELD: the claim on this command is no longer held",
            StaleClaimReason::NotHeld,
        ),
        (
            409,
            "DEVICE_OUTCOME_CONFLICT: the command already completed with a different outcome",
            StaleClaimReason::OutcomeConflict,
        ),
        (404, "Device command not found", StaleClaimReason::Missing),
    ];
    for (status, detail, reason) in cases {
        let f = Fixture::new();
        let body = dl_json::to_string(&dl_json::json!({
            "status": status, "code": "CONFLICT", "detail": detail
        }));
        f.api.then(status, Some(&body));
        match f.queue().send_result(&timed_out_result()) {
            Err(QueueError::StaleClaim(s)) => {
                assert_eq!(s.command_id, claimed().id);
                assert_eq!(s.reason, reason, "{detail}");
            }
            other => panic!("expected a stale claim, got {other:?}"),
        }
    }
}

#[test]
fn i0_h5_result_send_is_one_attempt_and_5xx_is_a_device_api_error() {
    let f = Fixture::new();
    f.api.then(503, None);
    match f.queue().send_result(&timed_out_result()) {
        Err(QueueError::Api(e)) => {
            assert_eq!(e.status, 503);
            assert_eq!(e.message, "Device agent API result returned 503: ");
        }
        other => panic!("expected an API error, got {other:?}"),
    }
    assert_eq!(f.api.requests().len(), 1);
}

#[test]
fn i0_h6_401_is_a_device_api_error_without_echoing_the_token() {
    let f = Fixture::new();
    f.api.then(
        401,
        Some(r#"{"title":"Unauthorized","detail":"Invalid device agent credentials"}"#),
    );
    match f.queue().claim() {
        Err(QueueError::Api(e)) => {
            assert_eq!(e.status, 401);
            assert_eq!(
                e.message,
                "Device agent API claim returned 401: Invalid device agent credentials"
            );
            assert!(!e.message.contains(TOKEN));
        }
        other => panic!("expected an API error, got {other:?}"),
    }
}

#[test]
fn detail_falls_back_to_title_message_and_raw_text_capped_at_300() {
    let f = Fixture::new();
    f.api
        .then(500, Some(r#"{"title":"Boom","message":"m"}"#))
        .then(502, Some(r#"{"detail":5,"message":"from message"}"#))
        .then(502, Some(&format!("  <html>{}</html>  ", "x".repeat(400))));
    let q = f.queue();
    let msg = |r: Result<Option<DeviceCommand>, QueueError>| match r {
        Err(QueueError::Api(e)) => e.message,
        other => panic!("{other:?}"),
    };
    assert_eq!(msg(q.claim()), "Device agent API claim returned 500: Boom");
    assert_eq!(
        msg(q.claim()),
        "Device agent API claim returned 502: from message"
    );
    let raw = msg(q.claim());
    assert_eq!(
        raw,
        format!(
            "Device agent API claim returned 502: <html>{}",
            "x".repeat(294)
        )
    );
}

#[test]
fn unreachable_api_missing_token_and_empty_claim_body() {
    // Nothing listens on the port any more: a transport error, retried by the reporter.
    let f = Fixture::new();
    let (q, dead_url) = {
        let dead = FakeApi::start();
        let options = AgentOptions {
            api_base_url: dead.base_url(),
            agent_id: "FD01".into(),
            ..AgentOptions::default()
        };
        let token = Arc::new(TokenFile::new(f.token_path.to_string_lossy().into_owned()));
        (
            ApiCommandQueue::new(&options, token, f.clock.clone()),
            dead.base_url(),
        )
    };
    let e = q.claim().unwrap_err();
    let QueueError::Transport(t) = &e else {
        panic!("{e:?}")
    };
    // SocketsHttpHandler: "<OS text> (host:port)", the SocketException inside.
    let authority = dead_url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    assert!(
        t.message.ends_with(&format!(" ({authority})")),
        "{}",
        t.message
    );
    assert!(!t.message.contains("os error"), "{}", t.message);
    let (code, socket_text) = t.socket.clone().expect("the SocketException");
    assert_eq!(code, ECONNREFUSED);
    assert_eq!(t.message, format!("{socket_text} ({authority})"));
    let dotnet = e.to_dotnet();
    assert_eq!(dotnet.type_name, "System.Net.Http.HttpRequestException");
    assert_eq!(
        dotnet.inner.as_ref().map(|i| i.type_name.as_str()),
        Some(format!("System.Net.Sockets.SocketException ({ECONNREFUSED})").as_str())
    );

    // A missing token file is not a transport error and names the path.
    let options = AgentOptions {
        api_base_url: f.api.base_url(),
        agent_id: "FD01".into(),
        ..AgentOptions::default()
    };
    let missing = temp_path("agent-token-missing");
    let token = Arc::new(TokenFile::new(missing.to_string_lossy().into_owned()));
    match ApiCommandQueue::new(&options, token, f.clock.clone()).claim() {
        Err(QueueError::Other(e)) => {
            assert_eq!(e.type_name, "System.InvalidOperationException");
            assert!(e.message.contains(&*missing.to_string_lossy()));
        }
        other => panic!("{other:?}"),
    }
    assert!(f.api.requests().is_empty());

    // 200 with `null` is DeviceApiException(200), with no body a JsonException.
    f.api.then(200, Some("null")).then(200, None);
    let q = f.queue();
    assert!(
        matches!(q.claim(), Err(QueueError::Api(e)) if e.status == 200
        && e.message == "Device agent API claim returned an empty body")
    );
    assert!(
        matches!(q.claim(), Err(QueueError::Other(e)) if e.type_name == "System.Text.Json.JsonException")
    );
}
