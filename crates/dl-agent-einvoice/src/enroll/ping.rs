//! Connection tests for the enroll flow: a read-only ping, and the transport fallback for
//! old SQL Servers (`enroll_transport.go`).

use std::time::Duration;

use crate::dsn::{Dsn, ENCRYPT_DISABLE, Transport};

const PING_TIMEOUT: Duration = Duration::from_secs(10);

/// A failed connection test. `tls` is set when the TLS handshake failed
/// (`dl_tds::Error::is_tls`), which is what makes an older transport worth trying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingError {
    /// What went wrong, shown to the operator.
    pub message: String,
    /// The failure happened in the TLS handshake.
    pub tls: bool,
}

impl std::fmt::Display for PingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// A connection test.
pub type Ping<'a> = &'a dyn Fn(&Dsn) -> Result<(), PingError>;

/// `PingDSN`: connects and runs a read-only check; always succeeds under
/// `MMEI_AGENT_MOCK=1`, so CI and dev boxes without SQL Server exercise the flow.
pub fn ping_dsn(dsn: &Dsn, mock: bool) -> Result<(), PingError> {
    if mock {
        return Ok(());
    }
    let fail = |e: dl_tds::Error| PingError {
        tls: e.is_tls(),
        message: format!("agent: ping FO source: {e}"),
    };
    let cfg: dl_tds::Config = dsn.to_string().parse().map_err(fail)?;
    let mut c = dl_tds::Client::connect(&cfg.with_connect_timeout(PING_TIMEOUT)).map_err(fail)?;
    c.set_timeout(Some(PING_TIMEOUT)).map_err(fail)?;
    c.ping().map_err(fail)?;
    c.close();
    Ok(())
}

/// `pingNegotiated`: pings `dsn` and, when the driver-default transport fails in the TLS
/// handshake, walks the legacy ladder (TLS 1.0, then no encryption) until one gets past
/// it. The unencrypted step is tried only where dl-agent's core allows clear text
/// ([`dl_agent::sql::transport::clear_allowed`]: Windows login or a server on this
/// computer), so a man in the middle cannot push a SQL login to another host into clear
/// text; such a site pins `encrypt=disable` by hand (or carries it over from the Go agent). Returns the DSN to seal: the first whose ping succeeded or failed for a non-TLS
/// reason (the handshake worked; that error, login or database, is the one worth
/// showing). When every step fails in TLS the original DSN and error come back. A DSN
/// that already carries an explicit transport is pinged as is.
pub fn ping_negotiated(dsn: &Dsn, ping: Ping<'_>) -> (Dsn, Result<(), PingError>) {
    let first = ping(dsn);
    let tls_failure = matches!(&first, Err(e) if e.tls);
    if !tls_failure || !dsn.transport().is_default() {
        return (dsn.clone(), first);
    }
    let clear_ok = clear_allowed(dsn);
    for t in Transport::legacy_ladder() {
        if t.encrypt == ENCRYPT_DISABLE && !clear_ok {
            continue;
        }
        let alt = dsn.clone().with_transport(&t);
        let r = ping(&alt);
        if !matches!(&r, Err(e) if e.tls) {
            return (alt, r);
        }
    }
    (dsn.clone(), first)
}

/// The core rule for an unencrypted fallback, applied to `dsn` (unparsable → not allowed).
fn clear_allowed(dsn: &Dsn) -> bool {
    let here = std::env::var("COMPUTERNAME").ok();
    dsn.to_string()
        .parse::<dl_tds::Config>()
        .is_ok_and(|cfg| dl_agent::sql::transport::clear_allowed(&cfg, here.as_deref()))
}
