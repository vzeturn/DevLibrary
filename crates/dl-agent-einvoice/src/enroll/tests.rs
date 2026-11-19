//! Tests for the enroll flow: prompt flow with scripted input, refusal without a SQL
//! password, the transport fallback, and nothing persisted on error. Hand-written fakes
//! only: a socket platform and a closure ping.

use super::prompt::split_server_address;
use super::*;
use std::io::Read;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A platform that answers every request with an API key (the exchange) and counts
/// the exchanges; `/config` gets the same body, which the agent reads as defaults.
pub(super) fn fake_platform() -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let n = Arc::new(AtomicUsize::new(0));
    let count = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => req.extend_from_slice(&buf[..k]),
                }
            }
            let head = String::from_utf8_lossy(&req).to_ascii_lowercase();
            if head.starts_with("post /agent/enroll") {
                count.fetch_add(1, Ordering::SeqCst);
            }
            // Drain the body.
            if let Some(len) = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
            {
                let have = req.len() - head.find("\r\n\r\n").map_or(req.len(), |i| i + 4);
                let mut rest = vec![0u8; len.saturating_sub(have)];
                let _ = s.read_exact(&mut rest);
            }
            let body =
                r#"{"api_key":"dl_key_SECRET","tenant_id":"22222222-2222-2222-2222-222222222222"}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, n)
}

struct Run {
    cfg: Option<AgentConfig>,
    out: String,
    exchanges: usize,
    err: Option<String>,
}

#[test]
fn without_a_password_or_a_build_default_the_enroll_refuses_before_the_exchange() {
    if !default_sql_password().is_empty() {
        return; // a release-shaped build: the default applies
    }
    let mut out = Vec::new();
    let err = enroll(EnrollOptions {
        config_dir: std::env::temp_dir().join("dl-enroll-no-password"),
        // Nothing listens here: reaching the exchange would fail differently.
        platform_url: "http://127.0.0.1:1".into(),
        enroll_token: "tok".into(),
        agent_host: "APP-01".into(),
        sql_host: String::new(),
        sql_user: String::new(),
        sql_password: String::new(),
        sql_auth: String::new(),
        collect_from: None,
        previous: None,
        prompt_sql: false,
        input: None,
        hide_input: None,
        out: &mut out,
        agent_version: "dl-agent-test".into(),
        ping: None,
        env: &|_| None,
    })
    .unwrap_err();
    assert!(err.contains("this build has no default password"), "{err}");
    assert!(out.is_empty());
}

fn run(prompt: bool, stdin: &str, ping: Ping<'_>) -> Run {
    run_with(prompt, stdin, ping, None)
}

/// `previous` set: no SQL flags at all, the earlier enrolment fills them.
fn run_with(prompt: bool, stdin: &str, ping: Ping<'_>, previous: Option<Previous>) -> Run {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let (url, exchanges) = fake_platform();
    let dir = std::env::temp_dir().join(format!(
        "dl-enroll-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut input = std::io::Cursor::new(stdin.as_bytes().to_vec());
    let mut out = Vec::new();
    let r = enroll(EnrollOptions {
        config_dir: dir.clone(),
        platform_url: url,
        enroll_token: "tok".into(),
        agent_host: "APP-01".into(),
        sql_host: String::new(),
        sql_user: String::new(),
        // The Go tests rely on the built-in password; test builds have none.
        sql_password: if prompt || previous.is_some() {
            String::new()
        } else {
            "pw".into()
        },
        sql_auth: String::new(),
        collect_from: None,
        previous,
        prompt_sql: prompt,
        input: Some(&mut input),
        hide_input: None,
        out: &mut out,
        agent_version: "dl-agent-test".into(),
        ping: Some(ping),
        env: &|k| (k == "MMEI_AGENT_MOCK").then(|| "1".to_owned()),
    });
    let cfg = r.is_ok().then(|| {
        AgentConfig::load(&dir)
            .expect("load sealed config")
            .expect("config.enc exists")
    });
    let _ = std::fs::remove_dir_all(&dir);
    Run {
        cfg,
        out: String::from_utf8_lossy(&out).into_owned(),
        exchanges: exchanges.load(Ordering::SeqCst),
        err: r.err(),
    }
}

fn ok(r: &Run) -> &AgentConfig {
    assert!(r.err.is_none(), "enroll: {:?}\n{}", r.err, r.out);
    r.cfg.as_ref().unwrap()
}

fn tls_fail(m: &str) -> Result<(), PingError> {
    Err(PingError {
        message: m.into(),
        tls: true,
    })
}

#[test]
fn prompted_seals_typed_connection() {
    // host, port (Enter = 1433), user, password, FO db (Enter), POS db.
    let r = run(
        true,
        "192.168.1.10\n\nsa\np@ss word\n\nSMILE_POS2\n",
        &|_| Ok(()),
    );
    let cfg = ok(&r);
    for w in ["sa:p%40ss%20word@192.168.1.10:1433", "database=SMILE_FO"] {
        assert!(cfg.sql_dsn.contains(w), "{} lacks {w}", cfg.sql_dsn);
    }
    assert!(
        cfg.pos_sql_dsn.contains("database=SMILE_POS2"),
        "{}",
        cfg.pos_sql_dsn
    );
    assert!(!r.out.contains("p@ss word"), "password echoed:\n{}", r.out);
}

#[test]
fn prompted_accepts_ssms_style_address() {
    let r = run(true, "dbsrv\\SQL2012\n\n\nsecret\n\n\n", &|_| Ok(()));
    assert!(
        ok(&r).sql_dsn.contains("@dbsrv/SQL2012?"),
        "{}",
        ok(&r).sql_dsn
    );
}

#[test]
fn prompted_asks_again_after_failed_test() {
    let ping = |d: &Dsn| {
        if d.host == "10.0.0.9" {
            Ok(())
        } else {
            Err(PingError {
                message: "dial tcp: no route to host".into(),
                tls: false,
            })
        }
    };
    // Round 1: wrong host; "Nhap lai?" -> Enter (yes); round 2 retypes only the host.
    let stdin = "10.0.0.1\n\nsa\nsecret\n\n\n\n10.0.0.9\n\n\n\n\n\n";
    let r = run(true, stdin, &ping);
    assert!(
        ok(&r).sql_dsn.contains("sa:secret@10.0.0.9:1433"),
        "{}",
        ok(&r).sql_dsn
    );
    assert!(
        r.out.contains("LOI: dial tcp") && r.exchanges == 1,
        "{}",
        r.out
    );
}

#[test]
fn prompted_falls_back_to_legacy_tls_unasked() {
    let ping = |d: &Dsn| {
        if d.tls_min.is_empty() {
            tls_fail("TLS handshake failed (0x80090302): the function is not supported")
        } else {
            Ok(())
        }
    };
    let r = run(true, "10.0.0.1\n\nsa\nsecret\n\n\n", &ping);
    let cfg = ok(&r);
    assert!(cfg.sql_dsn.contains("tlsmin=1.0") && cfg.pos_sql_dsn.contains("tlsmin=1.0"));
    assert!(
        !cfg.sql_dsn.contains("encrypt=") && r.out.contains("dung TLS 1.0"),
        "{}",
        r.out
    );
}

#[test]
fn prompted_abort_does_not_spend_token() {
    let ping = |_: &Dsn| {
        Err(PingError {
            message: "login failed".into(),
            tls: false,
        })
    };
    let r = run(true, "10.0.0.1\n\nsa\nsecret\n\n\n", &ping);
    assert!(
        r.err.is_some() && r.exchanges == 0,
        "{:?} {}",
        r.err,
        r.exchanges
    );
}

#[test]
fn local_falls_back_to_legacy_tls_unasked() {
    let ping = |d: &Dsn| {
        if d.tls_min.is_empty() {
            tls_fail("TLS I/O error: connection reset")
        } else {
            Ok(())
        }
    };
    let r = run(false, "", &ping);
    let cfg = ok(&r);
    assert!(cfg.sql_dsn.contains("tlsmin=1.0") && cfg.pos_sql_dsn.contains("tlsmin=1.0"));
}

#[test]
fn no_encryption_only_when_tls10_also_fails() {
    let ping = |d: &Dsn| {
        if d.encrypt == "disable" {
            Ok(())
        } else {
            tls_fail("TLS handshake failed: handshake failure")
        }
    };
    let r = run(false, "", &ping);
    let cfg = ok(&r);
    assert!(cfg.sql_dsn.contains("encrypt=disable") && !cfg.sql_dsn.contains("tlsmin"));
    assert!(r.out.contains("KHONG ma hoa"), "{}", r.out);
}

#[test]
fn default_kept_for_non_tls_failure() {
    let calls = AtomicUsize::new(0);
    let ping = |_: &Dsn| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(PingError {
            message: "login failed for user 'smile'".into(),
            tls: false,
        })
    };
    let r = run(false, "", &ping);
    let cfg = ok(&r);
    assert!(!cfg.sql_dsn.contains("tlsmin") && !cfg.sql_dsn.contains("encrypt"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn default_kept_when_every_fallback_fails() {
    let r = run(false, "", &|_| tls_fail("TLS I/O error: unexpected EOF"));
    let cfg = ok(&r);
    assert!(!cfg.sql_dsn.contains("tlsmin") && !cfg.sql_dsn.contains("encrypt"));
}

#[test]
fn explicit_transport_is_pinged_as_is() {
    let calls = AtomicUsize::new(0);
    let ping = |_: &Dsn| {
        calls.fetch_add(1, Ordering::SeqCst);
        tls_fail("x")
    };
    let d = Dsn {
        tls_min: "1.0".into(),
        ..Dsn::default()
    };
    let (got, r) = ping_negotiated(&d, &ping);
    assert_eq!(got, d);
    assert!(r.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn closed_input_is_an_error_not_a_loop() {
    let r = run(true, "10.0.0.1\n", &|_| Ok(()));
    assert_eq!(r.err.as_deref(), Some("agent: read answer: EOF"));
    assert_eq!(r.exchanges, 0);
}

#[test]
fn the_earlier_enrolment_fills_the_connection_and_the_window() {
    let since = vn_midnight(Some("2026-09-01")).unwrap();
    let prev = Previous {
        dir: PathBuf::from("C:/ProgramData/MM-EInvoice.bench-old-20261119090000"),
        fo: previous::connection("sqlserver://smile:old%21pw@SRV/SQL2008?database=SMILE_FO_X"),
        pos: previous::connection("sqlserver://smile:old%21pw@SRV/SQL2008?database=SMILE_POS"),
        collect_from: Some(since),
    };
    let pinged = std::sync::Mutex::new(Vec::new());
    let ping = |d: &Dsn| {
        pinged.lock().unwrap().push(d.clone());
        Ok(())
    };
    let r = run_with(false, "", &ping, Some(prev));
    let cfg = ok(&r);
    assert!(r.out.contains("Earlier enrolment found in"), "{}", r.out);
    assert!(
        !r.out.contains("old!pw"),
        "no secret in the output: {}",
        r.out
    );
    let fo = previous::connection(&cfg.sql_dsn).unwrap();
    assert_eq!(
        (fo.host.as_str(), fo.instance.as_str(), fo.database.as_str()),
        ("SRV", "SQL2008", "SMILE_FO_X")
    );
    assert_eq!(
        (fo.user.as_str(), fo.password.as_str()),
        ("smile", "old!pw")
    );
    assert_eq!(
        previous::connection(&cfg.pos_sql_dsn).unwrap().database,
        "SMILE_POS"
    );
    // The fake platform's /config has no collect_from: the earlier window wins over today.
    assert_eq!(cfg.collect_from, Some(since));
    assert_eq!(pinged.lock().unwrap().len(), 2);
}

#[test]
fn an_earlier_windows_login_stays_windows() {
    let prev = Previous {
        fo: previous::connection("sqlserver://SRV?database=SMILE_FO"),
        ..Previous::default()
    };
    let ok_ping = |_: &Dsn| Ok(());
    let r = run_with(false, "", &ok_ping, Some(prev));
    let fo = previous::connection(&ok(&r).sql_dsn).unwrap();
    assert!(fo.user.is_empty() && fo.password.is_empty(), "{fo:?}");
}

#[test]
fn address_forms() {
    assert_eq!(split_server_address("h\\I"), ("h".into(), "I".into()));
    assert_eq!(split_server_address("h, 1444"), ("h".into(), "1444".into()));
    assert_eq!(split_server_address("h:1444"), ("h".into(), "1444".into()));
    assert_eq!(split_server_address("h"), ("h".into(), String::new()));
    assert_eq!(split_server_address(":1"), (":1".into(), String::new()));
}
