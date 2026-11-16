//! Windows integrated authentication against a real SQL Server.
//!
//! Set `DL_TDS_SSPI_SERVER` to a server that accepts the current Windows account, e.g.
//! `localhost`, `.\SQLEXPRESS` or `db01,1433` (the ADO `Server=` value). Without it the
//! tests print a note and pass.

use dl_tds::{Client, Config};

fn server() -> Option<String> {
    match std::env::var("DL_TDS_SSPI_SERVER") {
        Ok(s) if !s.is_empty() => Some(s),
        _ => {
            eprintln!(
                "note: DL_TDS_SSPI_SERVER is not set; skipping integrated authentication test"
            );
            None
        }
    }
}

/// `DOMAIN\user` of the current Windows account.
#[cfg(windows)]
fn current_user() -> String {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!("{domain}\\{user}")
}

/// Logs in and checks who the server thinks we are and how we authenticated.
#[cfg(windows)]
fn check(cfg: &Config) {
    assert!(cfg.integrated_auth(), "{cfg:?}");
    let mut c = Client::connect(cfg).unwrap_or_else(|e| panic!("connect {cfg:?}: {e}"));
    for m in c.take_messages() {
        eprintln!("  login message {}: {}", m.number, m.message);
    }
    let rs = c
        .query(
            "SELECT SUSER_SNAME(), auth_scheme FROM sys.dm_exec_connections WHERE session_id = @@SPID",
            &[],
        )
        .unwrap_or_else(|e| panic!("query {cfg:?}: {e}"));
    let row = &rs.rows()[0];
    let login: String = row.get(0).unwrap();
    let scheme: String = row.get(1).unwrap();
    eprintln!(
        "integrated login ({:?}): SUSER_SNAME()={login} auth_scheme={scheme} encrypted={} server={:?}",
        cfg.encrypt(),
        c.is_encrypted(),
        c.server_version()
    );
    assert!(
        login.eq_ignore_ascii_case(&current_user()),
        "{login} vs {}",
        current_user()
    );
    assert!(
        scheme == "NTLM" || scheme == "KERBEROS",
        "auth_scheme {scheme}"
    );
    c.ping().unwrap();
    c.close();
}

#[cfg(windows)]
#[test]
fn integrated_login_ado_spellings() {
    let Some(server) = server() else { return };
    // Default encryption: the login packet only, if the server has TLS.
    for key in [
        "Integrated Security=SSPI",
        "Integrated Security=true",
        "Trusted_Connection=yes",
    ] {
        let cfg: Config = format!("Server={server};Database=master;{key}")
            .parse()
            .unwrap();
        check(&cfg);
    }
}

#[cfg(windows)]
#[test]
fn integrated_login_encrypted_session() {
    let Some(server) = server() else { return };
    let cfg: Config = format!(
        "Server={server};Integrated Security=SSPI;Encrypt=true;TrustServerCertificate=true"
    )
    .parse()
    .unwrap();
    assert_eq!(cfg.encrypt(), dl_tds::Encrypt::Required);
    check(&cfg);
}

#[cfg(windows)]
#[test]
fn integrated_login_without_encryption() {
    let Some(server) = server() else { return };
    // SQL Server may still insist on encrypting the login.
    let cfg: Config = format!("Server={server};Integrated Security=SSPI;Encrypt=disable")
        .parse()
        .unwrap();
    check(&cfg);
}

#[cfg(windows)]
#[test]
fn integrated_login_url_forms_and_builder() {
    let Some(server) = server() else { return };
    // URL forms: explicit key, and no user name.
    let (host, rest) = match server.split_once('\\') {
        Some((h, i)) => (h.to_string(), format!("/{i}")),
        None => (server.clone(), String::new()),
    };
    let host = host.replace(',', ":");
    for url in [
        format!("sqlserver://{host}{rest}?trusted_connection=true&database=master"),
        format!("sqlserver://{host}{rest}?database=master"),
    ] {
        check(&url.parse().unwrap());
    }
    // Builder.
    let base: Config = format!("Server={server}").parse().unwrap();
    check(&base.with_integrated_auth());
}

#[cfg(windows)]
#[test]
fn unregistered_spn_override_is_ntlm_or_a_typed_error() {
    let Some(server) = server() else { return };
    // An SPN nobody registered: in a domain Negotiate falls back to NTLM and logs in; on a
    // workgroup machine SSPI has no credentials for it (SEC_E_NO_CREDENTIALS). Either way
    // the result is a login or a typed error, never a hang or a panic.
    let cfg: Config = format!(
        "Server={server};Integrated Security=SSPI;ServerSPN=MSSQLSvc/no-such-host.invalid:1433"
    )
    .parse()
    .unwrap();
    match Client::connect(&cfg) {
        Ok(c) => c.close(),
        Err(e) => {
            let msg = e.to_string();
            eprintln!("override SPN: {msg}");
            assert!(matches!(
                e,
                dl_tds::Error::Sspi { .. } | dl_tds::Error::Server { .. }
            ));
        }
    }
}

#[cfg(not(windows))]
#[test]
fn integrated_login_is_unsupported() {
    let cfg: Config = "Server=localhost;Integrated Security=SSPI".parse().unwrap();
    match Client::connect(&cfg) {
        Err(dl_tds::Error::Unsupported(m)) => {
            assert_eq!(m, "integrated authentication needs Windows (SSPI)")
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("integrated authentication outside Windows"),
    }
    let _ = server();
}

/// Runs an unencrypted integrated login through a recording TCP proxy and checks the
/// packets the client sent: PRELOGIN, LOGIN7, SSPI answer(s) with data, then requests.
/// SQL Server sends SPNEGO's final token in a message of its own; answering it with an
/// empty SSPI message shifted every later response by one and the server reset the
/// connection.
#[cfg(windows)]
#[test]
fn login_exchange_on_the_wire() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    let Some(server) = server() else { return };
    if server.contains('\\') {
        eprintln!("note: named instance; skipping the proxy test");
        return;
    }
    let (host, port) = match server.split_once(',') {
        Some((h, p)) => (h.to_string(), p.trim().parse::<u16>().unwrap()),
        None => (server.clone(), 1433),
    };
    let host = match host.to_ascii_lowercase().as_str() {
        "." | "(local)" | "localhost" => "127.0.0.1".to_string(),
        _ => host,
    };
    let upstream = TcpStream::connect((host.as_str(), port)).expect("server reachable");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let sent: Arc<Mutex<Vec<u8>>> = Arc::default();
    let record = Arc::clone(&sent);
    std::thread::spawn(move || {
        let (client, _) = listener.accept().unwrap();
        let pump = |mut from: TcpStream, mut to: TcpStream, log: Option<Arc<Mutex<Vec<u8>>>>| {
            std::thread::spawn(move || {
                let mut buf = [0u8; 16384];
                loop {
                    let n = match from.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if let Some(l) = &log {
                        l.lock().unwrap().extend_from_slice(&buf[..n]);
                    }
                    if to.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                let _ = to.shutdown(std::net::Shutdown::Both);
            })
        };
        let a = pump(
            client.try_clone().unwrap(),
            upstream.try_clone().unwrap(),
            Some(record),
        );
        let b = pump(upstream, client, None);
        let _ = (a.join(), b.join());
    });

    let cfg: Config =
        format!("Server=127.0.0.1,{proxy_port};Integrated Security=SSPI;Encrypt=disable")
            .parse()
            .unwrap();
    let mut c = Client::connect(&cfg).expect("connect through the proxy");
    let v: i32 = c.query("SELECT 7", &[]).unwrap().rows()[0].get(0).unwrap();
    assert_eq!(v, 7);
    let name: String = c.query("SELECT SUSER_SNAME()", &[]).unwrap().rows()[0]
        .get(0)
        .unwrap();
    assert!(name.eq_ignore_ascii_case(&current_user()), "{name}");
    c.close();

    let bytes = sent.lock().unwrap().clone();
    if c_is_tls(&bytes) {
        eprintln!("note: the server forces encryption; packet sequence not checked");
        return;
    }
    // (type, payload length) of every packet the client sent.
    let mut packets = Vec::new();
    let mut rest = &bytes[..];
    while rest.len() >= 8 {
        let len = usize::from(u16::from_be_bytes([rest[2], rest[3]]));
        assert!(len >= 8 && len <= rest.len(), "bad packet header");
        packets.push((rest[0], len - 8));
        rest = &rest[len..];
    }
    eprintln!("client packets (type, payload bytes): {packets:?}");
    assert_eq!(packets[0].0, 0x12, "PRELOGIN");
    assert_eq!(packets[1].0, 0x10, "LOGIN7");
    let sspi: Vec<usize> = packets
        .iter()
        .filter(|p| p.0 == 0x11)
        .map(|p| p.1)
        .collect();
    assert!(!sspi.is_empty(), "no SSPI answer: {packets:?}");
    assert!(
        sspi.iter().all(|&n| n > 0),
        "empty SSPI message: {packets:?}"
    );
    // After the login: exactly the two batches.
    let after: Vec<u8> = packets
        .iter()
        .map(|p| p.0)
        .skip_while(|&t| t != 0x01)
        .collect();
    assert_eq!(after, [0x01, 0x01], "{packets:?}");
}

/// `true` if the client bytes after PRELOGIN are a TLS handshake (server forced it).
#[cfg(windows)]
fn c_is_tls(bytes: &[u8]) -> bool {
    let first = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    bytes
        .get(first..first + 9)
        .is_some_and(|p| p[0] == 0x12 && p[8] == 0x16)
}
