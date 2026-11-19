//! The runtime SQL Server connection of the least-privilege `pda_sync` login (port of
//! `Bootstrap/AgentSqlConnection.cs`), as a `dl_tds::Config` with the settings the C#
//! `SqlConnectionStringBuilder` produces with Microsoft.Data.SqlClient 5.2:
//!
//! | SqlClient | dl-tds |
//! |---|---|
//! | `Encrypt` (default `Mandatory` since SqlClient 4.0) | [`Encrypt::Required`](dl_tds::Encrypt::Required) |
//! | `TrustServerCertificate=true` | `with_trust_server_certificate(true)` |
//! | `ConnectTimeout=10` | `with_connect_timeout(10 s)` |
//! | `ApplicationIntent=ReadWrite` | the default (no read-only intent) |
//! | `ApplicationName=PdaSyncAgent` | `with_app_name("PdaSyncAgent")` |
//! | `DataSource` `host\instance,port`, `tcp:` prefix, `.` / `(local)` | [`data_source`] |
//!
//! SqlClient's command timeout (30 s per command) is the caller's business
//! (`dl_tds::Client::set_timeout`), as is SqlClient's login-phase timeout, which dl-tds
//! bounds per connect attempt.

use super::errors::{AgentError, codes};
use super::identity::AgentIdentity;
use crate::security::{SecretStore, names};
use std::time::Duration;

/// `ApplicationName` of the runtime connection.
pub const APP_NAME: &str = "PdaSyncAgent";
/// The runtime SQL login.
pub const LOGIN: &str = "pda_sync";
/// `ConnectTimeout` of the runtime connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A SqlClient `Data Source` (`host`, `host\instance`, `host,port`, `tcp:host\instance,port`,
/// `.`, `(local)`) applied to a fresh `dl_tds::Config`.
pub fn data_source(instance: &str) -> dl_tds::Config {
    let mut s = instance.trim();
    for prefix in ["tcp:", "TCP:", "Tcp:"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
        }
    }
    let (server, port) = match s.rsplit_once(',') {
        Some((h, p)) => (h.trim(), p.trim().parse::<u16>().ok()),
        None => (s, None),
    };
    let (host, inst) = match server.split_once('\\') {
        Some((h, i)) => (h.trim(), Some(i.trim())),
        None => (server, None),
    };
    let host = if host.is_empty() || host == "." || host.eq_ignore_ascii_case("(local)") {
        "localhost"
    } else {
        host
    };
    let mut c = dl_tds::Config::new(host);
    if let Some(i) = inst {
        c = c.with_instance(i);
    }
    if let Some(p) = port {
        c = c.with_port(p);
    }
    c
}

/// The connection of `pda_sync` to `database` on `instance` (the enroll preflight uses it
/// before an identity exists). The password is registered with the secret scrubber.
pub fn agent_connection(instance: &str, database: &str, password: &str) -> dl_tds::Config {
    crate::reporting::scrubber::register(Some(password));
    data_source(instance)
        .with_database(database)
        .credentials(LOGIN, password)
        .with_encrypt(dl_tds::Encrypt::Required)
        .with_trust_server_certificate(true)
        .with_connect_timeout(CONNECT_TIMEOUT)
        .with_app_name(APP_NAME)
}

/// The runtime connection of an enrolled agent: its SQL target and the stored `pda_sync`
/// password.
pub fn agent_connection_for(
    identity: &AgentIdentity,
    secrets: &dyn SecretStore,
) -> Result<dl_tds::Config, AgentError> {
    let (Some(instance), Some(database)) = (&identity.sql_instance, &identity.sql_database) else {
        return Err(AgentError::new(
            codes::NOT_ENROLLED,
            "Agent has no SQL target recorded; re-run enroll.",
        ));
    };
    let password = secrets
        .load(names::SQL_PASSWORD)
        .map_err(|e| AgentError::with_source(codes::KEY_STORE_FAILED, e.to_string(), e))?
        .ok_or_else(|| {
            AgentError::new(
                codes::REENROLL_REQUIRED,
                "The pda_sync password is missing; re-run enroll.",
            )
        })?;
    Ok(agent_connection(
        instance,
        database,
        &String::from_utf8_lossy(&password),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_sources_parse_like_sqlclient() {
        let c = data_source(r".\SQLEXPRESS");
        assert_eq!(
            (c.host(), c.instance(), c.port()),
            ("localhost", Some("SQLEXPRESS"), None)
        );
        let c = data_source(r"tcp:SQLBOX\POS,1444");
        assert_eq!(
            (c.host(), c.instance(), c.port()),
            ("SQLBOX", Some("POS"), Some(1444))
        );
        let c = data_source("(local)");
        assert_eq!((c.host(), c.instance()), ("localhost", None));
        let c = data_source("10.0.0.5,14333");
        assert_eq!((c.host(), c.port()), ("10.0.0.5", Some(14333)));
    }

    #[test]
    fn the_runtime_connection_matches_the_csharp_builder() {
        let c = agent_connection(r"SQLBOX\POS", "SMILE_POS", "pw-1234-secret");
        assert_eq!(c.user(), "pda_sync");
        assert_eq!(c.password(), "pw-1234-secret");
        assert_eq!(c.database(), "SMILE_POS");
        assert_eq!(c.app_name(), "PdaSyncAgent");
        assert_eq!(c.encrypt(), dl_tds::Encrypt::Required);
        assert!(c.trust_server_certificate());
        assert!(!c.read_only_intent());
        assert_eq!(c.connect_timeout(), Duration::from_secs(10));
        assert_eq!(crate::reporting::scrub("x pw-1234-secret y"), "x *** y");
    }
}
