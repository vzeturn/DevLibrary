//! Live checks of the SQL bridge against a real SQL Server.
//!
//! Skipped unless `DL_AGENT_SQL_LIVE` holds a connection string, e.g.
//! `DL_AGENT_SQL_LIVE='Server=127.0.0.1,14333;User Id=sa;Password=…;Encrypt=disable' \
//!  cargo test -p dl-agent --test sql_live -- --test-threads=1`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent::sql::{Pool, PoolOptions, TdsConnector};

fn live() -> Option<Pool<TdsConnector>> {
    let conn = std::env::var("DL_AGENT_SQL_LIVE").ok()?;
    let opts = PoolOptions {
        max_connections: 4,
        query_timeout: Some(Duration::from_secs(2)),
        ping_after_idle: Duration::ZERO,
        backoff_min: Duration::from_millis(200),
        backoff_max: Duration::from_millis(400),
        ..PoolOptions::default()
    };
    Some(Pool::new("live", TdsConnector::parse(&conn).unwrap(), opts))
}

fn spid(c: &mut dl_tds::Client) -> i32 {
    let r = c.query("SELECT CAST(@@SPID AS int) AS spid", &[]).unwrap();
    r.rows()[0].get::<i32>(0).unwrap()
}

#[test]
fn many_threads_share_four_sessions() {
    let Some(pool) = live() else { return };
    let pool = Arc::new(pool);
    let workers: Vec<_> = (0..16)
        .map(|i| {
            let p = Arc::clone(&pool);
            std::thread::spawn(move || {
                p.with(|c| {
                    let r = c
                        .query("SELECT @n * 2 AS twice", &[dl_tds::Param::new("n", i)])
                        .map_err(|e| dl_agent::Error::Sql(e.to_string()))?;
                    Ok(r.rows()[0].get::<i32>(0).unwrap())
                })
                .unwrap()
            })
        })
        .collect();
    let sum: i32 = workers.into_iter().map(|w| w.join().unwrap()).sum();
    assert_eq!(sum, (0..16).map(|i| i * 2).sum::<i32>());
    let stats = pool.stats();
    let n = |k: &str| stats.get(k).and_then(dl_json::Value::as_u64).unwrap();
    assert!(n("connects") <= 4, "{stats:?}");
    assert_eq!(n("open"), n("idle"));
    println!("16 queries over {} sessions", n("connects"));
}

#[test]
fn a_slow_query_times_out_and_the_pool_keeps_working() {
    let Some(pool) = live() else { return };
    let started = Instant::now();
    let err = pool
        .with(|c| {
            c.query("WAITFOR DELAY '00:00:10'", &[])
                .map_err(|e| dl_agent::Error::Sql(e.to_string()))
        })
        .err()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "{err}");
    println!("slow query stopped after {:?}: {err}", started.elapsed());
    assert!(pool.check().is_ok());
}

#[test]
fn a_killed_session_is_replaced_on_next_checkout() {
    let Some(pool) = live() else { return };
    let victim = pool.with(|c| Ok(spid(c))).unwrap();
    // A second, separate session kills the idle one server-side.
    let killer = TdsConnector::parse(&std::env::var("DL_AGENT_SQL_LIVE").unwrap()).unwrap();
    let mut k = dl_tds::Client::connect(killer.config()).unwrap();
    k.batch(&format!("KILL {victim}")).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let fresh = pool.with(|c| Ok(spid(c))).unwrap();
    let discarded = pool
        .stats()
        .get("discarded")
        .and_then(dl_json::Value::as_u64);
    println!("killed spid {victim}; pool now uses spid {fresh}; discarded {discarded:?}");
    assert_eq!(discarded, Some(1));
}

#[test]
fn an_open_transaction_is_never_handed_to_the_next_caller() {
    let Some(pool) = live() else { return };
    pool.with(|c| {
        c.batch("BEGIN TRANSACTION").unwrap();
        assert!(c.in_transaction());
        Ok(())
    })
    .unwrap();
    let in_tx = pool.with(|c| Ok(c.in_transaction())).unwrap();
    assert!(!in_tx);
}
