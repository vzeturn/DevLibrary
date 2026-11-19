//! Connection path against the scripted server in [`crate::fake`]: server aliases, the
//! peer address, the SSPI token flow and the TLS versions each transport offers.

use super::*;
use crate::fake::{FakeServer, Script};

fn plain() -> Script {
    Script {
        never_encrypt: true,
        ..Script::default()
    }
}

#[test]
fn local_aliases_dial_loopback() {
    for h in [".", "(local)", "(LOCAL)", "localhost", "LocalHost"] {
        assert_eq!(dial_host(h), "127.0.0.1", "{h}");
    }
    assert_eq!(dial_host("db01"), "db01");
    assert_eq!(dial_host("10.0.0.5"), "10.0.0.5");
}

#[test]
fn alias_server_logs_in_and_reports_the_peer() {
    let srv = FakeServer::start(plain()).unwrap();
    let port = srv.addr().port();
    for server in [".", "(local)", "localhost"] {
        let cfg: Config = format!("Server={server},{port};User Id=smile;Password=x;Encrypt=false")
            .parse()
            .unwrap();
        let mut c = Client::connect(&cfg).unwrap();
        assert_eq!(c.peer_addr().map(|a| a.port()), Some(port));
        assert!(!c.is_encrypted());
        c.execute("SET LOCK_TIMEOUT 8000", &[]).unwrap();
    }
    let seen = srv.attempts();
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|a| a.logged_in && a.user == "smile"));
}

#[test]
fn sspi_token_flow_answers_each_challenge_until_loginack() {
    let srv = FakeServer::start(Script {
        never_encrypt: true,
        sspi_challenges: vec![b"CHALLENGE-1".to_vec(), b"CHALLENGE-2".to_vec()],
    })
    .unwrap();
    let port = srv.addr().port();
    let targets = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    crate::sspi::fake::install(crate::sspi::fake::Package {
        rounds: 2,
        answered: 0,
        targets: std::rc::Rc::clone(&targets),
    });
    let cfg: Config = format!(
        "Server=127.0.0.1,{port};Integrated Security=SSPI;Encrypt=false;ServerSPN=MSSQLSvc/fake.test:{port}"
    )
    .parse()
    .unwrap();
    let c = Client::connect(&cfg).unwrap();
    assert_eq!(c.server_version().0, 16);
    let a = &srv.attempts()[0];
    assert!(a.logged_in);
    assert_eq!(a.user, "", "integrated login sends no user name");
    let sspi: Vec<&[u8]> = a.sspi.iter().map(Vec::as_slice).collect();
    assert_eq!(
        sspi,
        [
            b"NEGOTIATE".as_slice(),
            b"AUTH:CHALLENGE-1".as_slice(),
            b"AUTH:CHALLENGE-2".as_slice()
        ]
    );
    assert_eq!(*targets.borrow(), [format!("MSSQLSvc/fake.test:{port}")]);
}

#[test]
fn sspi_challenge_without_a_context_is_refused() {
    // A server that asks for SSPI round trips from a SQL login is malformed for us.
    let srv = FakeServer::start(Script {
        never_encrypt: true,
        sspi_challenges: vec![b"X".to_vec()],
    })
    .unwrap();
    let cfg: Config = format!(
        "Server=127.0.0.1,{};User Id=sa;Password=x;Encrypt=false;Connect Timeout=3",
        srv.addr().port()
    )
    .parse()
    .unwrap();
    assert!(Client::connect(&cfg).is_err());
}

/// The TLS versions each transport offers; needs a TLS backend (not macOS).
#[cfg(any(windows, all(unix, not(target_vendor = "apple"))))]
#[test]
fn tls_rungs_offer_their_minimum_version() {
    let srv = FakeServer::start(Script::default()).unwrap();
    let base: Config = format!(
        "Server=127.0.0.1,{};User Id=sa;Password=x;TrustServerCertificate=true;Connect Timeout=5",
        srv.addr().port()
    )
    .parse()
    .unwrap();
    assert!(matches!(Client::connect(&base), Err(Error::Tls(_))));
    let legacy = base.clone().with_tls_min(crate::TlsVersion::Tls10);
    assert!(matches!(Client::connect(&legacy), Err(Error::Tls(_))));
    let seen = srv.attempts();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].tls_versions.contains(&0x0303), "{:?}", seen[0]);
    assert!(!seen[0].tls_versions.contains(&0x0301), "{:?}", seen[0]);
    assert!(seen[1].tls_versions.contains(&0x0301), "{:?}", seen[1]);
}
