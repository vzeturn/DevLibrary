//! Connecting to a SQL Server that only speaks TLS 1.0, as SQL Server 2008 without the
//! TLS 1.2 update does. Runs when `DL_TDS_TEST_TLS10_URL` is a login URL (without
//! `encrypt` or `tlsmin`) for such a server, e.g. SQL Server 2022 on Linux with
//! `network.tlsprotocols = 1.0` and `network.tlsciphers = DEFAULT:@SECLEVEL=0`; skipped
//! otherwise.

use dl_tds::{Client, Config, Encrypt, TlsVersion};

fn base() -> Option<Config> {
    let url = std::env::var("DL_TDS_TEST_TLS10_URL")
        .ok()
        .filter(|u| !u.is_empty());
    let Some(url) = url else {
        eprintln!("note: skipping, DL_TDS_TEST_TLS10_URL is not set");
        return None;
    };
    Some(url.parse().expect("DL_TDS_TEST_TLS10_URL"))
}

fn select_one(c: &mut Client) -> i32 {
    let rs = c.query("SELECT 1", &[]).unwrap();
    rs.rows()[0].get::<i32>(0).unwrap()
}

#[test]
fn default_transport_fails_in_the_tls_handshake() {
    let Some(cfg) = base() else { return };
    let e = Client::connect(&cfg).expect_err("TLS 1.2 against a TLS 1.0-only server");
    assert!(e.is_tls(), "want a TLS error, got {e:?}");
}

#[test]
fn tls10_minimum_logs_in() {
    let Some(cfg) = base() else { return };
    let mut c = Client::connect(&cfg.with_tls_min(TlsVersion::Tls10)).unwrap();
    assert_eq!(select_one(&mut c), 1);
    // Default `encrypt`: only the login was encrypted.
    assert!(!c.is_encrypted());
}

#[test]
fn tls10_minimum_from_the_url() {
    let Some(cfg) = base() else { return };
    let url = std::env::var("DL_TDS_TEST_TLS10_URL").unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let cfg2: Config = format!("{url}{sep}tlsmin=1.0").parse().unwrap();
    assert_eq!(cfg2.tls_min(), TlsVersion::Tls10);
    let mut c = Client::connect(&cfg2).unwrap();
    assert_eq!(select_one(&mut c), 1);
    drop(cfg);
}

#[test]
fn whole_session_over_tls10() {
    let Some(cfg) = base() else { return };
    let mut c = Client::connect(
        &cfg.with_tls_min(TlsVersion::Tls10)
            .with_encrypt(Encrypt::Required),
    )
    .unwrap();
    assert!(c.is_encrypted());
    assert_eq!(select_one(&mut c), 1);
}

#[test]
fn unencrypted_login_needs_no_tls() {
    let Some(cfg) = base() else { return };
    let mut c = Client::connect(&cfg.with_encrypt(Encrypt::Disabled)).unwrap();
    assert!(!c.is_encrypted());
    assert_eq!(select_one(&mut c), 1);
}

#[test]
fn login_failure_is_not_a_tls_error() {
    let Some(cfg) = base() else { return };
    let e = Client::connect(
        &cfg.credentials("sa", "definitely-wrong")
            .with_tls_min(TlsVersion::Tls10),
    )
    .expect_err("wrong password");
    assert!(
        !e.is_tls() && e.is_server(),
        "want a login error, got {e:?}"
    );
}
