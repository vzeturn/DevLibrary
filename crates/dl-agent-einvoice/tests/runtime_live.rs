//! The run-time SQL paths against a real SQL Server (`MMEI_TEST_MSSQL_URL`, a sysadmin URL
//! without `database`; skipped otherwise), on the SMILE-shaped schema of `legacy_live`:
//!
//! - `collect_dry_run` and `writeback_probe` read and never write: no `MMEI_Collected` row,
//!   nothing changed in the source tables;
//! - the write-back procedure is idempotent and order-safe, so a command the Go agent already
//!   applied and the platform sends again to the new enrolment changes nothing;
//! - a sealed connection the server drops mid-run fails that one call and reconnects on the
//!   next, the way a cycle after a SQL Server restart does.

mod common;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{SCHEMA, TestDb, base_url, connect, exec};
use dl_agent_einvoice::agent_config::AgentConfig;
use dl_agent_einvoice::dryrun::{self, DryRun};
use dl_agent_einvoice::sql::{CollectKey, Sql, WritebackApply};
use dl_json::Value;
use dl_tds::Client;

const T: Duration = Duration::from_secs(60);

fn scalar_rows(c: &mut Client, q: &str) -> Vec<String> {
    let rs = c.query(q, &[]).unwrap_or_else(|e| panic!("{q}: {e}"));
    rs.rows()
        .iter()
        .map(|r| {
            (0..r.len())
                .map(|i| {
                    r.get::<Option<String>>(i)
                        .unwrap_or_else(|e| panic!("{q}: column {i}: {e}"))
                        .unwrap_or_else(|| "NULL".into())
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

/// Everything a read-only command could have touched.
fn fingerprint(c: &mut Client) -> Vec<String> {
    let mut all = scalar_rows(
        c,
        "SELECT CONVERT(varchar(64), HASHBYTES('SHA2_256', (SELECT * FROM dbo.BillInfor FOR XML RAW)), 2)",
    );
    all.extend(scalar_rows(
        c,
        "SELECT CONVERT(varchar(64), HASHBYTES('SHA2_256', (SELECT * FROM dbo.Invoice FOR XML RAW)), 2)",
    ));
    all.extend(scalar_rows(
        c,
        "SELECT CAST(COUNT(*) AS varchar(10)) FROM dbo.MMEI_Collected",
    ));
    all
}

fn mark_row(c: &mut Client) -> Vec<String> {
    scalar_rows(
        c,
        "SELECT InvoiceNumber, KyHieu, MaCQT, CONVERT(varchar(30), IssuedAt, 126) FROM dbo.MMEI_Collected WHERE Source = 'FO' AND ID = '7'",
    )
}

fn args(json: &str) -> Value {
    dl_json::parse(json).unwrap()
}

#[test]
fn dry_run_and_probe_are_read_only_and_writeback_is_idempotent() {
    let Some(base) = base_url() else { return };
    let db_guard = TestDb::create(&base, "mmei_runtime_live_rs");
    let dsn = db_guard.dsn.clone();
    let mut db = connect(&dsn.parse().unwrap());
    exec(&mut db, SCHEMA);
    dl_agent_einvoice::install::install_into(&mut db).expect("install the collect set");
    dl_agent_einvoice::install::install_writeback_into(&mut db).expect("install write-back");

    let cfg = AgentConfig::default();
    let (rules, sql) = (cfg.rules(), Sql::new(&dsn, &dsn, cfg.sp_names()));
    let before = fingerprint(&mut db);

    // collect_dry_run: lists and builds, writes nothing.
    let today: dl_time::Date = "2026-11-01".parse().unwrap();
    let d = DryRun::parse(&args(r#"{"sources":"FO,POS"}"#), &["FO", "POS"], today).unwrap();
    let out = dryrun::run(&sql, &rules, &d, Instant::now() + T);
    let text = dl_json::to_string(&out);
    assert_eq!(out.get("uploaded"), Some(&Value::from(false)), "{text}");
    for source in ["FO", "POS"] {
        let s = out.get("sources").and_then(|v| v.get(source)).unwrap();
        assert!(s.get("error").is_none(), "{source}: {text}");
        assert_eq!(
            s.get("failed"),
            Some(&Value::from(0_u64)),
            "{source}: {text}"
        );
        assert_eq!(
            s.get("listed"),
            Some(&Value::from(1_u64)),
            "{source}: {text}"
        );
    }
    assert_eq!(fingerprint(&mut db), before, "the dry run wrote: {text}");

    // writeback_probe: the target, the rights and the bill's (absent) mark, read only.
    let p = sql.writeback_probe("FO", Some("7"), T).unwrap();
    assert!(p.table_exists && p.proc_exists && p.may_write(), "{p:?}");
    assert_eq!(p.bill, Some(None), "no mark before the first push");
    assert_eq!(fingerprint(&mut db), before, "the probe wrote");

    // A pushed bill, then the platform's write-back commands, each delivered twice and the
    // number again after the tax-office code (a redelivery after the switch).
    let key = CollectKey {
        source: "FO".into(),
        id: "7".into(),
    };
    sql.mark_collected(&key, true, "ok", T).unwrap();
    let issued = dl_time::OffsetDateTime::from_unix_seconds(1_793_000_000).ok();
    let number = WritebackApply {
        bill_id: "7".into(),
        bill_type: "FO".into(),
        invoice_number: "0000123".into(),
        ky_hieu: "C26TAA".into(),
        ma_cqt: String::new(),
        issued_at: issued,
    };
    let cqt = WritebackApply {
        invoice_number: String::new(),
        ky_hieu: String::new(),
        ma_cqt: "M1-26-ABCDE".into(),
        issued_at: None,
        ..number.clone()
    };
    sql.apply_writeback(&number, T).unwrap();
    let once = mark_row(&mut db);
    sql.apply_writeback(&number, T).unwrap();
    assert_eq!(mark_row(&mut db), once, "a repeated number changed the row");
    sql.apply_writeback(&cqt, T).unwrap();
    let with_cqt = mark_row(&mut db);
    assert!(
        with_cqt[0].starts_with("0000123|C26TAA|M1-26-ABCDE|"),
        "{with_cqt:?}"
    );
    sql.apply_writeback(&cqt, T).unwrap();
    sql.apply_writeback(&number, T).unwrap();
    assert_eq!(mark_row(&mut db), with_cqt, "a redelivery changed the row");

    let p = sql.writeback_probe("FO", Some("7"), T).unwrap();
    assert_eq!((p.marked, p.invoiced), (Some(1), Some(1)), "{p:?}");
    let mark = p.bill.flatten().expect("the mark");
    assert_eq!(mark.invoice_number.as_deref(), Some("0000123"));

    // The next dry run sees the bill as collected and unchanged, with its number.
    let d = DryRun::parse(&args(r#"{"sources":"FO"}"#), &["FO"], today).unwrap();
    let out = dryrun::run(&sql, &rules, &d, Instant::now() + T);
    let text = dl_json::to_string(&out);
    let bill = out
        .get("sources")
        .and_then(|v| v.get("FO"))
        .and_then(|v| v.get("bills"))
        .and_then(|v| v.get_index(0))
        .unwrap_or_else(|| panic!("{text}"));
    assert_eq!(bill.get("state"), Some(&Value::from("unchanged")), "{text}");
    assert_eq!(
        bill.get("invoice_number"),
        Some(&Value::from("0000123")),
        "{text}"
    );

    db.close();
    drop(db_guard);
}

/// A TCP relay to the server whose open connections can be cut, like a server restart.
struct Relay {
    port: u16,
    open: Arc<Mutex<Vec<TcpStream>>>,
    stop: Arc<AtomicBool>,
}

impl Relay {
    fn start(target: String) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let open = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (o, s) = (open.clone(), stop.clone());
        std::thread::spawn(move || {
            for client in listener.incoming() {
                if s.load(Ordering::Acquire) {
                    break;
                }
                let Ok(client) = client else { continue };
                let Ok(server) = TcpStream::connect(&target) else {
                    continue;
                };
                let mut keep = o.lock().unwrap();
                keep.push(client.try_clone().unwrap());
                keep.push(server.try_clone().unwrap());
                pipe(client.try_clone().unwrap(), server.try_clone().unwrap());
                pipe(server, client);
            }
        });
        Relay { port, open, stop }
    }

    /// Cuts every open connection.
    fn cut(&self) {
        for s in self.open.lock().unwrap().drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cut();
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn pipe(mut from: TcpStream, mut to: TcpStream) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = to.shutdown(Shutdown::Write);
    });
}

#[test]
fn a_dropped_session_fails_one_call_and_reconnects() {
    let Some(base) = base_url() else { return };
    let db_guard = TestDb::create(&base, "mmei_runtime_drop_rs");
    let mut db = connect(&db_guard.dsn.parse().unwrap());
    exec(&mut db, SCHEMA);
    dl_agent_einvoice::install::install_into(&mut db).expect("install the collect set");
    db.close();

    // The same URL through the relay: `sqlserver://user:pw@host:port?...` → 127.0.0.1:relay.
    let cfg: dl_tds::Config = base.parse().unwrap();
    let target = format!("{}:{}", cfg.host(), cfg.port().unwrap_or(1433));
    let relay = Relay::start(target);
    let at = base.find('@').map(|i| i + 1).expect("a URL with a login");
    let rest = &base[at..];
    let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let relayed = format!(
        "{}127.0.0.1:{}{}",
        &base[..at],
        relay.port,
        &rest[host_end..]
    );
    let dsn = common::with_database(&relayed, "mmei_runtime_drop_rs");

    let sql = Sql::new(&dsn, "", AgentConfig::default().sp_names());
    let window = |sql: &Sql| {
        let day: dl_time::Date = "2026-11-01".parse().unwrap();
        sql.window(
            "FO",
            day.midnight(),
            day.midnight() + dl_time::Duration::from_days(1),
            10,
            T,
        )
    };
    assert_eq!(window(&sql).expect("through the relay").len(), 1);
    relay.cut();
    let err = window(&sql).expect_err("the cut session");
    assert!(!err.is_empty());
    assert_eq!(
        window(&sql).expect("reconnected").len(),
        1,
        "the next call reconnects"
    );
    assert!(
        sql.take_step_downs().is_empty(),
        "a dropped session is an outage, not a step down"
    );
    drop(relay);
    drop(db_guard);
}
