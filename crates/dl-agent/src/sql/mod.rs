//! The shared on-prem SQL Server bridge: one connection pool per database, used by every
//! module on the site (PROTOCOL.md §9).
//!
//! Databases are declared on the site in `<data>/sql.json`, so connection strings and
//! passwords never leave the machine:
//!
//! ```json
//! {"databases": {"SMILE_FO": {"connection": "Server=.\\SQLEXPRESS;Database=SMILE_FO;User Id=smile;Password=…",
//!                             "max_connections": 4, "query_timeout_seconds": 30}}}
//! ```
//!
//! The hub may tune the limits per database through remote config
//! (`{"sql": {"SMILE_FO": {"max_connections": 8}}}`); a `connection` key from the hub is
//! ignored. `transport` (see [`transport`]) and `lock_timeout_seconds` are local only.

pub mod bundle;
pub mod install;
pub mod module;
pub mod pool;
pub mod probe;
pub mod survey;
pub mod transport;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dl_json::{Map, Value, json};

use crate::Error;
pub use pool::{Connection, Connector, Pool, PoolOptions, Pooled};

/// File in the data directory that declares the site's databases.
pub const SQL_FILE: &str = "sql.json";

/// Default `SET LOCK_TIMEOUT`: a row locked elsewhere in the product fails fast with SQL
/// error 1222 instead of holding a pool slot until the query timeout (Check-In 2026-11-18).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(8);

/// Opens `dl_tds` sessions, walking the [`transport`] ladder until one rung connects.
pub struct TdsConnector {
    config: dl_tds::Config,
    rungs: Vec<transport::Rung>,
    at: AtomicUsize,
    settled: AtomicBool,
    lock_timeout: Option<Duration>,
}

impl TdsConnector {
    /// Parses a connection string (URL or ADO form); the app name defaults to `dl-agent`.
    ///
    /// # Errors
    /// [`Error::Config`] when the string does not parse (the message never echoes it).
    pub fn parse(connection: &str) -> Result<TdsConnector, Error> {
        let mut config: dl_tds::Config = connection
            .parse()
            .map_err(|e: dl_tds::Error| Error::Config(format!("connection string: {e}")))?;
        if config.app_name() == "dl-tds" {
            config = config.with_app_name("dl-agent");
        }
        Ok(TdsConnector {
            rungs: transport::rungs(&config, None),
            config,
            at: AtomicUsize::new(0),
            settled: AtomicBool::new(false),
            lock_timeout: Some(DEFAULT_LOCK_TIMEOUT),
        })
    }

    /// Fixes the transport rung (`None` = walk the ladder).
    pub fn with_transport(mut self, pin: Option<transport::Rung>) -> TdsConnector {
        self.rungs = transport::rungs(&self.config, pin);
        self
    }

    /// `SET LOCK_TIMEOUT` on every new session (`None` = wait forever).
    pub fn with_lock_timeout(mut self, t: Option<Duration>) -> TdsConnector {
        self.lock_timeout = t;
        self
    }

    /// The parsed settings.
    pub fn config(&self) -> &dl_tds::Config {
        &self.config
    }

    /// The rung that connected, once one has.
    pub fn rung(&self) -> Option<transport::Rung> {
        self.settled
            .load(Ordering::Acquire)
            .then(|| self.rungs[self.at.load(Ordering::Acquire)])
    }

    fn open(&self) -> Result<dl_tds::Client, dl_tds::Error> {
        let start = self.at.load(Ordering::Acquire);
        if self.settled.load(Ordering::Acquire) {
            // Once a rung worked, a later drop is an outage, not a reason to lower security.
            return dl_tds::Client::connect(&self.rungs[start].apply(&self.config));
        }
        let (r, at) = transport::walk(&self.rungs, start, |rung| {
            dl_tds::Client::connect(&rung.apply(&self.config))
        });
        if r.is_ok() {
            self.at.store(at, Ordering::Release);
            self.settled.store(true, Ordering::Release);
        }
        r
    }
}

/// A session error, prefixed with its [`probe::Problem`] code when it is one the agent acts
/// on (`sql_missing_object: Invalid column name 'MaCQT'.`); see [`problem_code`].
fn tds(e: dl_tds::Error) -> Error {
    match probe::classify(&e) {
        probe::Problem::Other => Error::Sql(e.to_string()),
        p => Error::Sql(format!("{}: {e}", p.code())),
    }
}

/// The problem code of an [`Error::Sql`] (`sql_missing_object`, `sql_locked`, …), or
/// `fallback` for any other error.
pub fn problem_code<'a>(e: &Error, fallback: &'a str) -> &'a str {
    const CODES: [&str; 5] = [
        "sql_missing_object",
        "sql_locked",
        "sql_deadlock",
        "sql_access_denied",
        "sql_timeout",
    ];
    match e {
        Error::Sql(m) => CODES
            .into_iter()
            .find(|c| m.strip_prefix(c).is_some_and(|r| r.starts_with(':')))
            .unwrap_or(fallback),
        _ => fallback,
    }
}

impl Connector for TdsConnector {
    type Conn = dl_tds::Client;
    fn connect(&self) -> Result<dl_tds::Client, Error> {
        let mut client = self.open().map_err(tds)?;
        if let Some(t) = self.lock_timeout {
            client
                .execute(&format!("SET LOCK_TIMEOUT {}", t.as_millis()), &[])
                .map_err(tds)?;
        }
        Ok(client)
    }
    fn describe(&self) -> Value {
        json!({
            "rung": self.rung().map(transport::Rung::name),
            "lock_timeout_ms": self.lock_timeout.map(|t| t.as_millis() as u64),
        })
    }
}

impl Connection for dl_tds::Client {
    fn ping(&mut self) -> Result<(), Error> {
        dl_tds::Client::ping(self).map_err(tds)
    }
    fn is_broken(&self) -> bool {
        dl_tds::Client::is_broken(self)
    }
    fn in_transaction(&self) -> bool {
        dl_tds::Client::in_transaction(self)
    }
    fn set_query_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        self.set_timeout(timeout).map_err(tds)
    }
    fn run(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<(), Error> {
        let params: Vec<dl_tds::Param> = params
            .iter()
            .map(|(k, v)| dl_tds::Param::new(k, *v))
            .collect();
        self.execute(sql, &params).map(|_| ()).map_err(tds)
    }

    fn query_json(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
        let params: Vec<dl_tds::Param> = params
            .iter()
            .map(|(k, v)| dl_tds::Param::new(k, *v))
            .collect();
        let results = self.query(sql, &params).map_err(tds)?;
        let Some(set) = results.first() else {
            return Ok(Vec::new());
        };
        Ok(set
            .rows()
            .iter()
            .map(|row| {
                let mut m = Map::new();
                for (col, v) in set.columns().iter().zip(row.values()) {
                    m.insert(col.name(), tds_to_json(v));
                }
                Value::Object(m)
            })
            .collect())
    }
}

/// A TDS value as JSON: numbers stay numbers, decimals and dates become their text form.
fn tds_to_json(v: &dl_tds::Value) -> Value {
    use dl_tds::Value as T;
    match v {
        T::Null => Value::Null,
        T::Bool(b) => Value::from(*b),
        T::I8(n) => json!(u64::from(*n)),
        T::I16(n) => json!(i64::from(*n)),
        T::I32(n) => json!(i64::from(*n)),
        T::I64(n) => json!(*n),
        T::F32(f) => Value::from(f64::from(*f)),
        T::F64(f) => Value::from(*f),
        T::String(s) => Value::from(s.as_str()),
        T::Bytes(b) => Value::from(dl_encoding::hex::encode(b)),
        T::Decimal(d) => Value::from(d.to_string()),
        T::Uuid(u) => Value::from(u.to_string()),
        T::Date(d) => Value::from(d.to_string()),
        T::Time(t) => Value::from(t.to_string()),
        T::DateTime(d) => Value::from(d.to_string()),
        T::DateTimeOffset(d) => Value::from(d.to_string()),
        other => Value::from(format!("{other:?}")),
    }
}

/// The pool type modules use.
pub type SqlPool = Pool<TdsConnector>;
/// The registry modules see through [`ModuleContext::sql`](crate::supervisor::ModuleContext::sql).
pub type SqlPools = Pools<TdsConnector>;

/// The site's pools by database name.
pub struct Pools<C: Connector> {
    inner: Arc<Mutex<BTreeMap<String, Arc<Pool<C>>>>>,
    surveys: Arc<Mutex<BTreeMap<String, Value>>>,
}

impl<C: Connector> Clone for Pools<C> {
    fn clone(&self) -> Pools<C> {
        Pools {
            inner: Arc::clone(&self.inner),
            surveys: Arc::clone(&self.surveys),
        }
    }
}

impl<C: Connector> Default for Pools<C> {
    fn default() -> Pools<C> {
        Pools {
            inner: Arc::default(),
            surveys: Arc::default(),
        }
    }
}

impl<C: Connector> Pools<C> {
    /// Registers (or replaces) a pool.
    pub fn insert(&self, pool: Pool<C>) {
        let name = pool.name().to_string();
        self.lock().insert(name, Arc::new(pool));
    }

    /// The pool for `database`.
    ///
    /// # Errors
    /// [`Error::Sql`] when the site does not declare that database.
    pub fn get(&self, database: &str) -> Result<Arc<Pool<C>>, Error> {
        self.lock()
            .get(database)
            .cloned()
            .ok_or_else(|| Error::Sql(format!("database {database} is not declared in {SQL_FILE}")))
    }

    /// Declared database names, sorted.
    pub fn names(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// Every pool.
    pub fn all(&self) -> Vec<Arc<Pool<C>>> {
        self.lock().values().cloned().collect()
    }

    /// `true` when the site declares no database.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Applies the hub's `sql` config section on top of each pool's local options.
    pub fn tune(&self, local: &Value, remote: &Value) {
        for pool in self.all() {
            let mut entry = local
                .get("databases")
                .and_then(|d| d.get(pool.name()))
                .cloned()
                .unwrap_or(Value::Null);
            if let (Some(base), Some(over)) = (
                entry.as_object_mut(),
                remote.get(pool.name()).and_then(Value::as_object),
            ) {
                for (k, v) in over.iter().filter(|(k, _)| k.as_str() != "connection") {
                    base.insert(k, v.clone());
                }
            }
            match options_from(&entry) {
                Ok(opts) if opts != pool.options() => pool.set_options(opts),
                _ => {}
            }
        }
    }

    /// `{"<db>": {open, idle, …}}` for heartbeats and `sql.status`.
    pub fn stats(&self) -> Value {
        let mut m = Map::new();
        for pool in self.all() {
            m.insert(pool.name(), pool.stats());
        }
        Value::Object(m)
    }

    /// Stores the latest survey of `database` (PROTOCOL.md §9.5).
    pub fn set_survey(&self, database: &str, mut survey: Value) {
        let mut s = self.surveys.lock().unwrap_or_else(PoisonError::into_inner);
        // A fresh survey keeps the last schema probe until the next probe replaces it.
        if let (Some(old), Some(new)) = (
            s.get(database).and_then(|o| o.get("probe")).cloned(),
            survey.as_object_mut(),
        ) {
            new.insert("probe", old);
        }
        s.insert(database.to_string(), survey);
    }

    /// Records the last schema probe of `database` inside its survey (sent with the next
    /// heartbeat, so the hub gate sees what a product still lacks).
    pub fn set_probe(&self, database: &str, probe: Value) {
        let mut s = self.surveys.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = s
            .entry(database.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(m) = entry.as_object_mut() {
            m.insert("probe", probe);
        }
    }

    /// `{"<db>": survey}` for every database surveyed so far.
    pub fn surveys(&self) -> Value {
        let s = self.surveys.lock().unwrap_or_else(PoisonError::into_inner);
        let mut m = Map::new();
        for (k, v) in s.iter() {
            m.insert(k.as_str(), v.clone());
        }
        Value::Object(m)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Pool<C>>>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SqlPools {
    /// Builds the pools declared in `<data_dir>/sql.json` (none when the file is absent).
    ///
    /// # Errors
    /// [`Error::Config`] for an invalid file or entry; the whole file is rejected so a
    /// typo never silently drops a database.
    pub fn load(data_dir: &Path) -> Result<SqlPools, Error> {
        let doc = crate::store::read_json(&data_dir.join(SQL_FILE))?.unwrap_or(Value::Null);
        SqlPools::from_json(&doc)
    }

    /// Builds pools from a `{"databases": {…}}` document.
    ///
    /// # Errors
    /// [`Error::Config`] for an invalid entry.
    pub fn from_json(doc: &Value) -> Result<SqlPools, Error> {
        let pools = SqlPools::default();
        let Some(dbs) = doc.get("databases").and_then(Value::as_object) else {
            return Ok(pools);
        };
        for (name, entry) in dbs.iter() {
            let bad = |m: &str| Error::Config(format!("{SQL_FILE}: database {name}: {m}"));
            if !valid_name(name) {
                return Err(bad("name must be 1-64 letters, digits, '_' or '-'"));
            }
            let connection = entry
                .get("connection")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("missing \"connection\""))?;
            let pin = match entry.get("transport") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let s = v
                        .as_str()
                        .ok_or_else(|| bad("transport must be a string"))?;
                    transport::Rung::parse(s).map_err(|m| bad(&m))?
                }
            };
            let lock = seconds(entry, "lock_timeout_seconds", Some(DEFAULT_LOCK_TIMEOUT))
                .map_err(|e| bad(&e.to_string()))?;
            let connector = TdsConnector::parse(connection)
                .map_err(|e| bad(&e.to_string()))?
                .with_transport(pin)
                .with_lock_timeout(lock);
            let opts = options_from(entry).map_err(|e| bad(&e.to_string()))?;
            pools.insert(Pool::new(name, connector, opts));
        }
        Ok(pools)
    }
}

/// The site's pools plus the local declaration the hub's tuning is applied over.
pub struct Site {
    /// The pools modules share.
    pub pools: SqlPools,
    local: Value,
    tuned_version: Option<u64>,
    /// Why `sql.json` was rejected; the agent then runs without SQL and reports it.
    pub load_error: Option<String>,
}

impl Site {
    /// Loads `<data_dir>/sql.json`; an invalid file yields no pools and a `load_error`
    /// (the agent keeps running and reports it instead of crash-looping).
    pub fn load(data_dir: &Path) -> Site {
        let parsed = crate::store::read_json(&data_dir.join(SQL_FILE)).and_then(|doc| {
            let doc = doc.unwrap_or(Value::Null);
            SqlPools::from_json(&doc).map(|pools| (pools, doc))
        });
        let (pools, local, load_error) = match parsed {
            Ok((pools, doc)) => (pools, doc, None),
            Err(e) => (SqlPools::default(), Value::Null, Some(e.to_string())),
        };
        Site {
            pools,
            local,
            tuned_version: None,
            load_error,
        }
    }

    /// Applies the config's `sql` section when its version changed since the last call.
    pub fn tune(&mut self, config: &crate::config::RemoteConfig) {
        if self.tuned_version != Some(config.version) {
            let remote = config.body.get("sql").cloned().unwrap_or(Value::Null);
            self.pools.tune(&self.local, &remote);
            self.tuned_version = Some(config.version);
        }
    }
}

/// Adds or replaces one database in `<data_dir>/sql.json` after validating it.
///
/// # Errors
/// [`Error::Config`] for an invalid name or connection string; I/O errors.
pub fn declare(data_dir: &Path, name: &str, connection: &str, max: usize) -> Result<(), Error> {
    let path = data_dir.join(SQL_FILE);
    let mut doc = crate::store::read_json(&path)?.unwrap_or_else(|| json!({"databases": {}}));
    let entry = json!({"connection": connection, "max_connections": max as u64});
    let mut probe = Map::new();
    probe.insert(name, entry.clone());
    SqlPools::from_json(&json!({"databases": Value::Object(probe)}))?;
    let root = doc
        .as_object_mut()
        .ok_or_else(|| Error::Config(format!("{SQL_FILE} is not an object")))?;
    if !root.contains_key("databases") {
        root.insert("databases", Value::Object(Map::new()));
    }
    root.get_mut("databases")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| Error::Config(format!("{SQL_FILE}: databases is not an object")))?
        .insert(name, entry);
    crate::store::write_json(&path, &doc)?;
    // Connection strings carry passwords: only the service account and admins may read them.
    crate::identity::restrict_file(&path)
}

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn seconds(entry: &Value, key: &str, default: Option<Duration>) -> Result<Option<Duration>, Error> {
    match entry.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_u64() {
            Some(0) => Ok(None),
            Some(s) if s <= 86_400 => Ok(Some(Duration::from_secs(s))),
            _ => Err(Error::Config(format!("{key} must be 0..86400 seconds"))),
        },
    }
}

/// Pool options from a database entry (missing keys keep the defaults).
///
/// # Errors
/// [`Error::Config`] for out-of-range values.
pub fn options_from(entry: &Value) -> Result<PoolOptions, Error> {
    let d = PoolOptions::default();
    let max = match entry.get("max_connections") {
        None | Some(Value::Null) => d.max_connections,
        Some(v) => match v.as_u64() {
            Some(n @ 1..=64) => n as usize,
            _ => return Err(Error::Config("max_connections must be 1..64".into())),
        },
    };
    let required = |key: &str, default: Duration| -> Result<Duration, Error> {
        seconds(entry, key, Some(default))?
            .ok_or_else(|| Error::Config(format!("{key} must be at least 1 second")))
    };
    Ok(PoolOptions {
        max_connections: max,
        acquire_timeout: required("acquire_timeout_seconds", d.acquire_timeout)?,
        query_timeout: seconds(entry, "query_timeout_seconds", d.query_timeout)?,
        idle_timeout: required("idle_timeout_seconds", d.idle_timeout)?,
        max_lifetime: required("max_lifetime_seconds", d.max_lifetime)?,
        ..d
    })
}

#[cfg(test)]
mod tests;
