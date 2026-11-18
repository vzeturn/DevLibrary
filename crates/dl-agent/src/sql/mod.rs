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
//! ignored.

pub mod module;
pub mod pool;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dl_json::{Map, Value, json};

use crate::Error;
pub use pool::{Connection, Connector, Pool, PoolOptions, Pooled};

/// File in the data directory that declares the site's databases.
pub const SQL_FILE: &str = "sql.json";

/// Opens `dl_tds` sessions.
pub struct TdsConnector {
    config: dl_tds::Config,
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
        Ok(TdsConnector { config })
    }

    /// The parsed settings.
    pub fn config(&self) -> &dl_tds::Config {
        &self.config
    }
}

fn tds(e: dl_tds::Error) -> Error {
    Error::Sql(e.to_string())
}

impl Connector for TdsConnector {
    type Conn = dl_tds::Client;
    fn connect(&self) -> Result<dl_tds::Client, Error> {
        dl_tds::Client::connect(&self.config).map_err(tds)
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
}

/// The pool type modules use.
pub type SqlPool = Pool<TdsConnector>;
/// The registry modules see through [`ModuleContext::sql`](crate::supervisor::ModuleContext::sql).
pub type SqlPools = Pools<TdsConnector>;

/// The site's pools by database name.
pub struct Pools<C: Connector> {
    inner: Arc<Mutex<BTreeMap<String, Arc<Pool<C>>>>>,
}

impl<C: Connector> Clone for Pools<C> {
    fn clone(&self) -> Pools<C> {
        Pools {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<C: Connector> Default for Pools<C> {
    fn default() -> Pools<C> {
        Pools {
            inner: Arc::default(),
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
            let connector = TdsConnector::parse(connection).map_err(|e| bad(&e.to_string()))?;
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
