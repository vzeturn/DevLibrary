//! `SMILE_SPECIAL` extra-bed rows against a real SQL Server (opt-in, like `sql_live`).
//!
//! Mirrors MM-MEMBERSHIP `FolioWriterExtraBedSpecialTests`: a throw-away database from the
//! SMILE fixture schema, seeded with Victory's terms (`SmileFixture.SeedExtraBedSpecialAsync`
//! and `SeedSmileRatesAsync`), then hub jobs through the dispatcher and the real
//! `SmileWriter`. A create writes one agent `Folio_Special` row; a replay and an unchanged
//! modify write nothing; a bed change rewrites only the agent row (a front-desk row stays);
//! a switch back to `FOLD_LEGACY` removes the agent row. Without
//! `MEMBERSHIP_AGENT_TEST_SQL` it returns at once; the connection string is never printed
//! and the data is synthetic.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dl_agent_membership::jobs::beds::{MODE_FOLD_LEGACY, MODE_SMILE_SPECIAL};
use dl_agent_membership::jobs::dispatch::{DispatchSettings, Dispatcher, Outcome};
use dl_agent_membership::jobs::plan::PmsWriter;
use dl_agent_membership::jobs::result::Ack;
use dl_agent_membership::smile::{TdsSmile, normalize_connection_string};
use dl_agent_membership::terms::TermsSource;
use dl_agent_membership::wire::parse::Job;
use dl_agent_membership::writer::folio::{FolioWriter, WriteOptions};
use dl_agent_membership::writer::smile_writer::SmileWriter;
use dl_tds::{Client, Config};

const ENV: &str = "MEMBERSHIP_AGENT_TEST_SQL";
const SCHEMA: &str = include_str!("fixtures/smile_schema.sql");
const BOOKING: &str = "6f1c2d3e-0000-4000-8000-000000000003";
const ROOM: &str = "6f1c2d3e-0000-4000-8000-000000000301";
/// The Victory fingerprint, computed from the canonical text outside Rust.
const VICTORY_FINGERPRINT: &str =
    "872409c5dda0487f181793ce3bedf6b6c29a6a6be2593c9fa65fc89ae11c1d33";

/// `SeedExtraBedSpecialAsync` + `SeedSmileRatesAsync` + `SeedExtraBedTermsAsync` for `VIC`.
const SEED: &str = "\
INSERT INTO RoomType (RoomTypeCode, Description, NumRoom, DisplayOrder, TrnCode, TrnSubCode)
VALUES ('DLX', 'Deluxe', 2, 1, 0, 0);
UPDATE FolioNumberCounter SET Value = 7000;
UPDATE ConfirmNumberCounter SET Value = 8000;
INSERT INTO SpecialService (SpecialServiceCode, Description, AmountCharge, IncludeInRoomCharge, OpenPrice,
                            TrnCode, TrnSubCode)
VALUES ('EB', N'Extra bed', 0, 0, 1, '110', '9');
INSERT INTO TransactionCode (TransactionCode, TransactionSubCode, Description, TaxCode, ServiceChargeCode,
                             STaxCode, AutoPost)
VALUES ('110', '9', N'Extra bed', 'V8', 'S5', NULL, 0),
       ('110', '1', N'Room charge', 'V8', 'S5', NULL, 1);
CREATE TABLE dbo.TaxDef (TaxCode varchar(10) NOT NULL PRIMARY KEY, Description nvarchar(50) NULL,
                         TaxRate float NULL);
CREATE TABLE dbo.ServiceCharge (ServiceChargeCode varchar(10) NOT NULL PRIMARY KEY,
                                Description nvarchar(50) NULL, ServiceRate float NULL);
INSERT INTO dbo.TaxDef (TaxCode, Description, TaxRate) VALUES ('V8', N'VAT 8%', 0.08), ('V10', N'VAT 10%', 0.1);
INSERT INTO dbo.ServiceCharge (ServiceChargeCode, Description, ServiceRate) VALUES ('S5', N'SC 5%', 0.05);
INSERT INTO GlobalParameter (HotelID, ParaName, ParaString, Status)
VALUES ('VIC', 'SpecialByDate', '1', 1), ('VIC', 'RateCodePP', '1', 1), ('VIC', 'ExtraBedSurChg', '0', 1);
INSERT INTO ExtraBedInv (CruiseCode, NumBed, HotelID) VALUES ('C', 48, 'VIC');";

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
    let name = format!("dl_mbb_{}_{nanos}", std::process::id());
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

fn run(db: &Db, sql: &str) {
    let mut c = connect(&db.cfg);
    c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    c.close();
}

fn count(db: &Db, sql: &str) -> i64 {
    let mut c = connect(&db.cfg);
    let n = c.query(sql, &[]).expect(sql).rows()[0]
        .get::<i64>(0)
        .expect("count");
    c.close();
    n
}

fn job(id: &str, action: &str, beds: i64) -> Job {
    let net = 1_300_000 + beds * 400_000;
    Job {
        job_id: id.into(),
        action: action.into(),
        booking_id: Some(BOOKING.parse().unwrap()),
        payload_json: format!(
            r#"{{"bookingId":"{BOOKING}","confirmationCode":"AZ-BED-1",
                "extraBedVerifiedFingerprint":"{VICTORY_FINGERPRINT}",
                "rooms":[{{"Id":"{ROOM}","RoomTypeCode":"DLX","Arrival":"2026-12-01","Departure":"2026-12-03",
                "NumAdult":3,"GuestLastName":"Guest","GuestFirstName":"Synthetic",
                "PricingModel":"OCCUPANCY_V1","ExtraBeds":{beds},"ExtraBedNightly":400000,
                "nights":[{{"Date":"2026-12-01","Net":{net},"NetExclBed":1300000}},
                          {{"Date":"2026-12-02","Net":{net},"NetExclBed":1300000}}]}}]}}"#
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

fn dispatcher(conn: &str, db: &Db, mode: &str) -> (Arc<SmileWriter>, Dispatcher) {
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
            extra_bed_mode: mode.into(),
            ..WriteOptions::default()
        }),
    ));
    let d = Dispatcher::new(
        writer.clone(),
        None,
        DispatchSettings {
            origin_code: "AZ".into(),
            member_discount_code: String::new(),
            extra_bed_mode: mode.into(),
        },
    );
    (writer, d)
}

const MM_ROWS: &str = "SELECT CAST(COUNT(*) AS bigint) FROM Folio_Special WHERE FolioNum = 7001 AND SPNotes LIKE 'MM:6f1c2d3e-0000-4000-8000-000000000301 %'";

#[test]
fn smile_special_rows_are_written_and_reconciled_against_sql_server() {
    let Ok(conn) = std::env::var(ENV) else {
        eprintln!("skipped: {ENV} is not set");
        return;
    };
    let db = create(&conn);
    let (writer, d) = dispatcher(&conn, &db, MODE_SMILE_SPECIAL);

    // The terms SMILE answers hash to the fingerprint computed outside Rust.
    let inputs = writer
        .read_extra_bed_inputs(Some(VICTORY_FINGERPRINT), None)
        .unwrap()
        .unwrap();
    let terms = inputs.terms.unwrap();
    assert_eq!(terms.structural_fingerprint, VICTORY_FINGERPRINT);
    assert_eq!(terms.smile_hotel_id, "VIC");
    assert_eq!(terms.stock_total, Some(48));

    // The terms sync reads the same SMILE and the SpecialService catalogue.
    let smile = TdsSmile::new(&format!(
        "{};Database={}",
        conn.trim_end_matches(';'),
        db.name
    ))
    .expect("TdsSmile");
    let (read, catalog) = TermsSource::read(&smile, None, "EB").unwrap();
    assert_eq!(
        read.terms.unwrap().structural_fingerprint,
        VICTORY_FINGERPRINT
    );
    let catalog = catalog.unwrap().unwrap();
    assert_eq!(catalog.items.len(), 1);
    assert_eq!(catalog.items[0].tax_label.as_deref(), Some("V8/S5"));
    assert_eq!(catalog.rate_code_plus_plus, Some(true));

    // Create: one agent row at the grossed-up price, ExtraBed 1, the rate without the bed.
    let a = ack(d.process(&job("c1", "Create", 1)));
    assert!(a.success, "create failed: {:?} {:?}", a.error_code, a.error);
    let map = &a.folios.unwrap()[0];
    assert_eq!(map.extra_bed_written, Some(1));
    assert_eq!(map.extra_bed_special_qty_written, Some(1));
    assert_eq!(count(&db, MM_ROWS), 1);
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio_Special WHERE FolioNum = 7001 AND SpecialCode = 'EB'
             AND Qty = 1 AND Price = 453600 AND FromDay = 1 AND ToDay = 2 AND CancelNum = 0 AND SalesPerson IS NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio WHERE FolioNum = 7001 AND ExtraBed = 1 AND RateAmount = 1300000 AND UserInfo3 IS NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM FolioRate WHERE FolioNum = 7001 AND RateAmount = 1300000"
        ),
        2
    );

    // A replayed create and an unchanged modify keep the same single row (same nSeq).
    let first_seq = count(&db, "SELECT CAST(MIN(nSeq) AS bigint) FROM Folio_Special");
    assert!(ack(d.process(&job("c1", "Create", 1))).success);
    let m = ack(d.process(&job("m0", "Modify", 1)));
    assert!(m.success, "{:?} {:?}", m.error_code, m.error);
    assert_eq!(m.folios.unwrap()[0].extra_bed_special_qty_found, Some(1));
    assert_eq!(
        count(&db, "SELECT CAST(COUNT(*) AS bigint) FROM Folio_Special"),
        1
    );
    assert_eq!(
        count(&db, "SELECT CAST(MIN(nSeq) AS bigint) FROM Folio_Special"),
        first_seq
    );

    // A front-desk row of the same code is never touched; two beds rewrite the agent row.
    run(
        &db,
        "INSERT INTO Folio_Special (FolioNum, SpecialCode, Qty, Price, FromDay, ToDay, SPNotes, CancelNum)
         VALUES (7001, 'EB', 1, 100000, 1, 1, N'desk', 0)",
    );
    let m = ack(d.process(&job("m1", "Modify", 2)));
    assert!(m.success, "{:?} {:?}", m.error_code, m.error);
    let map = &m.folios.unwrap()[0];
    assert_eq!(map.extra_bed_special_qty_found, Some(2), "1 agent + 1 desk");
    assert_eq!(map.extra_bed_special_qty_written, Some(2));
    assert_eq!(
        count(&db, &format!("{MM_ROWS} AND Qty = 2 AND Price = 453600")),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio_Special WHERE SPNotes = N'desk' AND Qty = 1"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio WHERE FolioNum = 7001 AND ExtraBed = 2"
        ),
        1
    );

    // Switching back to FOLD_LEGACY deletes the agent row and puts the bed in the note.
    let (_, fold) = dispatcher(&conn, &db, MODE_FOLD_LEGACY);
    let f = ack(fold.process(&job("m2", "Modify", 2)));
    assert!(f.success, "{:?} {:?}", f.error_code, f.error);
    assert_eq!(f.folios.unwrap()[0].extra_bed_written, Some(0));
    assert_eq!(count(&db, MM_ROWS), 0);
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio_Special WHERE SPNotes = N'desk'"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio WHERE FolioNum = 7001 AND ExtraBed = 0 AND RateAmount = 2100000 AND UserInfo3 LIKE 'MM: +2 %'"
        ),
        1
    );

    // Cancel still works with the rows in place.
    assert!(ack(d.process(&job("x1", "Cancel", 2))).success);
    assert_eq!(
        count(
            &db,
            "SELECT CAST(COUNT(*) AS bigint) FROM Folio WHERE FolioNum = 7001 AND CancelNum > 0"
        ),
        1
    );
}
