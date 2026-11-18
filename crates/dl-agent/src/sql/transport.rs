//! Transport ladder for old SQL Servers (PROTOCOL.md §9.4).
//!
//! A pre-TLS-1.2 server fails the default handshake; SQL Server 2008 R2 RTM even completes a
//! TLS 1.0 handshake and then resets the connection. When the connection string leaves
//! encryption at its default (`encrypt=false`: login only), the agent steps down
//! as configured → TLS 1.0 minimum → unencrypted, on a handshake failure or on the server
//! dropping an established connection, never on a refused or timed-out dial or on an answer
//! from SQL Server itself (login, database). The first rung that connects is kept for the
//! life of the process and reported; an explicit `encrypt=true` / `strict` / `disable`
//! never steps down.

use std::io;

use dl_tds::{Config, Encrypt, TlsVersion};

/// One way of reaching the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// The connection string as written.
    AsConfigured,
    /// TLS 1.0 allowed (pre-2016 servers without the TLS 1.2 update).
    Tls10,
    /// No TLS at all, credentials included: last resort, reported as `sql_unencrypted`.
    Unencrypted,
}

impl Rung {
    /// Name used in `sql.json` (`"transport"`) and in stats.
    pub fn name(self) -> &'static str {
        match self {
            Rung::AsConfigured => "as_configured",
            Rung::Tls10 => "tls1.0",
            Rung::Unencrypted => "unencrypted",
        }
    }

    /// Parses a `sql.json` `"transport"` value (`auto` = `None`).
    ///
    /// # Errors
    /// The accepted values, for an unknown one.
    pub fn parse(s: &str) -> Result<Option<Rung>, String> {
        match s {
            "auto" => Ok(None),
            "as_configured" => Ok(Some(Rung::AsConfigured)),
            "tls1.0" => Ok(Some(Rung::Tls10)),
            "unencrypted" => Ok(Some(Rung::Unencrypted)),
            _ => Err("transport must be auto, as_configured, tls1.0 or unencrypted".into()),
        }
    }

    /// `base` adjusted for this rung.
    pub fn apply(self, base: &Config) -> Config {
        match self {
            Rung::AsConfigured => base.clone(),
            Rung::Tls10 => base.clone().with_tls_min(TlsVersion::Tls10),
            Rung::Unencrypted => base.clone().with_encrypt(Encrypt::Disabled),
        }
    }
}

/// The rungs to try for `base`, in order. `pin` (from `sql.json`) fixes one rung.
pub fn rungs(base: &Config, pin: Option<Rung>) -> Vec<Rung> {
    if let Some(p) = pin {
        return vec![p];
    }
    if base.encrypt() != Encrypt::Off {
        // Required / Strict demand encryption; Disabled already is the bottom rung.
        return vec![Rung::AsConfigured];
    }
    let mut out = vec![Rung::AsConfigured];
    if base.tls_min() > TlsVersion::Tls10 {
        out.push(Rung::Tls10);
    }
    out.push(Rung::Unencrypted);
    out
}

/// `true` when `e` happened below SQL Server's own answer and a lower rung may work: a TLS
/// handshake the server could not finish, or the server closing or resetting a connection it
/// had accepted. Never for certificate errors (stepping down would dodge the check) or a
/// client without TLS (macOS, Linux without OpenSSL): neither is the server's version.
pub fn should_step_down(e: &dl_tds::Error) -> bool {
    match e {
        dl_tds::Error::Tls(t) => match t {
            dl_tls::Error::Handshake { .. } => true,
            dl_tls::Error::Io(io) => !matches!(
                io.kind(),
                io::ErrorKind::TimedOut
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::ConnectionRefused
            ),
            _ => false,
        },
        dl_tds::Error::Io(io) => dropped(io),
        _ => false,
    }
}

fn dropped(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
    )
}

/// Tries `rungs[start..]` in order with `attempt` until one connects or fails for a reason a
/// lower rung cannot fix. Returns the outcome and the index of the last rung tried.
pub fn walk<T>(
    rungs: &[Rung],
    start: usize,
    mut attempt: impl FnMut(Rung) -> Result<T, dl_tds::Error>,
) -> (Result<T, dl_tds::Error>, usize) {
    let mut i = start.min(rungs.len().saturating_sub(1));
    loop {
        match attempt(rungs[i]) {
            Err(e) if i + 1 < rungs.len() && should_step_down(&e) => i += 1,
            r => return (r, i),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(cs: &str) -> Config {
        cs.parse().unwrap()
    }
    fn io(kind: io::ErrorKind) -> dl_tds::Error {
        dl_tds::Error::Io(io::Error::new(kind, "x"))
    }

    #[test]
    fn default_encryption_gets_the_full_ladder() {
        let c = cfg("Server=h;User Id=u;Password=p");
        assert_eq!(
            rungs(&c, None),
            [Rung::AsConfigured, Rung::Tls10, Rung::Unencrypted]
        );
        let low = cfg("Server=h;User Id=u;Password=p;TLSMin=1.0");
        assert_eq!(rungs(&low, None), [Rung::AsConfigured, Rung::Unencrypted]);
    }

    #[test]
    fn explicit_encryption_never_steps_down() {
        for cs in ["encrypt=true", "encrypt=strict", "encrypt=disable"] {
            let c = cfg(&format!("Server=h;User Id=u;Password=p;{cs}"));
            assert_eq!(rungs(&c, None), [Rung::AsConfigured], "{cs}");
        }
        let c = cfg("Server=h;User Id=u;Password=p");
        assert_eq!(rungs(&c, Some(Rung::Tls10)), [Rung::Tls10]);
    }

    #[test]
    fn rungs_change_only_the_transport() {
        let c = cfg("Server=h;Database=FO;User Id=u;Password=p");
        assert_eq!(Rung::Tls10.apply(&c).tls_min(), TlsVersion::Tls10);
        let plain = Rung::Unencrypted.apply(&c);
        assert_eq!(plain.encrypt(), Encrypt::Disabled);
        assert_eq!(plain.database(), "FO");
        assert_eq!(Rung::parse("auto"), Ok(None));
        assert_eq!(Rung::parse("tls1.0"), Ok(Some(Rung::Tls10)));
        assert!(Rung::parse("tls").is_err());
    }

    #[test]
    fn handshake_failure_and_drops_step_down_but_refusals_do_not() {
        assert!(should_step_down(&io(io::ErrorKind::ConnectionReset)));
        assert!(should_step_down(&io(io::ErrorKind::UnexpectedEof)));
        assert!(!should_step_down(&io(io::ErrorKind::ConnectionRefused)));
        assert!(!should_step_down(&io(io::ErrorKind::TimedOut)));
        assert!(!should_step_down(&dl_tds::Error::Timeout));
        let tls = |e| dl_tds::Error::Tls(e);
        assert!(should_step_down(&tls(dl_tls::Error::Handshake {
            code: 0x8009_0331,
            message: "no common algorithm".into()
        })));
        assert!(!should_step_down(&tls(dl_tls::Error::UntrustedCertificate)));
        assert!(!should_step_down(&tls(dl_tls::Error::NameMismatch)));
        assert!(!should_step_down(&tls(dl_tls::Error::Unsupported("none"))));
    }

    /// Field case 2026-11-17, SQL Server 2008 R2 RTM (10.50.1600): the default handshake
    /// fails, TLS 1.0 completes and the server resets; only the unencrypted rung works.
    #[test]
    fn sql_2008_r2_rtm_ends_unencrypted() {
        let c = cfg("Server=h;User Id=u;Password=p");
        let all = rungs(&c, None);
        let mut tried = Vec::new();
        let (r, at) = walk(&all, 0, |rung| {
            tried.push(rung);
            match rung {
                Rung::AsConfigured => Err(dl_tds::Error::Tls(dl_tls::Error::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "protocol version",
                )))),
                Rung::Tls10 => Err(io(io::ErrorKind::ConnectionReset)),
                Rung::Unencrypted => Ok("connected"),
            }
        });
        assert_eq!(r.unwrap(), "connected");
        assert_eq!(all[at], Rung::Unencrypted);
        assert_eq!(tried.len(), 3);
    }

    #[test]
    fn a_refused_dial_or_last_rung_failure_is_final() {
        let all = [Rung::AsConfigured, Rung::Tls10, Rung::Unencrypted];
        let mut n = 0;
        let (r, at) = walk(&all, 0, |_| -> Result<(), _> {
            n += 1;
            Err(io(io::ErrorKind::ConnectionRefused))
        });
        assert!(r.is_err());
        assert_eq!((n, at), (1, 0));
        let (r, at) = walk(&all, 2, |_| -> Result<(), _> {
            Err(io(io::ErrorKind::ConnectionReset))
        });
        assert!(r.is_err());
        assert_eq!(at, 2);
    }
}
