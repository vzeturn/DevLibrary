//! The SMILE site cases against the scripted server: `DOMAIN\user` logins over portable
//! NTLM, named instances resolved through a fake SQL Server Browser, SQL Server 2008 /
//! 2008 R2 answering LOGINACK with TDS 7.3, and TLS 1.0-only or resetting engines.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::fake::{FakeServer, Script, TDS_73A, TDS_73B, TlsPeer};
use crate::ntlm::tests::{challenge, verify};
use crate::token::tests::B;
use crate::token::{DONE, ROW};
use crate::types::ty;

fn plain(script: Script) -> FakeServer {
    FakeServer::start(Script {
        never_encrypt: true,
        ..script
    })
    .unwrap()
}

fn cfg(s: &str) -> Config {
    s.parse().unwrap()
}

/// The error of a connection that must fail.
fn fails(r: Result<Client, Error>, what: &str) -> Error {
    match r {
        Ok(_) => panic!("{what}: connected"),
        Err(e) => e,
    }
}

// ---- 1. Windows login with explicit credentials (NTLM) ----------------------------

fn ntlm_login(cs: &str, ch: Vec<u8>) -> (Result<Client, Error>, FakeServer, Vec<u8>) {
    let srv = plain(Script {
        sspi_challenges: vec![ch.clone()],
        ..Script::default()
    });
    let port = srv.addr().port();
    let r = Client::connect(&cfg(&format!(
        "Server=127.0.0.1,{port};Encrypt=false;Connect Timeout=5;{cs}"
    )));
    (r, srv, ch)
}

#[test]
fn domain_user_logs_in_with_ntlmv2_and_a_mic() {
    let ts = 0x01DB_5A00_1234_5678;
    let (r, srv, ch) = ntlm_login(
        "User ID=CORP\\svc_smile;Password=S3cret!;Workstation ID=POS-01",
        challenge([9; 8], Some(ts), None),
    );
    let c = r.unwrap();
    assert_eq!(c.server_version().0, 16);
    let a = &srv.attempts()[0];
    assert!(a.logged_in);
    assert_eq!(a.user, "", "a Windows login sends no SQL user name");
    assert_eq!(a.sspi.len(), 2, "NEGOTIATE in LOGIN7, one AUTHENTICATE");
    assert!(a.sspi[0].starts_with(b"NTLMSSP\0\x01\0\0\0"));
    let v = verify(&a.sspi[0], &ch, &a.sspi[1], "S3cret!").unwrap();
    assert_eq!(
        (v.domain.as_str(), v.user.as_str(), v.workstation.as_str()),
        ("CORP", "svc_smile", "POS-01")
    );
    assert!(v.mic, "MsvAvTimestamp present: MIC required");
    assert_eq!(
        verify(&a.sspi[0], &ch, &a.sspi[1], "wrong").unwrap_err(),
        "NTProofStr"
    );
    // The password never travels in clear text in any token.
    for t in &a.sspi {
        assert!(!t.windows(7).any(|w| w == b"S3cret!"));
        assert!(!t.windows(14).any(|w| w == crate::ntlm::utf16("S3cret!")));
    }
}

#[test]
fn ntlm_without_server_timestamp_uses_lmv2() {
    let (r, srv, ch) = ntlm_login(
        "User ID=MN-SERVER\\Administrator;Password=pw",
        challenge([3; 8], None, None),
    );
    r.unwrap();
    let a = &srv.attempts()[0];
    let v = verify(&a.sspi[0], &ch, &a.sspi[1], "pw").unwrap();
    assert!(!v.mic);
    assert_eq!(v.domain, "MN-SERVER");
}

#[test]
fn a_bad_challenge_is_an_ntlm_error_not_a_panic() {
    let (r, _srv, _) = ntlm_login("User ID=CORP\\u;Password=p", b"garbage".to_vec());
    let e = fails(r, "garbage challenge");
    assert!(matches!(e, Error::Ntlm(_)), "{e:?}");
    assert!(!e.to_string().contains("garbage"));
}

/// Off Windows, `Integrated Security=SSPI` with `DOMAIN\user` credentials logs in with
/// NTLM; without them it stays unsupported. On Windows SSPI (current account) stays the
/// first choice, checked by `sspi_token_flow_answers_each_challenge_until_loginack`.
#[cfg(not(windows))]
#[test]
fn integrated_security_off_windows_needs_domain_credentials() {
    let (r, srv, ch) = ntlm_login(
        "Integrated Security=SSPI;User ID=CORP\\svc;Password=x",
        challenge([5; 8], Some(1), None),
    );
    r.unwrap();
    let a = &srv.attempts()[0];
    assert_eq!(
        verify(&a.sspi[0], &ch, &a.sspi[1], "x").unwrap().user,
        "svc"
    );
    let e = Client::connect(&cfg("Server=127.0.0.1,1;Integrated Security=SSPI")).err();
    assert!(matches!(e, Some(Error::Unsupported(_))), "{e:?}");
}

#[test]
fn a_plain_sql_login_never_starts_ntlm() {
    let srv = plain(Script::default());
    let port = srv.addr().port();
    Client::connect(&cfg(&format!(
        "Server=127.0.0.1,{port};User ID=smile;Password=x;Encrypt=false"
    )))
    .unwrap();
    let a = &srv.attempts()[0];
    assert_eq!(a.user, "smile");
    assert!(a.sspi.is_empty());
}

// ---- 2. Named instances through the SQL Server Browser ------------------------------

/// A fake SQL Server Browser on loopback UDP: answers every request for `instance` with
/// `tcp_port`, records the requests, stops when dropped.
struct Browser {
    port: u16,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
}

impl Browser {
    fn start(instance: &str, tcp_port: u16) -> Browser {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let port = sock.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, halt) = (Arc::clone(&seen), Arc::clone(&stop));
        let body = format!(
            "ServerName;MN-SERVER;InstanceName;{instance};IsClustered;No;Version;10.50.1600.1;tcp;{tcp_port};;"
        );
        let mut answer = vec![0x05];
        answer.extend_from_slice(&(body.len() as u16).to_le_bytes());
        answer.extend_from_slice(body.as_bytes());
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            while !halt.load(Ordering::Relaxed) {
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    log.lock().unwrap().push(buf[..n].to_vec());
                    sock.send_to(&answer, from).unwrap();
                }
            }
        });
        crate::browser::test_port::set(Some(port));
        Browser { port, seen, stop }
    }

    fn requests(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        crate::browser::test_port::set(None);
    }
}

#[test]
fn named_instance_resolves_through_the_browser_then_logs_in() {
    let srv = plain(Script {
        tds_version: TDS_73B,
        server_version: Some((10, 50, 1600)),
        ..Script::default()
    });
    let tcp = srv.addr().port();
    let b = Browser::start("SMILE", tcp);
    assert_ne!(b.port, 1434);
    for server in [
        "127.0.0.1\\SMILE",
        ".\\SMILE",
        "(local)\\smile",
        "localhost\\SMILE",
    ] {
        let mut c = Client::connect(&cfg(&format!(
            "Server={server};Database=SMILE_FO;User ID=smile;Password=x;Encrypt=false"
        )))
        .unwrap_or_else(|e| panic!("{server}: {e}"));
        assert_eq!(c.peer_addr().map(|a| a.port()), Some(tcp), "{server}");
        c.execute("SET LOCK_TIMEOUT 8000", &[]).unwrap();
    }
    let url: Config = "sqlserver://smile:x@127.0.0.1/SMILE?encrypt=disable"
        .parse()
        .unwrap();
    Client::connect(&url).unwrap();
    let reqs = b.requests();
    assert_eq!(reqs.len(), 5, "one Browser lookup per connection");
    assert_eq!(reqs[0], b"\x04SMILE\x00");
    assert_eq!(reqs[2], b"\x04smile\x00", "sent as written");
    let seen = srv.attempts();
    assert_eq!(seen.len(), 5);
    assert!(seen.iter().all(|a| a.logged_in));
    // The instance name also goes into PRELOGIN (INSTOPT), as SqlClient sends it.
    assert_eq!(seen[0].instance, "SMILE");
}

#[test]
fn an_explicit_port_wins_over_the_instance_and_skips_the_browser() {
    let srv = plain(Script::default());
    let tcp = srv.addr().port();
    // The browser would send the client to a closed port.
    let b = Browser::start("SMILE", 1);
    for server in [
        format!("127.0.0.1\\SMILE,{tcp}"),
        format!(".\\SMILE,{tcp}"),
        format!("tcp:127.0.0.1\\SMILE,{tcp}"),
    ] {
        let c = Client::connect(&cfg(&format!(
            "Server={server};User ID=smile;Password=x;Encrypt=false"
        )))
        .unwrap_or_else(|e| panic!("{server}: {e}"));
        assert_eq!(c.peer_addr().map(|a| a.port()), Some(tcp), "{server}");
    }
    assert!(b.requests().is_empty(), "{:?}", b.requests());
}

#[test]
fn an_unknown_instance_is_a_browser_error_before_any_tcp() {
    let srv = plain(Script::default());
    let _b = Browser::start("OTHER", srv.addr().port());
    let r = Client::connect(&cfg(
        "Server=127.0.0.1\\SMILE;User ID=smile;Password=x;Encrypt=false;Connect Timeout=2",
    ));
    let e = fails(r, "unknown instance");
    assert!(matches!(e, Error::Browser(_)), "{e:?}");
    assert!(e.to_string().contains("SMILE"), "{e}");
    assert!(srv.attempts().is_empty());
}

// ---- 3. SQL Server 2008 / 2008 R2: TDS 7.3 --------------------------------------------

/// `SELECT id, name, at, d, amount` with an int, nvarchar(20), datetime2(7), date and
/// decimal(18,2) column, two rows (the second all NULL but `id`), as TDS 7.3 sends them.
fn select_2008() -> Vec<u8> {
    let days = |y, m, d| {
        let n = dl_time::Date::new(y, m, d).unwrap().to_unix_days() + 719_162;
        (n as u32).to_le_bytes()[..3].to_vec()
    };
    let ticks: u64 = (12 * 3600 + 34 * 60 + 56) * 10_000_000 + 1_234_567;
    let mut dt2 = ticks.to_le_bytes()[..5].to_vec();
    dt2.extend_from_slice(&days(2010, 4, 2));
    B::default()
        .colmeta(&[
            ("id", 0, &[ty::INT4]),
            ("name", 1, &[ty::NVARCHAR, 40, 0, 9, 4, 0xD0, 0, 52]),
            ("at", 1, &[ty::DATETIME2N, 7]),
            ("d", 1, &[ty::DATEN]),
            ("amount", 1, &[ty::DECIMALN, 9, 18, 2]),
        ])
        .u8(ROW)
        .u32(7)
        .u16(6)
        .raw(&crate::wire::to_utf16("Mũi"))
        .u8(8)
        .raw(&dt2)
        .u8(3)
        .raw(&days(2008, 8, 8))
        .u8(9)
        .u8(1)
        .u64(123_456)
        .u8(ROW)
        .u32(8)
        .u16(0xFFFF)
        .u8(0)
        .u8(0)
        .u8(0)
        .done(DONE, 0x10, 2)
        .0
}

fn sql2008(tds: u32, version: (u8, u8, u16)) {
    let srv = plain(Script {
        tds_version: tds,
        server_version: Some(version),
        responses: vec![select_2008(), B::default().done(DONE, 0x10, 3).0],
        ..Script::default()
    });
    let mut c = Client::connect(&cfg(&format!(
        "Server=127.0.0.1,{};User ID=smile;Password=x;Encrypt=false",
        srv.addr().port()
    )))
    .unwrap();
    assert_eq!(c.tds_version(), tds);
    assert_eq!(c.server_version(), version);
    let rs = c
        .query("SELECT id, name, at, d, amount FROM t", &[])
        .unwrap();
    let rows = rs.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<i32>("id").unwrap(), 7);
    assert_eq!(rows[0].get::<String>("name").unwrap(), "Mũi");
    let at = rows[0].get::<dl_time::DateTime>("at").unwrap();
    assert_eq!(
        (at.date().to_string(), at.time().nanosecond()),
        ("2010-04-02".to_string(), 123_456_700)
    );
    assert_eq!(
        rows[0].get::<dl_time::Date>("d").unwrap().to_string(),
        "2008-08-08"
    );
    assert_eq!(
        rows[0]
            .get::<dl_decimal::Decimal>("amount")
            .unwrap()
            .to_string(),
        "1234.56"
    );
    assert_eq!(rows[1].get::<Option<String>>("name").unwrap(), None);
    assert_eq!(
        rows[1].get::<Option<dl_time::DateTime>>("at").unwrap(),
        None
    );
    assert_eq!(c.execute("UPDATE t SET x = 1", &[]).unwrap(), 3);
    let a = &srv.attempts()[0];
    assert_eq!(
        a.login_tds_version,
        crate::login::TDS_74,
        "asks 7.4, accepts 7.3"
    );
    assert_eq!(a.requests, 2);
}

#[test]
fn sql_server_2008_r2_answers_tds_73b() {
    sql2008(TDS_73B, (10, 50, 1600));
}

#[test]
fn sql_server_2008_answers_tds_73a() {
    sql2008(TDS_73A, (10, 0, 1600));
}

// ---- 3b. TLS 1.0-only engines and resets ----------------------------------------------

/// SQL Server 2008 without the TLS 1.2 update, as the client walks dl-agent's ladder by
/// hand: the default transport gets a TLS 1.0 ServerHello and refuses it with an alert,
/// TLS 1.0 is reset, and only the unencrypted login reaches LOGINACK (TDS 7.3).
#[cfg(any(windows, all(unix, not(target_vendor = "apple"))))]
#[test]
fn tls10_only_sql_2008_r2_errors_are_tls_errors_until_unencrypted() {
    let srv = FakeServer::start(Script {
        tls: TlsPeer::Tls10Only,
        tds_version: TDS_73B,
        server_version: Some((10, 50, 1600)),
        ..Script::default()
    })
    .unwrap();
    let base = cfg(&format!(
        "Server=127.0.0.1,{};User ID=smile;Password=x;TrustServerCertificate=true;Connect Timeout=5",
        srv.addr().port()
    ));
    let e = fails(Client::connect(&base), "TLS 1.2 vs TLS 1.0 engine");
    eprintln!("default transport vs TLS 1.0 engine: {e}");
    assert!(e.is_tls(), "{e:?}");
    assert!(
        matches!(
            e,
            Error::Tls(dl_tls::Error::Handshake { .. } | dl_tls::Error::Io(_))
        ),
        "a step-down error for dl-agent's ladder: {e:?}"
    );
    let e = fails(
        Client::connect(&base.clone().with_tls_min(crate::TlsVersion::Tls10)),
        "reset",
    );
    eprintln!("TLS 1.0 transport vs resetting engine: {e}");
    assert!(e.is_tls(), "{e:?}");
    let c = Client::connect(&base.clone().with_encrypt(Encrypt::Disabled)).unwrap();
    assert_eq!(c.tds_version(), TDS_73B);
    let seen = srv.attempts();
    assert_eq!(seen.len(), 3);
    assert_eq!(
        seen[0].tls_alert,
        Some(70),
        "protocol_version alert: {:?}",
        seen[0]
    );
    assert!(seen[1].tls_versions.contains(&0x0301), "{:?}", seen[1]);
    assert!(!seen[0].logged_in && !seen[1].logged_in && seen[2].logged_in);
}

/// A reset in the middle of the handshake is an `Error::Tls` with an I/O cause (dl-agent
/// steps down on it), never a hang, and `Encrypt=true` does not fall back to clear text.
#[cfg(any(windows, all(unix, not(target_vendor = "apple"))))]
#[test]
fn a_reset_during_the_handshake_is_a_tls_io_error() {
    let srv = FakeServer::start(Script {
        tls: TlsPeer::ResetOnHello,
        ..Script::default()
    })
    .unwrap();
    for extra in ["", ";Encrypt=true", ";TLSMin=1.0"] {
        let r = Client::connect(&cfg(&format!(
            "Server=127.0.0.1,{};User ID=smile;Password=x;TrustServerCertificate=true;Connect Timeout=5{extra}",
            srv.addr().port()
        )));
        let e = fails(r, "reset");
        assert!(
            matches!(e, Error::Tls(dl_tls::Error::Io(_))),
            "{extra}: {e:?}"
        );
    }
    assert!(srv.attempts().iter().all(|a| !a.logged_in));
}
