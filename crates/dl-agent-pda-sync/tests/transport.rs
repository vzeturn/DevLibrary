//! Transport and enrollment against the fake cloud (ports of the parts of
//! `TransportTests.cs` and `EnrollmentTests.cs` that need no outbox or SQL bootstrap).

mod support;

use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::time::Clock;
use dl_agent_pda_sync::config::{AgentIdentity, AgentStatus};
use dl_agent_pda_sync::security::{AgentKey, names};
use dl_agent_pda_sync::transport::{EnrollmentClient, RequestSigner, TokenProvider};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::sync::Arc;
use std::time::Duration;
use support::{FakeSyncServer, TestEnv};

/// `TestEnv.EnrollAsync` without the SQL bootstrap: key, redeem, then what the workflow
/// persists (identity, token, `enrolled` status).
fn enroll(
    env: &TestEnv,
    server: &FakeSyncServer,
    code: Option<&str>,
) -> Result<AgentIdentity, dl_agent_pda_sync::transport::Error> {
    let (key, _) = AgentKey::load_or_create(env.secrets.as_ref()).unwrap();
    let http = env.http_for(&server.base, Arc::new(key.clone()));
    let code = code
        .map(str::to_owned)
        .unwrap_or_else(|| server.with(|s| s.valid_code.clone()));
    let r = EnrollmentClient::new(&http, &key, &env.log).redeem(
        &Ctx::background(),
        &code,
        "TEST-PC",
        "1.0.0-test",
    )?;
    env.secrets
        .save(names::AGENT_TOKEN, r.agent_token.as_bytes())
        .unwrap();
    let identity = AgentIdentity {
        agent_id: r.agent_id,
        tenant_id: r.tenant_id,
        cloud_url: server.base.clone(),
        machine_id: "TEST-PC".into(),
        sql_instance: Some(r".\SQLEXPRESS".into()),
        sql_database: Some("SMILE_POS".into()),
    };
    identity.save(&env.paths).unwrap();
    AgentStatus::write(
        &env.paths,
        &AgentStatus::new("enrolled", None, None, env.time.now()),
    )
    .unwrap();
    Ok(identity)
}

fn key_of(env: &TestEnv) -> Arc<AgentKey> {
    Arc::new(AgentKey::from_seed(&env.secrets.load(names::SIGNING_SEED).unwrap().unwrap()).unwrap())
}

fn tokens(env: &TestEnv, server: &FakeSyncServer) -> TokenProvider {
    let http = Arc::new(env.http_for(&server.base, key_of(env)));
    TokenProvider::new(
        http,
        env.secrets.clone(),
        env.paths.clone(),
        env.time.clone(),
        &env.log,
    )
}

#[test]
fn enroll_redeems_code_with_signed_request_and_persists_identity_and_secrets() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    let identity = enroll(&env, &server, None).unwrap();
    assert_eq!(identity.agent_id, server.agent_id);
    assert_eq!(identity.tenant_id, "tenant-1");
    assert_eq!(AgentIdentity::try_load(&env.paths).unwrap(), Some(identity));
    assert_eq!(
        env.secrets
            .load(names::SIGNING_SEED)
            .unwrap()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        env.secrets.load(names::AGENT_TOKEN).unwrap().unwrap(),
        server.agent_token.as_bytes()
    );
    assert!(server.rejections().is_empty());
    // The redeem body as System.Text.Json writes the anonymous object (default encoder).
    let req = server.with(|s| s.requests[0].clone());
    let body = String::from_utf8(req.body.clone()).unwrap();
    assert!(
        body.starts_with("{\"code\":\"CODE-1234\",\"publicKey\":\""),
        "{body}"
    );
    assert!(
        body.ends_with("\"machineId\":\"TEST-PC\",\"agentVersion\":\"1.0.0-test\"}"),
        "{body}"
    );
    assert!(
        !body.contains('+'),
        "base64 '+' is written as \\u002B: {body}"
    );
    assert_eq!(
        req.header("Content-Type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(req.header("User-Agent"), None);
    assert_eq!(req.header("Authorization"), None);
    assert_eq!(
        AgentStatus::try_read(&env.paths).unwrap().unwrap().state,
        "enrolled"
    );
}

#[test]
fn enroll_with_wrong_code_fails_with_stable_code_and_does_not_store_a_token() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    let err = enroll(&env, &server, Some("WRONG")).unwrap_err();
    assert_eq!(err.code(), Some(codes::ENROLL_CODE_INVALID));
    assert!(err.to_string().contains("(HTTP 400)"), "{err}");
    assert!(env.secrets.load(names::AGENT_TOKEN).unwrap().is_none());
    assert_eq!(AgentIdentity::try_load(&env.paths).unwrap(), None);
}

#[test]
fn reenroll_reuses_the_stored_key() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    let seed = env.secrets.load(names::SIGNING_SEED).unwrap();
    server.with(|s| s.valid_code = "CODE-1234".into());
    enroll(&env, &server, None).unwrap();
    assert_eq!(env.secrets.load(names::SIGNING_SEED).unwrap(), seed);
}

#[test]
fn replayed_request_is_rejected_by_the_server_nonce_cache() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    let signer = RequestSigner::new(key_of(&env), env.time.clone());
    let body = format!("{{\"agentToken\":\"{}\"}}", server.agent_token);
    let h = signer
        .sign("POST", "/sync/v1/token", body.as_bytes())
        .unwrap();
    let send = || {
        let url = dl_http::Url::parse(&format!("{}sync/v1/token", server.base)).unwrap();
        let req = dl_http::Request::post(url, body.as_bytes().to_vec())
            .with_header("X-Sync-Timestamp", &h.timestamp)
            .unwrap()
            .with_header("X-Sync-Nonce", &h.nonce)
            .unwrap()
            .with_header("X-Sync-Signature", &h.signature)
            .unwrap();
        dl_http::Client::new()
            .request(&req)
            .unwrap()
            .status
            .as_u16()
    };
    assert_eq!(send(), 200);
    assert_eq!(send(), 401);
    assert!(server.rejections().contains(&"replay".to_owned()));
}

#[test]
fn token_is_cached_and_refreshed_before_expiry() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    server.with(|s| s.token_lifetime_seconds = 300);
    let tokens = tokens(&env, &server);
    let ctx = Ctx::background();
    let first = tokens.get_access_token(&ctx).unwrap();
    assert_eq!(tokens.get_access_token(&ctx).unwrap(), first);
    assert_eq!(server.token_exchanges(), 1);
    env.time.advance(Duration::from_secs(250)); // inside the 60 s refresh skew of a 300 s token
    let second = tokens.get_access_token(&ctx).unwrap();
    assert_ne!(second, first);
    assert_eq!(server.token_exchanges(), 2);
    assert_eq!(tokens.refresh_failure(), None);
    // The token never reaches a log line.
    assert_eq!(
        dl_agent_pda_sync::reporting::scrub(&format!("x {second} {}", server.agent_token)),
        "x *** ***"
    );
}

#[test]
fn revoked_agent_gets_reenroll_hint_and_status_file() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    server.with(|s| s.revoke_agent = true);
    let tokens = tokens(&env, &server);
    let err = tokens.get_access_token(&Ctx::background()).unwrap_err();
    assert_eq!(err.code(), Some(codes::REENROLL_REQUIRED));
    assert_eq!(
        tokens.refresh_failure().as_deref(),
        Some(codes::REENROLL_REQUIRED)
    );
    let st = AgentStatus::try_read(&env.paths).unwrap().unwrap();
    assert_eq!(st.state, "reenroll_required");
    assert!(st.hint.unwrap().contains("enroll --code"));
    assert!(err.to_string().starts_with("Cloud rejected the agent token (HTTP 401); the agent was revoked or the enrollment expired. Create a new enrollment code"));
}

#[test]
fn missing_agent_token_needs_a_new_enrollment() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    env.secrets.delete(names::AGENT_TOKEN).unwrap();
    let err = tokens(&env, &server)
        .get_access_token(&Ctx::background())
        .unwrap_err();
    assert_eq!(err.code(), Some(codes::REENROLL_REQUIRED));
    assert!(err.to_string().starts_with("No stored agent token. "));
    assert_eq!(server.token_exchanges(), 0);
}

fn post_batch(
    env: &TestEnv,
    server: &FakeSyncServer,
) -> Result<dl_agent_pda_sync::transport::SyncResponse, dl_agent_pda_sync::transport::Error> {
    let tokens = tokens(env, server);
    let ctx = Ctx::background();
    let bearer = tokens.get_access_token(&ctx).unwrap();
    let http = env.http_for(&server.base, key_of(env));
    http.post_json(&ctx, "/sync/v1/batches", "{\"seq\":1}", Some(&bearer))
}

#[test]
fn transient_failures_retry_with_growing_backoff_then_succeed() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    server.with(|s| s.fail_next_batches = 2);
    let resp = post_batch(&env, &server).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(server.with(|s| s.applied.len()), 1);
    let sleeps = env.sleeps();
    assert_eq!(sleeps, [Duration::from_millis(500), Duration::from_secs(1)]);
    assert!(
        !server.rejections().contains(&"replay".to_owned()),
        "every retry is re-signed with a fresh nonce"
    );
    let warnings = env
        .sink
        .messages_at(dl_agent_pda_sync::reporting::Level::Warning);
    assert_eq!(
        warnings[0],
        "Request POST /sync/v1/batches failed (attempt 1/4): Transient HTTP 503 from /sync/v1/batches; retrying in 00:00:00.5000000"
    );
}

#[test]
fn a_persistent_outage_returns_the_last_answer_after_every_attempt() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    server.with(|s| s.fail_next_batches = 100);
    let resp = post_batch(&env, &server).unwrap();
    assert_eq!(resp.status, 503);
    assert_eq!(env.sleeps().len(), 3);
    assert!(server.with(|s| s.applied.is_empty()));
}

#[test]
fn a_permanent_refusal_is_not_retried_and_retry_after_is_honoured() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    enroll(&env, &server, None).unwrap();
    server.with(|s| {
        s.fail_next_batches = 1;
        s.fail_status = 422;
    });
    assert_eq!(post_batch(&env, &server).unwrap().status, 422);
    assert!(env.sleeps().is_empty());
    server.with(|s| {
        s.fail_next_batches = 1;
        s.fail_status = 429;
        s.retry_after = Some("7".into());
    });
    assert_eq!(post_batch(&env, &server).unwrap().status, 200);
    assert_eq!(env.sleeps(), [Duration::from_secs(7)]);
}

#[test]
fn an_unreachable_cloud_fails_with_cloud_unreachable_after_every_attempt() {
    let env = TestEnv::new();
    // A port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let key = Arc::new(AgentKey::generate().unwrap());
    let http = env.http_for(&format!("http://127.0.0.1:{port}/"), key);
    let err = http
        .get(&Ctx::background(), "/sync/v1/commands/poll", Some("jwt"))
        .unwrap_err();
    assert_eq!(err.code(), Some(codes::CLOUD_UNREACHABLE));
    assert!(
        err.to_string()
            .starts_with("Cloud unreachable after 4 attempts: "),
        "{err}"
    );
    assert_eq!(env.sleeps().len(), 3);
}

#[test]
fn a_cancelled_context_stops_before_sending() {
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());
    let http = env.http_for(&server.base, Arc::new(AgentKey::generate().unwrap()));
    let ctx = Ctx::background();
    ctx.cancel();
    assert!(matches!(
        http.get(&ctx, "/x", None),
        Err(dl_agent_pda_sync::transport::Error::Cancelled)
    ));
    assert!(server.with(|s| s.requests.is_empty()));
}

/// RT-1318: two sequential cloud calls share one TCP connection (no second handshake), the
/// second response says so, and the pool keeps idle connections for 90 s.
#[test]
fn sequential_cloud_calls_reuse_one_connection() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = accepted.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            counted.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                loop {
                    let mut length = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap();
                        }
                        if line == "\r\n" {
                            break;
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
                    if out.write_all(reply).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let env = TestEnv::new();
    let (key, _) = AgentKey::load_or_create(env.secrets.as_ref()).unwrap();
    let http = env.http_for(&base, Arc::new(key));
    let ctx = Ctx::background();
    let first = http
        .post_json(&ctx, "/sync/v1/batches", "{}", None)
        .unwrap();
    let second = http
        .post_json(&ctx, "/sync/v1/batches", "{}", None)
        .unwrap();
    assert!(first.is_success() && second.is_success());
    assert!(!first.reused && second.reused);
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "one connection for both calls"
    );
    assert_eq!(http.connections_opened(), 1);
    assert_eq!(
        dl_agent_pda_sync::transport::http::client_options(Duration::from_secs(5))
            .pool_idle_timeout,
        Duration::from_secs(90)
    );
}
