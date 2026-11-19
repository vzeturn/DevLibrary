//! Every SMILE write against a real SQL Server (opt-in, like `sql_live`).
//!
//! `MEMBERSHIP_AGENT_TEST_SQL` holds an ADO connection string to a SQL Server whose login
//! may create databases. The test builds a throw-away database from the SMILE fixture schema
//! (with its stored procedures), runs hub jobs through the dispatcher and the real
//! `SmileWriter` (create, replayed create, modify, a two-voucher redeem and its replay,
//! cancel), posts a discount, checks every row it expects, and drops the database. Without the variable it returns at once. The
//! connection string is never printed; the data is synthetic.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dl_agent_membership::jobs::beds::MODE_FOLD_LEGACY;
use dl_agent_membership::jobs::dispatch::{DispatchSettings, Dispatcher, Outcome};
use dl_agent_membership::jobs::plan::PmsWriter;
use dl_agent_membership::jobs::result::Ack;
use dl_agent_membership::smile::{TdsSmile, normalize_connection_string};
use dl_agent_membership::wire::parse::Job;
use dl_agent_membership::writer::folio::{FolioWriter, WriteOptions};
use dl_agent_membership::writer::smile_writer::SmileWriter;
use dl_decimal::Decimal;
use dl_tds::{Client, Config};

const ENV: &str = "MEMBERSHIP_AGENT_TEST_SQL";
const SCHEMA: &str = include_str!("fixtures/smile_schema.sql");
const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000001";
const ROOM: &str = "6f1c2d3e-0000-4000-8000-000000000101";

const SEED: &str = "\
INSERT INTO RoomType (RoomTypeCode, Description, NumRoom, DisplayOrder, TrnCode, TrnSubCode)
VALUES ('DLX', 'Deluxe', 2, 1, 0, 0);
UPDATE FolioNumberCounter SET Value = 7000;
UPDATE ConfirmNumberCounter SET Value = 8000;";

fn connect(cfg: &Config) -> Client {
    let mut last = String::new();
    for _ in 0..30 {
        match Client::connect(cfg) {
            Ok(c) => return c,
            Err(e) => last = format!("{:?}", e.number()),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("SQL Server login failed after 30 tries (error {last})");
}

struct Db {
    master: Config,
    cfg: Config,
    name: String,
}

impl Drop for Db {
    fn drop(&mut self) {
        let sql = format!(
            "ALTER DATABASE [{0}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{0}];",
            self.name
        );
        if let Err(e) = connect(&self.master).batch(&sql) {
            eprintln!("could not drop test database {}: {e}", self.name);
        }
    }
}

fn create(conn: &str) -> Db {
    let base = normalize_connection_string(conn)
        .unwrap_or_else(|_| panic!("{ENV} is not a usable connection string"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("dl_mbw_{}_{nanos}", std::process::id());
    let master = base.clone().with_database("master");
    connect(&master)
        .batch(&format!("CREATE DATABASE [{name}];"))
        .expect("CREATE DATABASE");
    let cfg = base.with_database(&name);
    let mut c = connect(&cfg);
    for (i, b) in SCHEMA
        .split("\nGO\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .enumerate()
    {
        c.batch(b)
            .unwrap_or_else(|e| panic!("schema batch {i}: {e}"));
    }
    c.batch(SEED).unwrap_or_else(|e| panic!("seed: {e}"));
    c.close();
    Db { master, cfg, name }
}

fn count(db: &Db, sql: &str) -> i32 {
    let mut c = connect(&db.cfg);
    let n = c.query(sql, &[]).expect(sql).rows()[0]
        .get::<i32>(0)
        .expect("count");
    c.close();
    n
}

fn job(id: &str, action: &str) -> Job {
    Job {
        job_id: id.into(),
        action: action.into(),
        booking_id: Some(BOOKING.parse().unwrap()),
        payload_json: format!(
            r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-LIVE-1","memberId":"6f1c2d3e-0000-4000-8000-0000000000aa",
                "memberCode":"MB-000001","member":{{"FullName":"Test Member"}},
                "rooms":[{{"Id":"{ROOM}","RoomTypeCode":"DLX","Arrival":"2026-12-01","Departure":"2026-12-03",
                "NumAdult":2,"GuestLastName":"Guest","GuestFirstName":"Synthetic",
                "nights":[{{"Date":"2026-12-01","Net":1000000}},{{"Date":"2026-12-02","Net":1200000}}]}}]}}"#
        ),
        attempts: 1,
    }
}

/// A REDEEM job with two stacked vouchers, listed out of `sequence` order.
fn redeem_job(id: &str) -> Job {
    Job {
        job_id: id.into(),
        action: "RedeemVoucher".into(),
        booking_id: Some(BOOKING.parse().unwrap()),
        payload_json: format!(
            r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-LIVE-1",
                "Vouchers":[{{"Code":"VCH-B","Type":"PercentOff","Value":0.15,"Sequence":2,"AppliedAmount":450000}},
                {{"Code":"VCH-A","Type":"AmountOff","Value":100000,"Sequence":1,"AppliedAmount":100000}}]}}"#
        ),
        attempts: 1,
    }
}

fn ack(o: Outcome) -> Ack {
    match o {
        Outcome::Ack(a) => a,
        Outcome::Leave(why) => panic!("left unacked: {why}"),
    }
}

#[test]
fn every_smile_write_runs_against_sql_server() {
    let Ok(conn) = std::env::var(ENV) else {
        eprintln!("skipped: {ENV} is not set");
        return;
    };
    let db = create(&conn);
    let smile = Arc::new(
        TdsSmile::new(&format!(
            "{};Database={}",
            conn.trim_end_matches(';'),
            db.name
        ))
        .expect("TdsSmile"),
    );
    let writer = Arc::new(SmileWriter::new(
        smile,
        FolioWriter::new(WriteOptions {
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
            ..WriteOptions::default()
        }),
    ));
    let d = Dispatcher::new(
        writer.clone(),
        None,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: String::new(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    );
    let live = "SELECT COUNT(*) FROM Folio WHERE ExternalConfirmNumber = 'AZ-LIVE-1' AND ISNULL(CancelNum, 0) = 0";

    // Create: one folio from GetFolioNumber, a ConfirmNum, nightly rates and a profile.
    let a = ack(d.process(&job("c1", "Create")));
    assert!(a.success, "create failed: {:?} {:?}", a.error_code, a.error);
    let folios = a.folios.clone().unwrap();
    assert_eq!(folios.len(), 1);
    assert_eq!(folios[0].room_item_id, ROOM.parse().unwrap());
    assert_eq!(folios[0].folio_num, Decimal::from(7001));
    assert_eq!(a.smile_confirm_num, Some(Decimal::from(8001)));
    assert_eq!(count(&db, live), 1);
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) FROM Folio WHERE FolioNum = 7001 AND UserInfo1 = '{ROOM}' AND UserInfo2 LIKE '%BKG:{BOOKING}%'"
            )
        ),
        1
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM FolioRate WHERE FolioNum = 7001"),
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM Profile WHERE ExternalProfileNumber = 'MB-000001'"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM AdditionalName WHERE FolioNum = 7001"
        ),
        2, // one row per adult (NumAdult = 2), as GuestCounts requires
    );

    // A replayed create finds its room under the lock: no second folio, same map.
    let again = ack(d.process(&job("c1", "Create")));
    assert!(again.success, "{:?} {:?}", again.error_code, again.error);
    assert_eq!(again.folios.unwrap()[0].folio_num, Decimal::from(7001));
    assert_eq!(count(&db, live), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM Folio"), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM Profile"), 1);

    // Modify with the same rooms keeps the folio.
    let m = ack(d.process(&job("m1", "Modify")));
    assert!(m.success, "{:?} {:?}", m.error_code, m.error);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM Folio"), 1);

    // Discount: posted once under its reference, found by the guard afterwards.
    let folio = Decimal::from(7001);
    assert!(!writer.has_posted_ref(&[folio], "VCH-LIVE-1").unwrap());
    writer
        .post_discount(folio, Decimal::from(-50000), "DS", "VCH-LIVE-1")
        .expect("post_discount");
    assert!(writer.has_posted_ref(&[folio], "VCH-LIVE-1").unwrap());
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM FolioTransaction WHERE FolioNum = '7001' AND RefNumber = 'VCH-LIVE-1'"
        ),
        1
    );

    // Stacked REDEEM (`vouchers` array): one line per voucher on the first live folio, for
    // the hub's appliedAmount in sequence order; a replay posts nothing more.
    let r = Dispatcher::new(
        writer.clone(),
        None,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: "DS".into(),
            extra_bed_mode: MODE_FOLD_LEGACY.into(),
        },
    );
    let two = redeem_job("r1");
    assert!(ack(r.process(&two)).success);
    assert!(ack(r.process(&two)).success);
    let lines = "SELECT COUNT(*) FROM FolioTransaction WHERE FolioNum = '7001' AND PostingClerkID = 'MEM' AND RefNumber IN ('VCH-A', 'VCH-B')";
    assert_eq!(count(&db, lines), 2);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM FolioTransaction WHERE FolioNum = '7001' AND RefNumber = 'VCH-A' AND TransactionAmount = -100000 AND TransactionCode = 'DS'"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM FolioTransaction WHERE FolioNum = '7001' AND RefNumber = 'VCH-B' AND TransactionAmount = -450000"
        ),
        1
    );

    // Travel agent reader over the real join (none seeded).
    assert_eq!(writer.find_travel_agent(5).unwrap(), None);

    // Cancel: CancelNum from GetCancelNum, idempotent on replay.
    let c = ack(d.process(&job("x1", "Cancel")));
    assert!(c.success, "{:?} {:?}", c.error_code, c.error);
    assert_eq!(count(&db, live), 0);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM Folio WHERE FolioNum = 7001 AND CancelNum > 0"
        ),
        1
    );
    assert!(ack(d.process(&job("x1", "Cancel"))).success);
    assert_eq!(
        count(&db, "SELECT CAST(Value AS int) FROM CancelNumberCounter"),
        1,
        "a replayed cancel takes no second cancel number"
    );
}
