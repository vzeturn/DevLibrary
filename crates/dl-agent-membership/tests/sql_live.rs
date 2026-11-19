//! Every SMILE read against a real SQL Server (opt-in).
//!
//! `MEMBERSHIP_AGENT_TEST_SQL` holds an ADO connection string to a SQL Server whose login
//! may create databases. The test builds a throw-away database from the MM-MEMBERSHIP SMILE
//! fixture schema (`tests/fixtures/smile_schema.sql`, copied from `SmileFixture.cs`), seeds
//! rooms, room types, a group block and folios, runs each `TdsSmile` read, then runs the
//! sync and scan workers over the real SMILE against the fake hub, and drops the database.
//! Without the variable the test returns at once. The connection string is never printed.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dl_agent_membership::folio::earn::{self, EarnPair};
use dl_agent_membership::hub::gate::TokenGate;
use dl_agent_membership::hub::{HubApi, HubClient};
use dl_agent_membership::identity::{IdentityProbe, Tracker};
use dl_agent_membership::module::TOKEN_RETRY;
use dl_agent_membership::module::scan::{ScanDeps, ScanLoop};
use dl_agent_membership::module::state::Shared;
use dl_agent_membership::settings::Settings;
use dl_agent_membership::smile::{
    AvailabilitySource, ExtraBedUsage, FolioSource, RoomSource, SmileHealth, TdsSmile, hotel_now,
    normalize_connection_string,
};
use dl_agent_membership::sync::{SyncSettings, SyncWorker};
use dl_json::{Value, json};
use dl_tds::{Client, Config};
use support::{FakeHub, HOTEL, TOKEN, TestDir};

const ENV: &str = "MEMBERSHIP_AGENT_TEST_SQL";
const SCHEMA: &str = include_str!("fixtures/smile_schema.sql");
const VERSION: &str = "dl-agent/test";

/// Synthetic SMILE data: two DLX rooms and one STD room (out of order for three days), an
/// in-house DLX folio with room revenue, a reservation for STD with an extra bed, and a
/// confirmed group block of one DLX room.
const SEED: &str = "\
DECLARE @today datetime = CAST(CAST(GETDATE() AS date) AS datetime);
INSERT INTO HotelParameter (HotelID, HotelDate) VALUES (NULL, @today);
INSERT INTO RoomType (RoomTypeCode, Description, NumRoom, DisplayOrder, TrnCode, TrnSubCode)
VALUES ('DLX', 'Deluxe', 2, 1, 0, 0), ('STD', 'Standard', 1, 2, 0, 0);
INSERT INTO Room (RoomCode, RoomName, RoomTypeCode, Floor, RStatus, HSKPClean, Inspected, TouchedUp)
VALUES ('101', 'Room 101', 'DLX', 1, 1, 1, 1, 0),
       ('102', 'Room 102', 'DLX', 1, 3, 0, 0, 0),
       ('201', 'Room 201', 'STD', 2, 1, 1, 0, 0);
INSERT INTO RoomOOS (RoomCode, SetTime, SetBy, isCleared, OOSCode, FromDate, ToDate)
VALUES ('201', GETDATE(), 'T', 0, 'OOO', @today, DATEADD(day, 2, @today));
INSERT INTO TransactionCode (TransactionCode, TransactionSubCode, Description, AutoPost)
VALUES ('10', '00', 'Room charge', 1), ('20', '00', 'Minibar', 0);
INSERT INTO SpecialService (SpecialServiceCode, Description, IncludeInRoomCharge, TrnCode, TrnSubCode)
VALUES ('EB', 'Extra bed', 0, '30', '00');
INSERT INTO Folio (FolioNum, FolioSubNum, ConfirmNum, ArrivalDate, DepartureDate, CheckInTime,
                   LastChangeTime, LastChangeClerkID, FolioStatus, RoomCode, RoomTypeCode,
                   RoomTypeBooked, NumAdult, NumChild, LastName, NumRoom, ExtraBed, NumEnf)
VALUES (1001, 0, 5001, DATEADD(day, -1, @today), DATEADD(day, 2, @today), DATEADD(hour, -20, GETDATE()),
        DATEADD(hour, -1, GETDATE()), 'FO', 2, '101', 'DLX', 'DLX', 2, 0, 'Sample', 1, 0, 0),
       (1002, 0, 5002, @today, DATEADD(day, 1, @today), NULL,
        DATEADD(minute, -30, GETDATE()), 'FO', 1, NULL, 'STD', 'STD', 1, 0, 'Sample', 1, 1, 0);
INSERT INTO AdditionalName (FolioNum, FolioSubNum, FirstName, LastName, MobileNum)
VALUES (1001, 0, 'Guest', 'One', '0900000001'), (1002, 0, 'Guest', 'Two', '0900000002');
INSERT INTO FolioTransaction (FolioNum, TransactionCode, TransactionSubCode, TransactionAmount,
                              FolioBalanceCode, SystemTime, VoidTransactionFlag)
VALUES ('1001', '10', 0, 1500000, 'A', GETDATE(), 0), ('1001', '20', 0, 50000, 'A', GETDATE(), 0);
INSERT INTO Folio_Special (FolioNum, SpecialCode, Qty, FromDay, ToDay, CancelNum)
VALUES (1002, 'EB', 1, 0, 1, 0);
INSERT INTO GroupFolio (GroupCode, GroupStatusCode, IsCanceled, ConfirmStatus)
VALUES ('GRP1', '1', 0, 1);
INSERT INTO GroupBlockDetail (AllocationID, RoomTypeCode, FromDate, ToDate, NumRoom, NumPickup, Cancelled)
VALUES ('GRP1', 'DLX', DATEADD(day, 3, @today), DATEADD(day, 5, @today), 1, 0, 0);";

/// Connects, retrying while SQL Server finishes starting (logins fail for a few seconds
/// after the container reports it is ready).
fn connect(cfg: &Config) -> Client {
    let mut last = String::new();
    for _ in 0..30 {
        match Client::connect(cfg) {
            Ok(c) => return c,
            Err(e) => last = format!("{:?} {}", e.number(), if e.is_tls() { "tls" } else { "" }),
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("SQL Server login failed after 30 tries (error {last})");
}

/// The throw-away database; dropped (with open sessions killed) when the test ends.
struct Db {
    master: Config,
    name: String,
}

impl Drop for Db {
    fn drop(&mut self) {
        let mut c = connect(&self.master);
        let sql = format!(
            "ALTER DATABASE [{0}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{0}];",
            self.name
        );
        if let Err(e) = c.batch(&sql) {
            eprintln!("could not drop test database {}: {e}", self.name);
        }
    }
}

fn create(conn: &str) -> (Db, Config) {
    let base = normalize_connection_string(conn)
        .unwrap_or_else(|_| panic!("{ENV} is not a usable connection string"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("dl_mb_{}_{nanos}", std::process::id());
    let master = base.clone().with_database("master");
    connect(&master)
        .batch(&format!("CREATE DATABASE [{name}];"))
        .expect("CREATE DATABASE");
    let db = base.with_database(&name);
    let mut c = connect(&db);
    let batches: Vec<&str> = SCHEMA
        .split("\nGO\n")
        .map(str::trim)
        .filter(|b| !b.is_empty())
        .collect();
    assert!(
        batches.len() > 20,
        "schema split into {} batches",
        batches.len()
    );
    for (i, b) in batches.iter().enumerate() {
        c.batch(b)
            .unwrap_or_else(|e| panic!("schema batch {i}: {e}"));
    }
    c.batch(SEED).unwrap_or_else(|e| panic!("seed: {e}"));
    c.close();
    (
        Db {
            master,
            name: name.clone(),
        },
        db,
    )
}

#[test]
fn every_smile_read_runs_against_sql_server() {
    let Ok(conn) = std::env::var(ENV) else {
        eprintln!("skipped: {ENV} is not set");
        return;
    };
    let (db, _) = create(&conn);
    let smile = Arc::new(
        TdsSmile::new(&format!(
            "{};Database={}",
            conn.trim_end_matches(';'),
            db.name
        ))
        .expect("TdsSmile"),
    );
    direct_reads(&smile, &db.name);
    workers_over_real_smile(smile);
}

fn direct_reads(smile: &TdsSmile, db: &str) {
    smile.ping().expect("ping");
    let snap = smile.read_identity().expect("identity");
    assert_eq!(snap.identity.database, db);
    assert!(!snap.identity.server.is_empty());
    assert!(snap.identity.database_guid.is_some(), "database_guid");
    assert!(
        snap.last_foreign_change.is_some(),
        "a clerk changed a folio"
    );

    let rooms = smile.scan_rooms().expect("rooms");
    assert_eq!(rooms.len(), 3);
    let r101 = rooms.iter().find(|r| r.room_code == "101").unwrap();
    assert!(r101.is_occupied, "{r101:?}");
    let r201 = rooms.iter().find(|r| r.room_code == "201").unwrap();
    assert!(r201.out_of_order || r201.out_of_service, "{r201:?}");
    let types = smile.room_types().expect("room types");
    assert_eq!(types.len(), 2);
    assert_eq!(types[0].pms_room_type_code, "DLX");

    let today = hotel_now().date();
    let (from, to) = (today, today.add_days(30));
    let probe = smile.probe_occupancy(from, to).expect("probe");
    assert!(probe.folio_count >= 2 && probe.room_count == 3, "{probe:?}");
    assert!(probe.hotel_date.is_some());
    let stays = smile.scan_occupancy(from, to, 1000).expect("occupancy");
    assert_eq!(stays.len(), 2, "{stays:?}");
    let blocks = smile.scan_room_blocks(from, to).expect("room blocks");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].room_code, "201");
    let groups = smile.scan_group_blocks(from, to).expect("group blocks");
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(
        (groups[0].group_code.as_str(), groups[0].num_room),
        ("GRP1", 1)
    );
    match smile
        .extra_bed_usage(from, to, 1000, "EB")
        .expect("extra beds")
    {
        ExtraBedUsage::Beds(beds) => assert_eq!(beds.get(&today), Some(&1), "{beds:?}"),
        ExtraBedUsage::Unknown(why) => panic!("extra beds unknown: {why}"),
    }

    let catalog = smile.earn_catalog("EB").expect("earn catalog");
    assert_eq!(catalog.transaction_codes.len(), 2);
    assert_eq!(catalog.specials.len(), 1);
    let watermark = today.add_days(-2).midnight();
    let rm = smile.room_charge_trn_since(watermark).expect("RmChgTrn");
    let set = earn::resolve(&catalog, &rm);
    assert!(!set.is_ambiguous());
    let columns = smile.occupancy_columns().expect("columns");
    assert!(columns.num_enf && columns.extra_bed && columns.has_special);
    let folios = smile
        .changed_folios(watermark, 100, columns)
        .expect("folios");
    assert_eq!(folios.len(), 2);
    assert!(folios[0].last_change_time <= folios[1].last_change_time);
    let nums: Vec<String> = vec!["1001".into(), "1002".into()];
    let revenue = smile.revenues(&nums, &set.pairs).expect("revenues");
    assert_eq!(revenue.len(), 1, "only room revenue earns: {revenue:?}");
    assert_eq!(revenue[0].txn_count, 1);
    let any_sub = smile
        .revenues(&nums, &[EarnPair::any_sub("10"), EarnPair::any_sub("20")])
        .expect("revenues, any sub");
    assert_eq!(any_sub[0].txn_count, 2);
    let decimals: Vec<_> = folios.iter().map(|f| f.folio_num).collect();
    let guests = smile.guests(&decimals, "[MobileNum]").expect("guests");
    assert_eq!(guests.len(), 2);
    assert!(guests.iter().all(|g| g.phone.is_some() && g.name.is_some()));
    let special = smile
        .extra_bed_special_qty(&decimals, "EB", columns.special_has_cancel_num)
        .expect("extra bed specials");
    assert_eq!(special.len(), 1);
}

/// The sync worker and the scan loop over the real SMILE: each scanner posts to the fake
/// hub and nothing is reported.
fn workers_over_real_smile(smile: Arc<TdsSmile>) {
    let hub = FakeHub::start();
    let dir = TestDir::new("sql");
    let client: Arc<dyn HubApi> = Arc::new(
        HubClient::new(
            &hub.url(),
            TOKEN,
            HOTEL.parse().unwrap(),
            VERSION,
            Arc::new(TokenGate::new(TOKEN_RETRY)),
        )
        .unwrap(),
    );
    let tracker = Arc::new(Tracker::new(smile.clone()));
    let shared = Arc::new(Shared::default());
    let mut sync = SyncWorker::new(
        client.clone(),
        smile.clone(),
        tracker.clone(),
        shared.clone(),
        SyncSettings {
            pull_jobs: false,
            max_jobs_per_pull: 20,
            heartbeat: Duration::from_secs(30),
            capabilities: vec!["availability".into()],
        },
        VERSION,
    );
    sync.cycle(Instant::now());
    let hb = dl_json::parse(&hub.to("/api/agent/heartbeat")[0].body).unwrap();
    assert_eq!(hb.get("SmileOk").and_then(Value::as_bool), Some(true));
    let settings = Settings::from_config(
        &json!({"program_data_dir": dir.0.display().to_string()}),
        &dir.0,
    );
    let deps = ScanDeps {
        src: smile,
        hub: client,
        tracker,
        shared: shared.clone(),
    };
    ScanLoop::new(deps, &settings, HOTEL, VERSION).turn(Instant::now());

    let reports = shared.take_reports();
    assert!(
        reports.is_empty(),
        "{:?}",
        reports.iter().map(|r| &r.code).collect::<Vec<_>>()
    );
    for path in [
        "/api/agent/availability",
        "/api/agent/room-inventory",
        "/api/agent/room-types",
        "/api/agent/folio-observations",
    ] {
        assert_eq!(hub.to(path).len(), 1, "{path}: {:?}", hub.lines());
    }
    let obs = dl_json::parse(&hub.to("/api/agent/folio-observations")[0].body).unwrap();
    let obs = obs.get("Observations").and_then(Value::as_array).unwrap();
    assert_eq!(obs.len(), 2);
    let stats = shared.stats();
    for scanner in ["availability", "rooms", "folio"] {
        assert_eq!(stats.scans[scanner].errors, 0, "{scanner}");
    }
}
