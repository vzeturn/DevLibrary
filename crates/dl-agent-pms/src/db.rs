//! The site's SMILE databases: SMILE_FO, SMILE_POS and SMILE_BO, the only databases a
//! customer has on site (SMILE-only contract, SMILE-PMS decision 2026-11-19). Each comes
//! from the dl-agent core `sql.json` when it declares it (auth windows|sql, sealed
//! password, saved TLS rung: [`dl_agent::sql`]) and otherwise from `databases.json` in the
//! protected state directory (`{"SMILE_FO": "<connection string>", …}`, the file
//! `SmilePmsAgent` left). An `MM_PMS` entry is ignored: MM_PMS lives in the cloud.
//! Connection strings are never logged; errors name the database.
//!
//! A `databases.json` pool plays the part of Go's `database/sql`: at most
//! [`MAX_OPEN_PER_DB`] connections, idle ones reused, a connection left broken or inside a
//! transaction closed instead of reused, and an idle connection pinged before reuse after
//! a minute. A `sql.json` pool is the core's shared pool, with the same reuse rules.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_agent::sql::secret::AuthMode;
use dl_agent::sql::{Pooled, SqlPool, TdsConnector};
use dl_json::Value;
use dl_tds::{Client, Config};

use crate::ctx::{Ctx, Done};
use crate::json;
use crate::ops::TARGET_DBS;
use crate::wire;

/// The connection file name in the state directory.
pub const DB_CONFIG_FILE: &str = "databases.json";
/// SMILE_FO (FO ops).
pub const DB_FO: &str = TARGET_DBS[0];
/// SMILE_POS (POS ops).
pub const DB_POS: &str = TARGET_DBS[1];
/// SMILE_BO (BO ops).
pub const DB_BO: &str = TARGET_DBS[2];
/// The pre-contract MM_PMS entry: in the cloud now, ignored when a site file still names it.
pub const DB_LEGACY_PMS: &str = "MM_PMS";
/// Each `databases.json` pool's cap: the in-flight cap plus one probe.
pub const MAX_OPEN_PER_DB: usize = wire::INFLIGHT_CAP + 1;

const PING_AFTER_IDLE: Duration = Duration::from_secs(60);

/// A database failure.
#[derive(Debug)]
pub enum DbError {
    /// The driver or the server.
    Tds(dl_tds::Error),
    /// The context ended first.
    Done(Done),
    /// The core's shared pool could not hand out a session (down, or no free slot).
    Unavailable(String),
}

impl DbError {
    /// The SQL Server error number, if the server raised one.
    pub fn number(&self) -> Option<i32> {
        match self {
            DbError::Tds(e) => e.number(),
            DbError::Done(_) | DbError::Unavailable(_) => None,
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Tds(e) => e.fmt(f),
            DbError::Done(d) => d.fmt(f),
            DbError::Unavailable(m) => f.write_str(m),
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

/// Where a database's connection settings came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The dl-agent core `sql.json` (shared pool).
    SqlJson,
    /// `databases.json` in the PMS state directory.
    DatabasesJson,
}

impl Source {
    /// The file name, for selftest and status.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::SqlJson => dl_agent::sql::SQL_FILE,
            Source::DatabasesJson => DB_CONFIG_FILE,
        }
    }
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

/// A `databases.json` pool.
struct Own {
    cfg: Config,
    max_open: usize,
    st: Mutex<State>,
    cv: Condvar,
}

enum Backend {
    Own(Box<Own>),
    Shared(Arc<SqlPool>),
}

/// One database's connections.
pub struct Pool {
    name: String,
    backend: Backend,
}

impl fmt::Debug for Pool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("name", &self.name)
            .field("source", &self.source())
            .finish()
    }
}

enum Slot<'a> {
    Own(&'a Own, Option<Client>),
    Shared(Pooled<'a, TdsConnector>),
}

/// A connection borrowed from a [`Pool`]; it goes back on drop.
pub struct Conn<'a> {
    slot: Slot<'a>,
}

impl Deref for Conn<'_> {
    type Target = Client;
    fn deref(&self) -> &Client {
        match &self.slot {
            Slot::Own(_, c) => c.as_ref().expect("connection present until drop"),
            Slot::Shared(p) => p,
        }
    }
}

impl DerefMut for Conn<'_> {
    fn deref_mut(&mut self) -> &mut Client {
        match &mut self.slot {
            Slot::Own(_, c) => c.as_mut().expect("connection present until drop"),
            Slot::Shared(p) => p,
        }
    }
}

impl Conn<'_> {
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
        // A shared session goes back through the core pool's own Drop.
        let Slot::Own(pool, c) = &mut self.slot else {
            return;
        };
        let Some(c) = c.take() else { return };
        if c.is_broken() || c.in_transaction() {
            c.close();
            pool.release_slot();
            return;
        }
        let mut st = pool.lock();
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
        pool.cv.notify_one();
    }
}

impl Own {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn release_slot(&self) {
        let mut st = self.lock();
        st.open = st.open.saturating_sub(1);
        self.cv.notify_one();
    }

    fn get(&self, ctx: &Ctx) -> Result<Conn<'_>, DbError> {
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
            if let Some(mut idle) = st.idle.pop() {
                drop(st);
                if idle.since.elapsed() >= PING_AFTER_IDLE {
                    let alive = idle.c.set_timeout(Some(Duration::from_secs(5))).is_ok()
                        && idle.c.ping().is_ok();
                    if !alive {
                        idle.c.close();
                        self.release_slot();
                        continue;
                    }
                }
                return Ok(Conn {
                    slot: Slot::Own(self, Some(idle.c)),
                });
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
                    slot: Slot::Own(self, Some(c)),
                }),
                Err(e) => {
                    self.release_slot();
                    Err(DbError::Tds(e))
                }
            };
        }
    }

    fn close(&self) {
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

impl Pool {
    /// A `databases.json` pool for `cfg`, opening at most `max_open` connections.
    pub fn new(name: &str, cfg: Config, max_open: usize) -> Pool {
        Pool {
            name: name.to_owned(),
            backend: Backend::Own(Box::new(Own {
                cfg,
                max_open: max_open.max(1),
                st: Mutex::new(State::default()),
                cv: Condvar::new(),
            })),
        }
    }

    /// A pool over the core's shared pool for this database (`sql.json`).
    pub fn shared(name: &str, pool: Arc<SqlPool>) -> Pool {
        Pool {
            name: name.to_owned(),
            backend: Backend::Shared(pool),
        }
    }

    /// The database name this pool serves.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Where its settings came from.
    pub fn source(&self) -> Source {
        match self.backend {
            Backend::Own(_) => Source::DatabasesJson,
            Backend::Shared(_) => Source::SqlJson,
        }
    }

    /// `windows` or `sql`: the login the pool connects with.
    pub fn auth(&self) -> String {
        match &self.backend {
            Backend::Own(o) => AuthMode::of(&o.cfg).as_str().to_owned(),
            Backend::Shared(p) => p.stats()["transport"]["auth"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned(),
        }
    }

    /// The TLS rung the core settled on (`None` for a `databases.json` pool or before the
    /// first session).
    pub fn rung(&self) -> Option<String> {
        match &self.backend {
            Backend::Own(_) => None,
            Backend::Shared(p) => p.stats()["transport"]["rung"].as_str().map(str::to_owned),
        }
    }

    /// Borrows a connection: an idle one, a new one under the cap, or the next one
    /// returned, waiting no longer than `ctx` allows (a shared pool waits up to its own
    /// acquire timeout).
    pub fn get(&self, ctx: &Ctx) -> Result<Conn<'_>, DbError> {
        match &self.backend {
            Backend::Own(o) => o.get(ctx),
            Backend::Shared(p) => {
                if let Some(d) = ctx.err() {
                    return Err(DbError::Done(d));
                }
                let s = p.get().map_err(|e| DbError::Unavailable(e.to_string()))?;
                if let Some(d) = ctx.err() {
                    return Err(DbError::Done(d));
                }
                Ok(Conn {
                    slot: Slot::Shared(s),
                })
            }
        }
    }

    /// Closes the idle connections and refuses new borrows (a shared pool belongs to the
    /// core and stays open).
    pub fn close(&self) {
        if let Backend::Own(o) = &self.backend {
            o.close();
        }
    }
}

/// The pools by database name.
pub type Pools = BTreeMap<String, Arc<Pool>>;

/// The core's shared pools for the SMILE databases `sql.json` declares.
pub type SharedPools = Vec<(String, Arc<SqlPool>)>;

/// Opening the connection file failed.
#[derive(Debug)]
pub enum PoolsError {
    /// The file is unusable; the message names the file or database, never a connection
    /// string.
    Invalid(String),
}

impl fmt::Display for PoolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoolsError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PoolsError {}

/// The site's SMILE databases.
#[derive(Debug, Default)]
pub struct SiteDbs {
    /// One pool per configured database.
    pub pools: Pools,
    /// Entries of `databases.json` left out (`MM_PMS`, which is in the cloud).
    pub ignored: Vec<String>,
    /// Whether `databases.json` exists.
    pub file_found: bool,
}

/// The shared pools of `ctx` for the SMILE databases its `sql.json` declares.
pub fn shared_pools(ctx: &dl_agent::supervisor::ModuleContext) -> SharedPools {
    TARGET_DBS
        .iter()
        .filter_map(|name| ctx.sql(name).ok().map(|p| ((*name).to_owned(), p)))
        .collect()
}

/// Builds the site's pools: the core's `shared` pools first, then `databases.json` at
/// `path` for every SMILE database `sql.json` does not declare (nothing connects yet).
/// A missing file is not an error; `MM_PMS` is ignored, any other name refused.
pub fn open(shared: &SharedPools, path: &Path) -> Result<SiteDbs, PoolsError> {
    let mut out = SiteDbs::default();
    for (name, pool) in shared {
        out.pools
            .insert(name.clone(), Arc::new(Pool::shared(name, Arc::clone(pool))));
    }
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => {
            return Err(PoolsError::Invalid(format!(
                "database connection file: {e}"
            )));
        }
    };
    out.file_found = true;
    let bad = || {
        PoolsError::Invalid(
            "database connection file: not a JSON object of name -> connection string".into(),
        )
    };
    let obj = json::parse_object(&raw).ok_or_else(bad)?;
    for (name, v) in obj.iter() {
        let dsn = match v {
            Value::String(s) => s.as_str(),
            Value::Null => "",
            _ => return Err(bad()),
        };
        if name == DB_LEGACY_PMS {
            out.ignored.push(name.clone());
            continue;
        }
        if !TARGET_DBS.contains(&name.as_str()) {
            return Err(PoolsError::Invalid(format!(
                "database connection file: unknown database {} (SMILE_FO, SMILE_POS, SMILE_BO)",
                json::quote(name)
            )));
        }
        if dsn.trim().is_empty() || out.pools.contains_key(name) {
            continue;
        }
        let cfg: Config = dsn.parse().map_err(|_| {
            PoolsError::Invalid(format!(
                "database {name}: connection string rejected by the driver"
            ))
        })?;
        out.pools.insert(
            name.clone(),
            Arc::new(Pool::new(name, cfg, MAX_OPEN_PER_DB)),
        );
    }
    Ok(out)
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

    fn shared(name: &str) -> SharedPools {
        let doc = dl_json::json!({"databases": {
            name: {"connection": "Server=127.0.0.1,1;Database=x;User Id=u;Password=p;Encrypt=false"}
        }});
        let pools = dl_agent::sql::SqlPools::from_json(&doc).unwrap();
        vec![(name.to_owned(), pools.get(name).unwrap())]
    }

    #[test]
    fn connection_file_rules() {
        let d = std::env::temp_dir().join(format!("pms-db-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(DB_CONFIG_FILE);
        let none = SharedPools::new();
        let empty = open(&none, &p).unwrap();
        assert!(empty.pools.is_empty() && !empty.file_found);
        let write = |s: &str| std::fs::write(&p, s).unwrap();
        write(
            r#"{"MM_PMS":"sqlserver://sa:x@localhost?database=MM_PMS","SMILE_FO":"sqlserver://sa:x@localhost?database=SMILE_FO","SMILE_POS":"","SMILE_BO":"sqlserver://sa:x@localhost?database=SMILE_BO"}"#,
        );
        let dbs = open(&none, &p).unwrap();
        assert_eq!(dbs.pools.keys().collect::<Vec<_>>(), [DB_BO, DB_FO]);
        assert_eq!(
            dbs.ignored,
            [DB_LEGACY_PMS],
            "MM_PMS is ignored, not required"
        );
        assert_eq!(dbs.pools[DB_FO].source(), Source::DatabasesJson);
        assert_eq!(dbs.pools[DB_FO].auth(), "sql");

        // sql.json wins over databases.json for the same database.
        let dbs = open(&shared(DB_FO), &p).unwrap();
        assert_eq!(dbs.pools[DB_FO].source(), Source::SqlJson);
        assert_eq!(dbs.pools[DB_BO].source(), Source::DatabasesJson);

        write(r#"{"SMILE_FO":"sqlserver://sa:x@localhost","OTHER":"x"}"#);
        assert!(open(&none, &p).unwrap_err().to_string().contains("OTHER"));
        write(r#"{"SMILE_FO":"sqlserver://sa:SECRET@localhost:notaport"}"#);
        let err = open(&none, &p).unwrap_err().to_string();
        assert!(!err.contains("SECRET"), "{err}");
        write("[]");
        assert!(open(&none, &p).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
