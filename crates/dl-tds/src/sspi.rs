//! Windows integrated authentication: SSPI "Negotiate" (Kerberos, or NTLM as fallback)
//! with the credentials of the current Windows account, as Microsoft.Data.SqlClient and
//! go-mssqldb's `winsspi` do.
//!
//! The first token goes into LOGIN7; each SSPI token the server answers with is passed to
//! [`Negotiate::next`] and the result is sent back as an SSPI message (packet type 0x11)
//! until the server sends LOGINACK.

use std::net::IpAddr;

use crate::Error;
use crate::config::Config;

#[cfg(windows)]
mod sys {
    pub(crate) use crate::sspi_windows::{Context, canonical_name, local_dns_name};
}

#[cfg(not(windows))]
mod sys {
    use crate::Error;

    /// No SSPI outside Windows: the type has no values, so `acquire` is the only entry.
    pub(crate) enum Context {}

    impl Context {
        pub(crate) fn acquire() -> Result<Context, Error> {
            Err(Error::Unsupported(
                "integrated authentication needs Windows (SSPI)",
            ))
        }
        pub(crate) fn initialize(&mut self, _target: &str) -> Result<Vec<u8>, Error> {
            match *self {}
        }
        pub(crate) fn next(&mut self, _input: &[u8]) -> Result<Vec<u8>, Error> {
            match *self {}
        }
        pub(crate) fn is_done(&self) -> bool {
            match *self {}
        }
    }

    pub(crate) fn local_dns_name() -> Option<String> {
        None
    }

    pub(crate) fn canonical_name(_host: &str) -> Option<String> {
        None
    }
}

/// An SSPI Negotiate client context with the current user's credentials.
pub(crate) struct Negotiate(sys::Context);

impl Negotiate {
    /// Acquires the current user's outbound credentials.
    ///
    /// # Errors
    /// [`Error::Unsupported`] outside Windows, [`Error::Sspi`] if SSPI refuses.
    pub(crate) fn acquire() -> Result<Negotiate, Error> {
        sys::Context::acquire().map(Negotiate)
    }

    /// The first token, for the service principal name `target`.
    pub(crate) fn initialize(&mut self, target: &str) -> Result<Vec<u8>, Error> {
        self.0.initialize(target)
    }

    /// The answer to a server token.
    pub(crate) fn next(&mut self, input: &[u8]) -> Result<Vec<u8>, Error> {
        self.0.next(input)
    }

    /// `true` once SSPI reported the context complete.
    pub(crate) fn is_done(&self) -> bool {
        self.0.is_done()
    }
}

/// `true` for the server names that mean "this computer": `.`, `(local)`, `localhost`
/// and loopback addresses.
pub(crate) fn is_local_host(host: &str) -> bool {
    host == "."
        || host.eq_ignore_ascii_case("(local)")
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The SPN to authenticate to, for this machine.
pub(crate) fn target_name(cfg: &Config) -> String {
    spn(cfg, sys::local_dns_name, sys::canonical_name)
}

/// SPN rules (Microsoft.Data.SqlClient's): the `ServerSPN` override if set; otherwise
/// `MSSQLSvc/<name>:<port or instance>` where `<name>` is the machine's DNS name for a
/// local server (`local`), the canonical DNS name of the host (`canonical`) for a host
/// name, and the address itself for an IP address.
fn spn(
    cfg: &Config,
    local: impl FnOnce() -> Option<String>,
    canonical: impl FnOnce(&str) -> Option<String>,
) -> String {
    if let Some(s) = cfg.server_spn() {
        return s.to_string();
    }
    let host = cfg.host();
    let name = if is_local_host(host) {
        local().unwrap_or_else(|| "localhost".to_string())
    } else if host.parse::<IpAddr>().is_ok() {
        host.to_string()
    } else {
        canonical(host)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| host.to_string())
    };
    cfg.spn_for(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(s: &str) -> Config {
        s.parse().unwrap()
    }

    fn local() -> Option<String> {
        Some("pc1.corp.example".into())
    }

    fn canon(h: &str) -> Option<String> {
        Some(format!("{}.corp.example", h.to_ascii_lowercase()))
    }

    #[test]
    fn local_hosts() {
        for h in [
            ".",
            "(local)",
            "(LOCAL)",
            "localhost",
            "LocalHost",
            "127.0.0.1",
            "127.0.0.2",
            "::1",
        ] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["srv", "10.0.0.5", "localhost.example", "", "::2"] {
            assert!(!is_local_host(h), "{h}");
        }
    }

    #[test]
    fn spn_rules() {
        let s = |c: &str| spn(&cfg(c), local, canon);
        for c in [
            "Server=.;Integrated Security=SSPI",
            "Server=(local);Integrated Security=SSPI",
            "Server=localhost;Integrated Security=SSPI",
            "Server=127.0.0.1;Integrated Security=SSPI",
            "sqlserver://localhost?trusted_connection=true",
        ] {
            assert_eq!(s(c), "MSSQLSvc/pc1.corp.example:1433", "{c}");
        }
        assert_eq!(
            s("Server=.\\SQLEXPRESS;Integrated Security=SSPI"),
            "MSSQLSvc/pc1.corp.example:SQLEXPRESS"
        );
        assert_eq!(
            s("Server=localhost,14333;Trusted_Connection=yes"),
            "MSSQLSvc/pc1.corp.example:14333"
        );
        assert_eq!(
            s("Server=SRV01;Integrated Security=SSPI"),
            "MSSQLSvc/srv01.corp.example:1433"
        );
        assert_eq!(
            s("sqlserver://srv01/INST"),
            "MSSQLSvc/srv01.corp.example:INST"
        );
        // IP addresses are used as they are.
        assert_eq!(
            s("Server=10.0.0.5,1500;Integrated Security=SSPI"),
            "MSSQLSvc/10.0.0.5:1500"
        );
        // Override.
        assert_eq!(
            s("Server=.;Integrated Security=SSPI;ServerSPN=MSSQLSvc/other:1433"),
            "MSSQLSvc/other:1433"
        );
        // Lookups that fail keep the name given.
        let c = cfg("Server=srv01;Integrated Security=SSPI");
        assert_eq!(spn(&c, || None, |_| None), "MSSQLSvc/srv01:1433");
        let c = cfg("Server=.;Integrated Security=SSPI");
        assert_eq!(spn(&c, || None, |_| None), "MSSQLSvc/localhost:1433");
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_outside_windows() {
        match Negotiate::acquire() {
            Err(Error::Unsupported(m)) => {
                assert_eq!(m, "integrated authentication needs Windows (SSPI)")
            }
            Err(e) => panic!("{e}"),
            Ok(_) => panic!("SSPI outside Windows"),
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_negotiate_first_token() {
        // Acquiring the current user's credentials and producing the first Negotiate token
        // needs no server and no network (SPNEGO NegTokenInit, or a raw NTLM NEGOTIATE).
        let mut n = Negotiate::acquire().expect("AcquireCredentialsHandleW");
        let spn = target_name(&cfg("Server=localhost;Integrated Security=SSPI"));
        assert!(spn.starts_with("MSSQLSvc/"), "{spn}");
        assert!(spn.ends_with(":1433"), "{spn}");
        let t = n.initialize(&spn).expect("InitializeSecurityContextW");
        assert!(!t.is_empty());
        assert!(
            t[0] == 0x60 || t.starts_with(b"NTLMSSP\0"),
            "{:02x?}",
            &t[..4]
        );
        assert!(!n.is_done());
        // A garbage challenge is an SSPI error, not a panic, and names no token bytes.
        let e = n.next(&[0xFF; 16]).unwrap_err();
        assert!(matches!(e, Error::Sspi { .. }), "{e}");
        assert!(e.to_string().contains("InitializeSecurityContextW"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_name() {
        let n = sys::local_dns_name().expect("GetComputerNameExW");
        assert!(!n.is_empty());
        assert!(!n.contains('\0'));
    }

    #[test]
    fn error_messages() {
        let e = Error::Sspi {
            function: "InitializeSecurityContextW",
            status: 0x8009_030C_u32 as i32,
        };
        assert_eq!(
            e.to_string(),
            "integrated authentication failed: InitializeSecurityContextW returned 0x8009030C (SEC_E_LOGON_DENIED)"
        );
        let e = Error::Sspi {
            function: "AcquireCredentialsHandleW",
            status: 0x8009_9999_u32 as i32,
        };
        assert_eq!(
            e.to_string(),
            "integrated authentication failed: AcquireCredentialsHandleW returned 0x80099999"
        );
    }
}
