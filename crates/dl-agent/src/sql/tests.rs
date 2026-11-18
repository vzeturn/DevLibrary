use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use dl_json::{Value, json};

use super::*;
use crate::store::testdir::TestDir;

/// A pretend server shared by every fake session.
#[derive(Default)]
pub struct FakeServer {
    down: AtomicBool,
    attempts: AtomicU64,
    generation: AtomicU64,
}

impl FakeServer {
    pub fn set_up(&self, up: bool) {
        self.down.store(!up, Ordering::SeqCst);
    }
    fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::SeqCst)
    }
}

pub struct FakeConnector(Arc<FakeServer>);

pub struct Fake {
    server: Arc<FakeServer>,
    generation: u64,
    broken: bool,
    in_tx: bool,
    timeout: Option<Duration>,
}

impl Fake {
    /// Kills every open session (their next ping fails).
    pub fn break_all(server: &FakeServer) {
        server.generation.fetch_add(1, Ordering::SeqCst);
    }
}

impl Connector for FakeConnector {
    type Conn = Fake;
    fn connect(&self) -> Result<Fake, Error> {
        self.0.attempts.fetch_add(1, Ordering::SeqCst);
        if self.0.down.load(Ordering::SeqCst) {
            return Err(Error::Sql("connection refused".into()));
        }
        Ok(Fake {
            server: Arc::clone(&self.0),
            generation: self.0.generation.load(Ordering::SeqCst),
            broken: false,
            in_tx: false,
            timeout: None,
        })
    }
}

impl Connection for Fake {
    fn ping(&mut self) -> Result<(), Error> {
        if self.server.down.load(Ordering::SeqCst)
            || self.generation != self.server.generation.load(Ordering::SeqCst)
        {
            self.broken = true;
            return Err(Error::Sql("connection reset".into()));
        }
        Ok(())
    }
    fn is_broken(&self) -> bool {
        self.broken
    }
    fn in_transaction(&self) -> bool {
        self.in_tx
    }
    fn set_query_timeout(&mut self, timeout: Option<Duration>) -> Result<(), Error> {
        self.timeout = timeout;
        Ok(())
    }
}

fn fast() -> PoolOptions {
    PoolOptions {
        max_connections: 2,
        acquire_timeout: Duration::from_millis(200),
        ping_after_idle: Duration::ZERO,
        backoff_min: Duration::from_millis(5),
        backoff_max: Duration::from_millis(10),
        ..PoolOptions::default()
    }
}

pub fn fake_pools(names: &[&str]) -> (Pools<FakeConnector>, Arc<FakeServer>) {
    let server = Arc::new(FakeServer::default());
    let pools = Pools::default();
    for name in names {
        pools.insert(Pool::new(name, FakeConnector(Arc::clone(&server)), fast()));
    }
    (pools, server)
}

fn pool() -> (Pool<FakeConnector>, Arc<FakeServer>) {
    let server = Arc::new(FakeServer::default());
    (
        Pool::new("db", FakeConnector(Arc::clone(&server)), fast()),
        server,
    )
}

fn stat(p: &Pool<FakeConnector>, key: &str) -> u64 {
    p.stats().get(key).and_then(Value::as_u64).unwrap()
}

#[test]
fn reuses_an_idle_session_and_sets_the_query_timeout() {
    let (p, server) = pool();
    drop(p.get().unwrap());
    let c = p.get().unwrap();
    assert_eq!(c.timeout, Some(Duration::from_secs(30)));
    drop(c);
    assert_eq!(server.attempts(), 1);
    assert_eq!((stat(&p, "open"), stat(&p, "idle")), (1, 1));
}

#[test]
fn callers_wait_for_a_free_session_then_time_out() {
    let (p, _server) = pool();
    let p = Arc::new(p);
    let a = p.get().unwrap();
    let b = p.get().unwrap();
    let err = p.get().err().unwrap().to_string();
    assert!(err.contains("all 2 connections busy"), "{err}");
    assert_eq!(stat(&p, "timeouts"), 1);

    let p2 = Arc::clone(&p);
    let waiter = std::thread::spawn(move || p2.get().map(|_| ()).is_ok());
    std::thread::sleep(Duration::from_millis(30));
    drop(a);
    assert!(waiter.join().unwrap(), "a freed session goes to the waiter");
    drop(b);
    assert_eq!(stat(&p, "open"), 2);
}

#[test]
fn broken_and_mid_transaction_sessions_are_closed_on_return() {
    let (p, server) = pool();
    let mut c = p.get().unwrap();
    c.broken = true;
    drop(c);
    let mut c = p.get().unwrap();
    c.in_tx = true;
    drop(c);
    assert_eq!((stat(&p, "open"), stat(&p, "discarded")), (0, 2));
    assert_eq!(server.attempts(), 2);
}

#[test]
fn a_dead_idle_session_is_replaced_transparently() {
    let (p, server) = pool();
    drop(p.get().unwrap());
    Fake::break_all(&server);
    assert!(p.get().is_ok());
    assert_eq!(server.attempts(), 2);
    assert_eq!(stat(&p, "discarded"), 1);
}

#[test]
fn a_down_database_fails_fast_until_the_backoff_then_recovers() {
    let (p, server) = pool();
    server.set_up(false);
    let err = p.get().err().unwrap().to_string();
    assert!(err.contains("connect failed: connection refused"), "{err}");
    let err = p.get().err().unwrap().to_string();
    assert!(err.contains("database unavailable"), "{err}");
    assert_eq!(server.attempts(), 1, "no second connect inside the backoff");
    assert!(p.down().is_some());

    std::thread::sleep(Duration::from_millis(15));
    assert!(p.get().is_err());
    assert_eq!(server.attempts(), 2, "one probe after the backoff");

    server.set_up(true);
    std::thread::sleep(Duration::from_millis(15));
    assert!(p.check().is_ok());
    assert!(p.down().is_none());
    assert_eq!((stat(&p, "connect_failures"), stat(&p, "connects")), (2, 1));
}

#[test]
fn reap_closes_idle_sessions_and_lower_limits_apply_on_return() {
    let (p, _server) = pool();
    let (a, b) = (p.get().unwrap(), p.get().unwrap());
    p.set_options(PoolOptions {
        max_connections: 1,
        idle_timeout: Duration::from_millis(10),
        ..fast()
    });
    drop((a, b));
    assert_eq!(
        stat(&p, "open"),
        1,
        "the session above the new limit closed"
    );
    std::thread::sleep(Duration::from_millis(15));
    p.reap();
    assert_eq!(stat(&p, "open"), 0);
}

#[test]
fn options_parse_with_defaults_and_bounds() {
    let o = options_from(&json!({"max_connections": 8, "query_timeout_seconds": 0})).unwrap();
    assert_eq!(o.max_connections, 8);
    assert_eq!(o.query_timeout, None);
    assert_eq!(o.acquire_timeout, Duration::from_secs(10));
    assert!(options_from(&json!({"max_connections": 0})).is_err());
    assert!(options_from(&json!({"max_connections": 65})).is_err());
    assert!(options_from(&json!({"acquire_timeout_seconds": 0})).is_err());
    assert!(options_from(&json!({"idle_timeout_seconds": "x"})).is_err());
}

const FO: &str = "Server=.\\SQLEXPRESS;Database=SMILE_FO;User Id=smile;Password=secret";

#[test]
fn declared_databases_load_and_bad_entries_are_rejected() {
    let pools = SqlPools::from_json(&json!({"databases": {
        "SMILE_FO": {"connection": FO, "max_connections": 6},
        "MM_PMS": {"connection": "sqlserver://u:p@srv:1433?database=MM_PMS&app+name=pms"}
    }}))
    .unwrap();
    assert_eq!(pools.names(), ["MM_PMS", "SMILE_FO"]);
    let fo = pools.get("SMILE_FO").unwrap();
    assert_eq!(fo.options().max_connections, 6);
    assert!(
        pools
            .get("SMILE_BO")
            .err()
            .unwrap()
            .to_string()
            .contains("not declared")
    );

    for bad in [
        json!({"databases": {"FO": {}}}),
        json!({"databases": {"bad name": {"connection": FO}}}),
        json!({"databases": {"FO": {"connection": FO, "max_connections": 100}}}),
    ] {
        let err = SqlPools::from_json(&bad).err().unwrap().to_string();
        assert!(!err.contains("secret"), "never echo the password: {err}");
    }
    assert!(SqlPools::from_json(&Value::Null).unwrap().is_empty());
}

#[test]
fn hub_tuning_overrides_limits_but_never_the_connection() {
    let local = json!({"databases": {"SMILE_FO": {"connection": FO, "max_connections": 4}}});
    let pools = SqlPools::from_json(&local).unwrap();
    let remote = json!({"SMILE_FO": {"max_connections": 12, "connection": "Server=evil"}});
    pools.tune(&local, &remote);
    let fo = pools.get("SMILE_FO").unwrap();
    assert_eq!(fo.options().max_connections, 12);
    pools.tune(&local, &Value::Null);
    assert_eq!(
        fo.options().max_connections,
        4,
        "removing the override restores local"
    );
}

#[test]
fn app_name_defaults_to_dl_agent() {
    let c = TdsConnector::parse(FO).unwrap();
    assert_eq!(c.config().app_name(), "dl-agent");
    assert_eq!(c.config().database(), "SMILE_FO");
    let c = TdsConnector::parse("sqlserver://u:p@srv?database=X&app+name=pms").unwrap();
    assert_eq!(c.config().app_name(), "pms");
}

#[test]
fn declare_writes_sql_json_that_load_reads_back() {
    let d = TestDir::new("sql-declare");
    declare(&d.0, "SMILE_FO", FO, 4).unwrap();
    declare(&d.0, "SMILE_POS", &FO.replace("SMILE_FO", "SMILE_POS"), 2).unwrap();
    assert!(declare(&d.0, "bad name", FO, 4).is_err());
    let pools = SqlPools::load(&d.0).unwrap();
    assert_eq!(pools.names(), ["SMILE_FO", "SMILE_POS"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(d.0.join(SQL_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "sql.json holds passwords");
    }
    assert_eq!(pools.get("SMILE_POS").unwrap().options().max_connections, 2);
}
