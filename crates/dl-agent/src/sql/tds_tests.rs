//! [`TdsConnector`] against the scripted TDS server of `dl_tds::fake`: the rung that
//! connects, the port a session reached, and what the hub sees in `describe()`.

use dl_tds::fake::{FakeServer, Script};

use super::*;

fn plain() -> FakeServer {
    FakeServer::start(Script {
        never_encrypt: true,
        ..Script::default()
    })
    .unwrap()
}

#[test]
fn a_server_without_tls_connects_on_the_first_rung_and_reports_its_port() {
    let srv = plain();
    let port = srv.addr().port();
    let c = TdsConnector::parse(&format!(
        "Server=(local),{port};Database=SMILE_FO;User Id=smile;Password=hunter2"
    ))
    .unwrap();
    assert_eq!(c.resolved_port(), None);
    assert!(c.rung().is_none());
    c.connect().unwrap();
    assert_eq!(c.rung(), Some(transport::Rung::AsConfigured));
    assert_eq!(c.resolved_port(), Some(port));
    let d = c.describe();
    assert_eq!(d["rung"].as_str(), Some("as_configured"));
    assert_eq!(d["auth"].as_str(), Some("sql"));
    assert_eq!(d["login"].as_str(), Some("smile"));
    // `(local)` and `.` are normalised to `localhost` by the parser.
    assert_eq!(d["server"].as_str(), Some("localhost"));
    assert_eq!(d["resolved_port"].as_u64(), Some(u64::from(port)));
    assert!(!dl_json::to_string(&d).contains("hunter2"));
    // The LOCK_TIMEOUT batch reached the server after login.
    let a = &srv.attempts()[0];
    assert!(a.logged_in && a.requests == 1, "{a:?}");
}

#[test]
fn selftest_reach_names_mode_login_server_and_port() {
    let srv = plain();
    let port = srv.addr().port();
    let c = TdsConnector::parse(&format!("Server=.,{port};User Id=smile;Password=x")).unwrap();
    c.connect().unwrap();
    let r = crate::selftest::checks_core::reach(&c.describe(), None);
    assert_eq!(r, format!("sql auth as smile to localhost (port {port})"));
    let r = crate::selftest::checks_core::reach(
        &json!({"auth": "windows", "server": ".", "instance": "SQLEXPRESS", "resolved_port": 49172}),
        Some("NT AUTHORITY\\SYSTEM"),
    );
    assert_eq!(
        r,
        "windows auth as NT AUTHORITY\\SYSTEM to .\\SQLEXPRESS (port 49172)"
    );
}

/// The ladder against a server that offers TLS and cannot finish the handshake: TLS 1.2,
/// then TLS 1.0, then unencrypted (PROTOCOL.md §9.4). Needs a TLS backend (not macOS).
#[cfg(any(windows, all(unix, not(target_vendor = "apple"))))]
#[test]
fn the_ladder_steps_from_tls12_to_tls10_to_unencrypted() {
    let srv = FakeServer::start(Script::default()).unwrap();
    let c = TdsConnector::parse(&format!(
        "Server=127.0.0.1,{};User Id=smile;Password=x;TrustServerCertificate=true",
        srv.addr().port()
    ))
    .unwrap();
    c.connect().unwrap();
    assert_eq!(c.rung(), Some(transport::Rung::Unencrypted));
    let seen = srv.attempts();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[0].tls_versions.contains(&0x0303) && !seen[0].tls_versions.contains(&0x0301));
    assert!(seen[1].tls_versions.contains(&0x0301), "{:?}", seen[1]);
    assert_eq!(seen[2].client_encryption, 2, "NOT_SUP on the last rung");
    assert!(seen[2].logged_in);
    // Settled: a reconnect stays on the rung that worked.
    c.connect().unwrap();
    assert_eq!(srv.attempts().len(), 4);
}
