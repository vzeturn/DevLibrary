//! The relay service's wire helpers (`PrintRelayService.cs`): the hub URL and the
//! `PrintJob` argument as the SignalR JSON protocol sends `PrintRelayJobMessage`.

mod support;

use dl_agent_pda_sync::config::AgentIdentity;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::printrelay::service::{connect_exception, hub_url, parse_job, start_hub};
use dl_agent_pda_sync::security::{AgentKey, names};
use dl_agent_pda_sync::transport::{EnrollmentClient, TokenProvider};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use support::{FakeSyncServer, TestEnv};

/// Enrolls against the fake cloud and returns a token provider over it.
fn enrolled_tokens(env: &TestEnv, server: &FakeSyncServer) -> TokenProvider {
    let (key, _) = AgentKey::load_or_create(env.secrets.as_ref()).unwrap();
    let key = Arc::new(key);
    let http = Arc::new(env.http_for(&server.base, key.clone()));
    let code = server.with(|s| s.valid_code.clone());
    let r = EnrollmentClient::new(&http, &key, &env.log)
        .redeem(&Ctx::background(), &code, "TEST-PC", "1.0.0-test")
        .unwrap();
    env.secrets
        .save(names::AGENT_TOKEN, r.agent_token.as_bytes())
        .unwrap();
    AgentIdentity {
        agent_id: r.agent_id,
        tenant_id: r.tenant_id,
        cloud_url: server.base.clone(),
        machine_id: "TEST-PC".into(),
        sql_instance: None,
        sql_database: None,
    }
    .save(&env.paths)
    .unwrap();
    TokenProvider::new(
        http,
        env.secrets.clone(),
        env.paths.clone(),
        env.time.clone(),
        &env.log,
    )
}

/// A hub that answers every WebSocket upgrade with `status`; records each request's
/// target and `Authorization` header.
fn refusing_hub(status: u16, requests: usize) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().take(requests) {
            let Ok(mut stream) = conn else { continue };
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            let _ = r.read_line(&mut request_line);
            let target = request_line.split(' ').nth(1).unwrap_or("").to_owned();
            let mut auth = String::new();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                    break;
                }
                if let Some((k, v)) = line.split_once(':')
                    && k.eq_ignore_ascii_case("authorization")
                {
                    auth = v.trim().to_owned();
                }
            }
            s.lock().unwrap().push(format!("{target} {auth}"));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (format!("http://127.0.0.1:{port}/"), seen)
}

#[test]
fn a_hub_that_refuses_the_token_keeps_it_cached_like_the_csharp_relay() {
    // C#: a 401 on the upgrade is a WebSocketException, not the HttpRequestException the
    // relay checks before `Tokens.Invalidate()`, so every retry reuses the cached token
    // (E1: Rust exchanged 10 tokens in 40 s, C# 3).
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    let tokens = enrolled_tokens(&env, &server);
    let (hub, seen) = refusing_hub(401, 3);
    for _ in 0..3 {
        let err = start_hub(&hub, &tokens, |_| {}, &Ctx::background()).unwrap_err();
        assert_eq!(err.type_name, "System.Net.WebSockets.WebSocketException");
        assert_eq!(
            err.message,
            "The server returned status code '401' when status code '101' was expected."
        );
    }
    assert_eq!(server.token_exchanges(), 1);
    // Header only, as the .NET client sends it (no access_token in the URL).
    assert_eq!(*seen.lock().unwrap(), ["/hubs/order Bearer jwt-1"; 3]);
}

#[test]
fn a_refused_upgrade_reads_like_the_dotnet_websocket_exception() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    let tokens = enrolled_tokens(&env, &server);
    let (hub, _) = refusing_hub(404, 1);
    let err = start_hub(&hub, &tokens, |_| {}, &Ctx::background()).unwrap_err();
    assert_eq!(err.type_name, "System.Net.WebSockets.WebSocketException");
    // The .NET 10 `ToString()` first line (stack frames aside).
    assert_eq!(
        err.to_dotnet_string(),
        "System.Net.WebSockets.WebSocketException (0x80004005): The server returned status code '404' when status code '101' was expected."
    );
}

#[test]
fn connect_failures_map_to_the_dotnet_client_exceptions() {
    let refused = dl_signalr::Error::from(dl_ws::Error::Http(dl_http::Error::Io(
        std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
    )));
    let ex = connect_exception(&refused);
    assert_eq!(ex.type_name, "System.Net.WebSockets.WebSocketException");
    assert_eq!(ex.message, "Unable to connect to the remote server");
    assert_eq!(
        ex.inner.as_ref().map(|i| i.type_name.as_str()),
        Some("System.Net.Http.HttpRequestException")
    );
    let handshake = dl_signalr::Error::Handshake("Requested protocol 'x' is not available.".into());
    let ex = connect_exception(&handshake);
    assert_eq!(ex.type_name, "Microsoft.AspNetCore.SignalR.HubException");
    assert_eq!(
        ex.message,
        "Unable to complete handshake with the server due to an error: Requested protocol 'x' is not available."
    );
    let hub = dl_signalr::Error::Hub {
        target: "JoinPrintRelay".into(),
        message: "denied".into(),
    };
    assert_eq!(connect_exception(&hub).message, "denied");
}

#[test]
fn the_hub_is_at_hubs_order_on_the_cloud_authority() {
    assert_eq!(
        hub_url("https://pda.example.com/"),
        "https://pda.example.com/hubs/order"
    );
    assert_eq!(
        hub_url("https://pda.example.com/api/v1?x=1"),
        "https://pda.example.com/hubs/order"
    );
    assert_eq!(
        hub_url("http://127.0.0.1:5000"),
        "http://127.0.0.1:5000/hubs/order"
    );
}

#[test]
fn a_print_job_reads_camel_case_ignoring_case() {
    let json = r#"{"jobId":"0f8fad5b-d9cb-469f-a165-70867728950e","rvcNo":3,"Host":"192.168.1.50","port":9100,"payload":"G0A=","expiresAt":"2026-11-15T08:05:00+00:00","attempts":2}"#;
    let v = dl_json::parse(json).unwrap();
    let job = parse_job(&[v]).unwrap();
    assert_eq!(
        job.job_id.to_string(),
        "0f8fad5b-d9cb-469f-a165-70867728950e"
    );
    assert_eq!(job.rvc_no, 3);
    assert_eq!(job.host, "192.168.1.50");
    assert_eq!(job.port, 9100);
    assert_eq!(job.payload, [0x1B, 0x40]);
    assert_eq!(
        job.expires_at.utc_ticks(),
        DateTimeOffset::parse("2026-11-15T08:05:00+00:00")
            .unwrap()
            .utc_ticks()
    );
    assert_eq!(job.attempts, 2);
}

#[test]
fn a_print_job_with_missing_members_takes_the_defaults_and_bad_base64_fails() {
    let v = dl_json::parse(r#"{"host":null}"#).unwrap();
    let job = parse_job(&[v]).unwrap();
    assert_eq!(job.job_id, dl_uuid::Uuid::NIL);
    assert!(job.host.is_empty() && job.payload.is_empty());
    assert_eq!(job.expires_at.utc_ticks(), DateTimeOffset::MIN.utc_ticks());
    let bad = dl_json::parse(r#"{"payload":"***"}"#).unwrap();
    assert!(parse_job(&[bad]).is_err());
    assert!(parse_job(&[]).is_err());
}
