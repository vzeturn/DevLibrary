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
    fn query_json(&mut self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
        self.ping()?;
        if sql.contains("SERVERPROPERTY") {
            return Ok(vec![json!({
                "product_version": "10.50.1600.1", "edition": "Express Edition",
                "server_collation": "Vietnamese_CI_AS", "database_collation": "Vietnamese_CI_AS",
                "code_page": "1258", "compatibility_level": "100", "db_owner": "1", "execute": "1"
            })]);
        }
        // Schema probe: dbo.HoaDon(Id, SoHD), dbo.Config(Key = EINV_URL), dbo.usp_GetBill.
        if sql.contains("FROM sys.objects") {
            return Ok(vec![
                json!({"s": "dbo", "n": "HoaDon", "t": "U "}),
                json!({"s": "dbo", "n": "Config", "t": "U "}),
                json!({"s": "dbo", "n": "usp_GetBill", "t": "P "}),
            ]);
        }
        if sql.contains("FROM sys.columns") {
            let mut rows = Vec::new();
            for (_, v) in params {
                if *v == "[dbo].[HoaDon]" {
                    rows.push(json!({"s": "dbo", "n": "HoaDon", "c": "Id"}));
                    rows.push(json!({"s": "dbo", "n": "HoaDon", "c": "SoHD"}));
                }
                if *v == "[dbo].[Config]" {
                    rows.push(json!({"s": "dbo", "n": "Config", "c": "Key"}));
                }
            }
            return Ok(rows);
        }
        if sql.contains("FROM [dbo].[Config]") {
            assert!(
                !sql.contains("EINV"),
                "values must be bound, not spliced: {sql}"
            );
            return Ok(params
                .iter()
                .filter(|(_, v)| *v == "EINV_URL")
                .map(|(_, v)| json!({"v": v}))
                .collect());
        }
        panic!("unexpected query: {sql}");
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
    assert_eq!(c.timeout, Some(Duration::from_secs(15)));
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
fn transport_and_lock_timeout_are_local_settings_reported_in_stats() {
    let pools = SqlPools::from_json(&json!({"databases": {
        "SMILE_FO": {"connection": FO},
        "OLD": {"connection": FO, "transport": "unencrypted", "lock_timeout_seconds": 0}
    }}))
    .unwrap();
    let t = pools.get("SMILE_FO").unwrap().stats()["transport"].clone();
    assert_eq!(t["lock_timeout_ms"].as_u64(), Some(8000));
    assert!(t["rung"].is_null(), "no rung before the first connect");
    let old = pools.get("OLD").unwrap().stats()["transport"].clone();
    assert!(
        old["lock_timeout_ms"].is_null(),
        "0 = wait for locks forever"
    );

    let c = TdsConnector::parse(FO).unwrap();
    assert_eq!(c.rung(), None);
    for bad in [json!("tls"), json!(1)] {
        let doc = json!({"databases": {"FO": {"connection": FO, "transport": bad}}});
        assert!(SqlPools::from_json(&doc).is_err());
    }
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

/// Live check against a real server: `DL_AGENT_LIVE_SQL="Server=…;User Id=…;Password=…"
/// cargo test -p dl-agent live_sql -- --ignored`.
#[test]
#[ignore = "needs a SQL Server in DL_AGENT_LIVE_SQL"]
fn live_sql_connects_on_a_rung_and_caps_lock_waits() {
    let cs = std::env::var("DL_AGENT_LIVE_SQL").expect("DL_AGENT_LIVE_SQL");
    // A client without TLS (macOS) must pin `unencrypted`: it never steps down by itself.
    let want = std::env::var("DL_AGENT_LIVE_SQL_RUNG").unwrap_or("as_configured".into());
    let pin = transport::Rung::parse(&want).unwrap();
    let c = TdsConnector::parse(&cs).unwrap().with_transport(pin);
    let mut s = Connector::connect(&c).unwrap();
    assert_eq!(c.rung().map(transport::Rung::name), Some(want.as_str()));
    let r = s.query("SELECT @@LOCK_TIMEOUT AS t", &[]).unwrap();
    let t: i32 = r.first().unwrap().rows()[0].get("t").unwrap();
    assert_eq!(t, 8000);
    // The survey query must run as written on a real engine (2017+ in docker here).
    let sv = survey::survey(&mut s, Some(&want)).unwrap();
    eprintln!("{}", dl_json::to_string_pretty(&sv));
    assert!(sv["engine"]["major"].as_u64().unwrap() >= 10);
    assert!(sv["database"]["compatibility_level"].as_u64().unwrap() >= 100);
    assert!(sv["grants"]["sysadmin"].is_boolean());

    // Schema probe on real catalogs, with a value that would break if it were spliced.
    s.execute(
        "IF OBJECT_ID('dbo.dl_probe_cfg') IS NOT NULL DROP TABLE dbo.dl_probe_cfg; \
         CREATE TABLE dbo.dl_probe_cfg ([Key] varchar(20) NOT NULL, Val nvarchar(50)); \
         INSERT dbo.dl_probe_cfg VALUES ('EINV_URL', N'x'), ('O''Brien', N'y');",
        &[],
    )
    .unwrap();
    let req = probe::Requirement::from_json(&json!({
        "tables": {"dbo.dl_probe_cfg": ["Key", "Val", "MaCQT"]},
        "procedures": ["dbo.dl_probe_missing"],
        "rows": [{"table": "dbo.dl_probe_cfg", "column": "Key",
                  "values": ["EINV_URL", "O'Brien", "EINV_KEY"]}]
    }))
    .unwrap();
    let result = probe::probe(&mut s, &req);
    let missing_col = s.query_json("SELECT MaCQT FROM dbo.dl_probe_cfg", &[]);
    s.execute("DROP TABLE dbo.dl_probe_cfg", &[]).unwrap();
    let pr = result.unwrap();
    eprintln!("{}", dl_json::to_string_pretty(&pr));
    let names: Vec<&str> = pr["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "dbo.dl_probe_missing",
            "dbo.dl_probe_cfg.MaCQT",
            "dbo.dl_probe_cfg.Key"
        ]
    );
    assert_eq!(pr["missing"][2]["value"].as_str(), Some("EINV_KEY"));
    let e = missing_col.unwrap_err();
    assert_eq!(problem_code(&e, "x"), "sql_missing_object", "{e}");
}
