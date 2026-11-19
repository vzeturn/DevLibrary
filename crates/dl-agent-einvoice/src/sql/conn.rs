//! One session per SMILE database, reached one of two ways:
//!
//! - **sealed** (default): the connection string sealed in `config.enc` at enrolment. The
//!   dl-agent transport ladder (as sealed → TLS 1.0 → unencrypted, PROTOCOL.md §9.4) runs at
//!   run time too, so a server that starts resetting the TLS handshake after enrolment is
//!   still reached. The rung saved at enrolment (`tlsmin=1.0`, `encrypt=disable`) is the top
//!   of the ladder, so a site never climbs back above what enrolment found. Each step down is
//!   kept for [`Sql::take_step_downs`](super::Sql::take_step_downs) and reported once.
//! - **site**: a database declared in dl-agent's `sql.json` (`sql-setup` / `sql-add`: Windows
//!   or SQL authentication, pinned transport), used when the module's `sql_source` setting is
//!   `site`. dl-agent's pool walks its own ladder and reports `sql_unencrypted` itself.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dl_agent::sql::SqlPool;
use dl_agent::sql::transport::{self, Rung};
use dl_json::Value;
use dl_tds::{Client, Config, Encrypt, TlsVersion};

/// Handshake-level failures in a row on a settled rung before the ladder is walked again
/// below it. One reset is an outage; three in a row look like a server that changed.
pub(crate) const REWALK_AFTER: u32 = 3;

/// The longest connect wait, whatever the caller's timeout.
const CONNECT_TIMEOUT_MAX: Duration = Duration::from_secs(30);

/// A step down the ladder at run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDown {
    /// The database (`SMILE_FO`, `SMILE_POS`, …).
    pub database: String,
    /// The rung that failed.
    pub from: Rung,
    /// The rung that connected.
    pub to: Rung,
    /// Why `from` failed.
    pub cause: String,
}

/// Where a connection stands on the ladder.
#[derive(Debug)]
pub(crate) struct Ladder {
    rungs: Vec<Rung>,
    at: usize,
    settled: bool,
    streak: u32,
}

impl Ladder {
    pub(crate) fn new(rungs: Vec<Rung>) -> Ladder {
        let rungs = if rungs.is_empty() {
            vec![Rung::AsConfigured]
        } else {
            rungs
        };
        Ladder {
            rungs,
            at: 0,
            settled: false,
            streak: 0,
        }
    }

    /// The rung that last connected; `None` before the first connection.
    pub(crate) fn rung(&self) -> Option<Rung> {
        self.settled.then(|| self.rungs[self.at])
    }

    /// Connects with `attempt`. Before the first connection the ladder is walked from the
    /// top; once settled, the same rung is used, and only after [`REWALK_AFTER`]
    /// handshake-level failures in a row is the ladder walked again below it. Returns the
    /// outcome and the step down to report, if the rung changed.
    pub(crate) fn connect<T>(
        &mut self,
        mut attempt: impl FnMut(Rung) -> Result<T, dl_tds::Error>,
    ) -> (Result<T, dl_tds::Error>, Option<Step>) {
        let from = self.at;
        let mut start = self.at;
        let mut cause = String::new();
        if self.settled {
            match attempt(self.rungs[self.at]) {
                Ok(c) => {
                    self.streak = 0;
                    return (Ok(c), None);
                }
                Err(e) => {
                    let lower = self.at + 1 < self.rungs.len() && transport::should_step_down(&e);
                    self.streak = if lower { self.streak + 1 } else { 0 };
                    if self.streak < REWALK_AFTER {
                        return (Err(e), None);
                    }
                    self.streak = 0;
                    cause = e.to_string();
                    start = self.at + 1;
                }
            }
        }
        let rungs = self.rungs.clone();
        let (r, at) = transport::walk(&rungs, start, |rung| {
            let r = attempt(rung);
            if let Err(e) = &r
                && transport::should_step_down(e)
            {
                cause = e.to_string();
            }
            r
        });
        if r.is_err() {
            return (r, None);
        }
        self.at = at;
        self.settled = true;
        let step = (at != from).then(|| (rungs[from], rungs[at], cause));
        (r, step)
    }
}

/// A rung change: from, to, and the failure that caused it.
type Step = (Rung, Rung, String);

/// A connection reached through the sealed DSN.
struct Sealed {
    dsn: String,
    state: Mutex<SealedState>,
}

#[derive(Default)]
struct SealedState {
    client: Option<Client>,
    ladder: Option<Ladder>,
    steps: Vec<StepDown>,
}

/// A connection to one SMILE database.
pub(crate) struct Conn {
    database: String,
    kind: Kind,
}

enum Kind {
    Sealed(Box<Sealed>),
    Site(Arc<SqlPool>),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Conn {
    /// A connection through the sealed DSN (`fallback` names it when the DSN has no database).
    pub(crate) fn sealed(dsn: &str, fallback: &str) -> Conn {
        let database = dsn
            .parse::<Config>()
            .ok()
            .map(|c| c.database().to_owned())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| fallback.to_owned());
        Conn {
            database,
            kind: Kind::Sealed(Box::new(Sealed {
                dsn: dsn.to_owned(),
                state: Mutex::new(SealedState::default()),
            })),
        }
    }

    /// A connection through dl-agent's pool for `database` (`sql.json`).
    pub(crate) fn site(database: &str, pool: Arc<SqlPool>) -> Conn {
        Conn {
            database: database.to_owned(),
            kind: Kind::Site(pool),
        }
    }

    /// The database name.
    pub(crate) fn database(&self) -> &str {
        &self.database
    }

    /// Runs `f` on a live session, connecting first if needed. A session that broke (I/O
    /// error, protocol error) is dropped so the next call reconnects, down the ladder when
    /// the server resets the handshake.
    pub(crate) fn with<T>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&mut Client) -> Result<T, dl_tds::Error>,
    ) -> Result<T, dl_tds::Error> {
        match &self.kind {
            Kind::Sealed(s) => s.with(&self.database, timeout, f),
            Kind::Site(pool) => {
                match pool.with(|c| Ok(c.set_timeout(Some(timeout)).and_then(|()| f(c)))) {
                    Ok(r) => r,
                    Err(e) => Err(dl_tds::Error::Io(io::Error::other(format!(
                        "{} (sql.json): {e}",
                        self.database
                    )))),
                }
            }
        }
    }

    /// Step downs since the last call (sealed connections only).
    pub(crate) fn take_step_downs(&self) -> Vec<StepDown> {
        match &self.kind {
            Kind::Sealed(s) => std::mem::take(&mut lock(&s.state).steps),
            Kind::Site(_) => Vec::new(),
        }
    }

    /// How this connection reaches SQL Server, for status and the self-test.
    pub(crate) fn describe(&self) -> Value {
        let mut m = dl_json::Map::new();
        m.insert("database", self.database.as_str());
        match &self.kind {
            Kind::Sealed(s) => {
                m.insert("source", "config.enc");
                let cfg = s.dsn.parse::<Config>().ok();
                let auth = match &cfg {
                    Some(c) if c.integrated_auth() => "windows",
                    Some(_) => "sql",
                    None => "unknown",
                };
                m.insert("auth", auth);
                m.insert(
                    "sealed_transport",
                    cfg.as_ref().map_or("unparsable", sealed_transport),
                );
                let rung = lock(&s.state).ladder.as_ref().and_then(Ladder::rung);
                m.insert("rung", rung.map_or(Value::Null, |r| Value::from(r.name())));
            }
            Kind::Site(pool) => {
                m.insert("source", "sql.json");
                let t = pool
                    .stats()
                    .get("transport")
                    .cloned()
                    .unwrap_or(Value::Null);
                let auth = t.get("auth").cloned().unwrap_or_else(|| "sql.json".into());
                m.insert("auth", auth);
                m.insert("rung", t.get("rung").cloned().unwrap_or(Value::Null));
            }
        }
        Value::Object(m)
    }
}

/// The transport enrolment sealed into the DSN.
fn sealed_transport(c: &Config) -> &'static str {
    match c.encrypt() {
        Encrypt::Disabled => "encrypt=disable (unencrypted)",
        Encrypt::Off if c.tls_min() <= TlsVersion::Tls10 => "tlsmin=1.0 (login over TLS 1.0+)",
        Encrypt::Off => "default (login over TLS 1.2+)",
        _ => "encrypt=true (whole session over TLS)",
    }
}

impl Sealed {
    fn with<T>(
        &self,
        database: &str,
        timeout: Duration,
        f: impl FnOnce(&mut Client) -> Result<T, dl_tds::Error>,
    ) -> Result<T, dl_tds::Error> {
        let mut st = lock(&self.state);
        if st.client.as_ref().is_none_or(Client::is_broken) {
            st.client = None;
            let cfg: Config = self.dsn.parse()?;
            let cfg = cfg.with_connect_timeout(timeout.min(CONNECT_TIMEOUT_MAX));
            let st = &mut *st;
            let ladder = st
                .ladder
                .get_or_insert_with(|| Ladder::new(transport::rungs(&cfg, None)));
            let (r, step) = ladder.connect(|rung| Client::connect(&rung.apply(&cfg)));
            if let Some((from, to, cause)) = step {
                st.steps.push(StepDown {
                    database: database.to_owned(),
                    from,
                    to,
                    cause,
                });
            }
            st.client = Some(r?);
        }
        let Some(c) = st.client.as_mut() else {
            unreachable!("connected above")
        };
        c.set_timeout(Some(timeout))?;
        let r = f(c);
        if c.is_broken() {
            st.client = None;
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() -> dl_tds::Error {
        dl_tds::Error::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset"))
    }
    fn refused() -> dl_tds::Error {
        dl_tds::Error::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "refused"))
    }
    const FULL: [Rung; 3] = [Rung::AsConfigured, Rung::Tls10, Rung::Unencrypted];

    #[test]
    fn first_connection_walks_down_and_reports_the_step() {
        let mut l = Ladder::new(FULL.to_vec());
        let mut tried = Vec::new();
        let (r, step) = l.connect(|rung| {
            tried.push(rung);
            if rung == Rung::AsConfigured {
                Err(reset())
            } else {
                Ok(())
            }
        });
        assert!(r.is_ok());
        assert_eq!(tried, [Rung::AsConfigured, Rung::Tls10]);
        let (from, to, cause) = step.expect("a step down");
        assert_eq!((from, to), (Rung::AsConfigured, Rung::Tls10));
        assert!(cause.contains("reset"), "{cause}");
        assert_eq!(l.rung(), Some(Rung::Tls10));
    }

    #[test]
    fn sealed_rung_that_works_reports_nothing() {
        let mut l = Ladder::new(FULL.to_vec());
        let (r, step) = l.connect(|_| Ok(()));
        assert!(r.is_ok());
        assert!(step.is_none());
        assert_eq!(l.rung(), Some(Rung::AsConfigured));
    }

    #[test]
    fn refused_dial_never_steps_down() {
        let mut l = Ladder::new(FULL.to_vec());
        let mut tried = 0;
        let (r, step) = l.connect(|_| -> Result<(), _> {
            tried += 1;
            Err(refused())
        });
        assert!(r.is_err());
        assert!(step.is_none());
        assert_eq!(tried, 1);
        assert_eq!(l.rung(), None, "nothing settled");
    }

    #[test]
    fn settled_rung_survives_one_reset_and_rewalks_after_a_streak() {
        let mut l = Ladder::new(FULL.to_vec());
        assert!(l.connect(|_| Ok(())).0.is_ok());
        // The server starts resetting the TLS handshake (as configured) later on.
        let server = |rung: Rung| {
            if rung == Rung::AsConfigured {
                Err(reset())
            } else {
                Ok(rung)
            }
        };
        for n in 1..REWALK_AFTER {
            let (r, step) = l.connect(server);
            assert!(r.is_err(), "failure {n} is an outage");
            assert!(step.is_none());
            assert_eq!(l.rung(), Some(Rung::AsConfigured));
        }
        let (r, step) = l.connect(server);
        assert_eq!(r.ok(), Some(Rung::Tls10));
        let (from, to, _) = step.expect("step reported");
        assert_eq!((from, to), (Rung::AsConfigured, Rung::Tls10));
        assert_eq!(l.rung(), Some(Rung::Tls10));
        // Kept from now on.
        let (r, step) = l.connect(server);
        assert_eq!(r.ok(), Some(Rung::Tls10));
        assert!(step.is_none());
    }

    #[test]
    fn other_failures_break_the_streak() {
        let mut l = Ladder::new(FULL.to_vec());
        assert!(l.connect(|_| Ok(())).0.is_ok());
        for _ in 0..REWALK_AFTER - 1 {
            assert!(l.connect(|_| -> Result<(), _> { Err(reset()) }).0.is_err());
        }
        assert!(
            l.connect(|_| -> Result<(), _> { Err(refused()) })
                .0
                .is_err()
        );
        let (r, step) = l.connect(|_| -> Result<(), _> { Err(reset()) });
        assert!(r.is_err());
        assert!(step.is_none(), "streak restarted");
        assert_eq!(l.rung(), Some(Rung::AsConfigured));
    }

    #[test]
    fn bottom_rung_never_rewalks() {
        let mut l = Ladder::new(vec![Rung::AsConfigured]);
        assert!(l.connect(|_| Ok(())).0.is_ok());
        for _ in 0..REWALK_AFTER * 2 {
            let (r, step) = l.connect(|_| -> Result<(), _> { Err(reset()) });
            assert!(r.is_err());
            assert!(step.is_none());
        }
    }

    #[test]
    fn failed_walk_keeps_the_rung() {
        let mut l = Ladder::new(FULL.to_vec());
        let (r, _) = l.connect(|_| -> Result<(), _> { Err(reset()) });
        assert!(r.is_err());
        assert_eq!(l.rung(), None);
        // The next attempt starts from the top again.
        let mut first = None;
        let _ = l.connect(|rung| {
            first.get_or_insert(rung);
            Ok(())
        });
        assert_eq!(first, Some(Rung::AsConfigured));
    }

    #[test]
    fn describe_names_auth_and_the_sealed_rung() {
        let c = Conn::sealed(
            "sqlserver://smile:p@127.0.0.1:1?database=SMILE_FO&tlsmin=1.0",
            "FO",
        );
        let d = c.describe();
        assert_eq!(d["database"], "SMILE_FO");
        assert_eq!(d["auth"], "sql");
        assert_eq!(d["source"], "config.enc");
        assert!(
            d["sealed_transport"]
                .as_str()
                .unwrap()
                .starts_with("tlsmin=1.0")
        );
        assert!(d["rung"].is_null());
        let w = Conn::sealed("sqlserver://127.0.0.1?database=SMILE_POS", "POS");
        assert_eq!(w.describe()["auth"], "windows");
        let off = Conn::sealed("sqlserver://u:p@h?database=X&encrypt=disable", "FO");
        assert!(
            off.describe()["sealed_transport"]
                .as_str()
                .unwrap()
                .starts_with("encrypt=disable")
        );
        assert_eq!(Conn::sealed("", "FO").database(), "FO");
    }
}
