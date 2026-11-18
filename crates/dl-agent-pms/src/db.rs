//! The site's SQL Server connections: `databases.json` in the protected state directory
//! (`{"MM_PMS": "<connection string>", "SMILE_FO": …, "SMILE_POS": …}`) and one small
//! pool per database. Connection strings are never logged; errors name the database.
//!
//! The pool plays the part of Go's `database/sql`: at most [`MAX_OPEN_PER_DB`]
//! connections, idle ones reused, a connection left broken or inside a transaction
//! closed instead of reused, and an idle connection pinged before reuse after a minute
//! so a restarted server costs no failed op.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::Value;
use dl_tds::{Client, Config};

use crate::ctx::{Ctx, Done};
use crate::json;
use crate::wire;

/// The connection file name in the state directory.
pub const DB_CONFIG_FILE: &str = "databases.json";
/// MM_PMS.
pub const DB_PMS: &str = "MM_PMS";
/// SMILE_FO.
pub const DB_FO: &str = "SMILE_FO";
/// SMILE_POS.
pub const DB_POS: &str = "SMILE_POS";
/// Each pool's cap: the in-flight cap plus the schema reader.
pub const MAX_OPEN_PER_DB: usize = wire::INFLIGHT_CAP + 1;

const PING_AFTER_IDLE: Duration = Duration::from_secs(60);

/// A database failure.
#[derive(Debug)]
pub enum DbError {
    /// The driver or the server.
    Tds(dl_tds::Error),
    /// The context ended first.
    Done(Done),
}

impl DbError {
    /// The SQL Server error number, if the server raised one.
    pub fn number(&self) -> Option<i32> {
        match self {
            DbError::Tds(e) => e.number(),
            DbError::Done(_) => None,
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Tds(e) => e.fmt(f),
            DbError::Done(d) => d.fmt(f),
        }
    }
}

impl std::error::Error for DbError {}

impl From<dl_tds::Error> for DbError {
    fn from(e: dl_tds::Error) -> DbError {
        DbError::Tds(e)
    }
}

/// The statement timeout for `ctx`: its time left (at least 1 ms), `None` without a
/// deadline; an error when it is already done.
pub fn timeout_for(ctx: &Ctx) -> Result<Option<Duration>, DbError> {
    if let Some(d) = ctx.err() {
        return Err(DbError::Done(d));
    }
    Ok(ctx.remaining().map(|d| d.max(Duration::from_millis(1))))
}

struct Idle {
    c: Client,
    since: Instant,
}

#[derive(Default)]
struct State {
    idle: Vec<Idle>,
    open: usize,
    closed: bool,
}

/// One database's connections.
pub struct Pool {
    name: String,
    cfg: Config,
    max_open: usize,
    st: Mutex<State>,
    cv: Condvar,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool").field("name", &self.name).finish()
    }
}

/// A connection borrowed from a [`Pool`]; it goes back on drop.
pub struct Conn<'a> {
    pool: &'a Pool,
    c: Option<Client>,
}

impl Deref for Conn<'_> {
    type Target = Client;
    fn deref(&self) -> &Client {
        self.c.as_ref().expect("connection present until drop")
    }
}

impl DerefMut for Conn<'_> {
    fn deref_mut(&mut self) -> &mut Client {
        self.c.as_mut().expect("connection present until drop")
    }
}

impl Conn<'_> {
    /// Closes the connection instead of returning it (its session state is not reusable).
    pub fn discard(mut self) {
        if let Some(c) = self.c.take() {
            c.close();
        }
        self.pool.release_slot();
    }

    /// Runs `sql` as a batch with the context's timeout.
    pub fn batch(&mut self, ctx: &Ctx, sql: &str) -> Result<dl_tds::Results, DbError> {
        let t = timeout_for(ctx)?;
        self.set_timeout(t)?;
        Ok(Client::batch(self, sql)?)
    }

    /// Calls a procedure by name with the context's timeout.
    pub fn call(
        &mut self,
        ctx: &Ctx,
        procedure: &str,
        params: &[dl_tds::Param],
    ) -> Result<dl_tds::Results, DbError> {
        let t = timeout_for(ctx)?;
        self.set_timeout(t)?;
        Ok(Client::call(self, procedure, params)?)
    }

    /// Runs a parameterized query with the context's timeout.
    pub fn query(
        &mut self,
        ctx: &Ctx,
        sql: &str,
        params: &[dl_tds::Param],
    ) -> Result<dl_tds::Results, DbError> {
        let t = timeout_for(ctx)?;
        self.set_timeout(t)?;
        Ok(Client::query(self, sql, params)?)
    }
}

impl Drop for Conn<'_> {
    fn drop(&mut self) {
        let Some(c) = self.c.take() else { return };
        if c.is_broken() || c.in_transaction() {
            c.close();
            self.pool.release_slot();
            return;
        }
        let mut st = self.pool.lock();
        if st.closed {
            st.open -= 1;
            drop(st);
            c.close();
        } else {
            st.idle.push(Idle {
                c,
                since: Instant::now(),
            });
        }
        self.pool.cv.notify_one();
    }
}

impl Pool {
    /// A pool for `cfg`, opening at most `max_open` connections.
    pub fn new(name: &str, cfg: Config, max_open: usize) -> Pool {
        Pool {
            name: name.to_owned(),
            cfg,
            max_open: max_open.max(1),
            st: Mutex::new(State::default()),
            cv: Condvar::new(),
        }
    }

    /// The database name this pool serves.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn release_slot(&self) {
        let mut st = self.lock();
        st.open = st.open.saturating_sub(1);
        self.cv.notify_one();
    }

    /// Borrows a connection: an idle one, a new one under the cap, or the next one
    /// returned, waiting no longer than `ctx` allows.
    pub fn get(&self, ctx: &Ctx) -> Result<Conn<'_>, DbError> {
        loop {
            let mut st = self.lock();
            loop {
                if let Some(d) = ctx.err() {
                    return Err(DbError::Done(d));
                }
                if st.closed {
                    return Err(DbError::Tds(dl_tds::Error::Broken));
                }
                if !st.idle.is_empty() || st.open < self.max_open {
                    break;
                }
                let wait = ctx
                    .remaining()
                    .unwrap_or(Duration::from_millis(200))
                    .min(Duration::from_millis(200));
                st = self
                    .cv
                    .wait_timeout(st, wait)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            if let Some(idle) = st.idle.pop() {
                drop(st);
                let mut conn = Conn {
                    pool: self,
                    c: Some(idle.c),
                };
                if idle.since.elapsed() >= PING_AFTER_IDLE {
                    let _ = conn.set_timeout(Some(Duration::from_secs(5)));
                    if conn.ping().is_err() {
                        conn.discard();
                        continue;
                    }
                }
                return Ok(conn);
            }
            st.open += 1;
            drop(st);
            let mut cfg = self.cfg.clone();
            if let Some(left) = ctx.remaining() {
                cfg = cfg.with_connect_timeout(
                    left.min(self.cfg.connect_timeout())
                        .max(Duration::from_millis(1)),
                );
            }
            return match Client::connect(&cfg) {
                Ok(c) => Ok(Conn {
                    pool: self,
                    c: Some(c),
                }),
                Err(e) => {
                    self.release_slot();
                    Err(DbError::Tds(e))
                }
            };
        }
    }

    /// Closes the idle connections and refuses new borrows.
    pub fn close(&self) {
        let idle = {
            let mut st = self.lock();
            st.closed = true;
            st.open -= st.idle.len().min(st.open);
            std::mem::take(&mut st.idle)
        };
        for i in idle {
            i.c.close();
        }
        self.cv.notify_all();
    }
}

/// The pools by database name.
pub type Pools = BTreeMap<String, Arc<Pool>>;

/// Opening the connection file failed.
#[derive(Debug)]
pub enum PoolsError {
    /// No file: the agent stays on the stub executor.
    NoConfig,
    /// The file is unusable; the message names the file or database, never a connection
    /// string.
    Invalid(String),
}

impl fmt::Display for PoolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolsError::NoConfig => f.write_str("no database connection file"),
            PoolsError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PoolsError {}

/// Reads the connection file and builds one pool per configured database (nothing
/// connects yet). MM_PMS is required.
pub fn open_pools(path: &Path) -> Result<Pools, PoolsError> {
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(PoolsError::NoConfig),
        Err(e) => {
            return Err(PoolsError::Invalid(format!(
                "database connection file: {e}"
            )));
        }
    };
    let bad = || {
        PoolsError::Invalid(
            "database connection file: not a JSON object of name -> connection string".into(),
        )
    };
    let obj = json::parse_object(&raw).ok_or_else(bad)?;
    let mut pools = Pools::new();
    for (name, v) in obj.iter() {
        let dsn = match v {
            Value::String(s) => s.as_str(),
            Value::Null => "",
            _ => return Err(bad()),
        };
        if ![DB_PMS, DB_FO, DB_POS].contains(&name.as_str()) {
            return Err(PoolsError::Invalid(format!(
                "database connection file: unknown database {} (MM_PMS, SMILE_FO, SMILE_POS)",
                json::quote(name)
            )));
        }
        if dsn.trim().is_empty() {
            continue;
        }
        let cfg: Config = dsn.parse().map_err(|_| {
            PoolsError::Invalid(format!(
                "database {name}: connection string rejected by the driver"
            ))
        })?;
        pools.insert(
            name.clone(),
            Arc::new(Pool::new(name, cfg, MAX_OPEN_PER_DB)),
        );
    }
    if !pools.contains_key(DB_PMS) {
        return Err(PoolsError::Invalid(
            "database connection file: MM_PMS is required".into(),
        ));
    }
    Ok(pools)
}

/// Closes every pool.
pub fn close_all(pools: &Pools) {
    for p in pools.values() {
        p.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_file_rules() {
        let d = std::env::temp_dir().join(format!("pms-db-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(DB_CONFIG_FILE);
        assert!(matches!(open_pools(&p), Err(PoolsError::NoConfig)));
        let write = |s: &str| std::fs::write(&p, s).unwrap();
        write(r#"{"MM_PMS":"sqlserver://sa:x@localhost?database=MM_PMS","SMILE_FO":""}"#);
        let pools = open_pools(&p).unwrap();
        assert_eq!(pools.keys().collect::<Vec<_>>(), [DB_PMS]);
        write(r#"{"SMILE_FO":"sqlserver://sa:x@localhost"}"#);
        assert!(
            open_pools(&p)
                .unwrap_err()
                .to_string()
                .contains("MM_PMS is required")
        );
        write(r#"{"MM_PMS":"sqlserver://sa:x@localhost","OTHER":"x"}"#);
        assert!(open_pools(&p).unwrap_err().to_string().contains("OTHER"));
        write(r#"{"MM_PMS":"sqlserver://sa:SECRET@localhost:notaport"}"#);
        let err = open_pools(&p).unwrap_err().to_string();
        assert!(!err.contains("SECRET"), "{err}");
        write("[]");
        assert!(open_pools(&p).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
