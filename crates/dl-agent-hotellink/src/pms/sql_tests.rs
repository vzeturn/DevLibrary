//! Port of Go `repo_test.go`: the repository against a real SQL Server.
//!
//! Gate: `HL_AGENT_TEST_SQL` = a connection string to `master` containing
//! `database=master`, e.g.
//! `server=127.0.0.1,14333;user id=sa;password=…;database=master;encrypt=disable`.
//! The tests create `SMILE_FO`, apply `tests/fixtures/smile-schema.sql` (the Go repo's
//! `deploy/smile-schema.sql`, idempotent) and empty the tables before each test; they run
//! one at a time because they share the database.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use dl_agent::sql::{Pool, PoolOptions, TdsConnector};
use dl_tds::Param;
use dl_time::{Date, OffsetDateTime};

use super::repo::{exec, rows};
use super::{AdditionalName, Folio, FolioRate, FullRepo, PmsRepo, SqlRepo, Val};
use crate::reconcile::tests::day;

const ENV: &str = "HL_AGENT_TEST_SQL";
const SCHEMA: &str = include_str!("../../tests/fixtures/smile-schema.sql");
const TABLES: [&str; 9] = [
    "Folio",
    "FolioRate",
    "AdditionalName",
    "CLIENT",
    "ClientContacts",
    "ClientSts",
    "RoomTypeMapped",
    "GlobalParameter",
    "AvailabilitySeed",
];

static SERIAL: Mutex<()> = Mutex::new(());

struct Db {
    repo: SqlRepo,
    _serial: MutexGuard<'static, ()>,
}

fn pool(conn: &str) -> Arc<dl_agent::sql::SqlPool> {
    let c = TdsConnector::parse(conn).expect("connection string");
    Arc::new(Pool::new("hl-test", c, PoolOptions::default()))
}

fn smile_fo(master: &str) -> String {
    let lower = master.to_ascii_lowercase();
    let at = lower
        .find("database=master")
        .unwrap_or_else(|| panic!("{ENV} must contain database=master"));
    format!(
        "{}database=SMILE_FO{}",
        &master[..at],
        &master[at + "database=master".len()..]
    )
}

/// `newTestRepo`: `None` (skip) without the gate.
fn db() -> Option<Db> {
    let master = std::env::var(ENV).ok().filter(|v| !v.is_empty())?;
    let serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    SqlRepo::new(pool(&master))
        .with_conn(|c| {
            exec(
                c,
                "IF DB_ID('SMILE_FO') IS NULL CREATE DATABASE SMILE_FO",
                &[],
            )
        })
        .expect("create SMILE_FO");
    let repo = SqlRepo::new(pool(&smile_fo(&master)));
    apply_schema(&repo);
    for t in TABLES {
        run(&repo, &format!("DELETE FROM {t}"));
    }
    Some(Db {
        repo,
        _serial: serial,
    })
}

/// `applySchema`: batches split on `GO` lines, skipping `CREATE DATABASE` / `USE`.
fn apply_schema(repo: &SqlRepo) {
    let mut batch = String::new();
    let mut batches = Vec::new();
    for line in SCHEMA.lines() {
        if line.trim().eq_ignore_ascii_case("GO") {
            batches.push(std::mem::take(&mut batch));
        } else {
            batch.push_str(line);
            batch.push('\n');
        }
    }
    batches.push(batch);
    for b in batches {
        let b = b.trim();
        if b.is_empty()
            || b.contains("CREATE DATABASE")
            || b.to_ascii_uppercase().starts_with("USE ")
        {
            continue;
        }
        run(repo, b);
    }
}

/// `mustExec`.
fn run(repo: &SqlRepo, sql: &str) {
    if let Err(e) = repo.with_conn(|c| exec(c, sql, &[])) {
        panic!("exec failed: {}\n{sql}", e.message());
    }
}

fn scalar(repo: &SqlRepo, sql: &str, folio_num: f64) -> f64 {
    repo.with_conn(|c| {
        let r = rows(c, sql, &[Param::new("p1", folio_num)])?;
        let row = r.first().expect("one row");
        match row.get::<f64>(0) {
            Ok(v) => Ok(v),
            Err(_) => Ok(f64::from(row.get::<i32>(0)?)),
        }
    })
    .expect("scalar")
}

fn now() -> dl_time::DateTime {
    OffsetDateTime::now_utc().datetime()
}

/// `sampleFolio`.
fn sample_folio(folio_num: f64) -> (Folio, Vec<FolioRate>, AdditionalName) {
    let mut f = Folio::new(now());
    f.folio_num = folio_num;
    f.arrival_date = day(2026, 6, 12);
    f.departure_date = day(2026, 6, 14);
    f.room_type_code = "DLX01".into();
    f.booker = "EXT-1".into();
    f.external_confirm_number = Some("123456".into());
    f.confirm_num = 500001.0;
    f.last_name = "NGUYEN VAN DUC".into();
    f.rate_amount = 1500000.0;
    let rate = |d| FolioRate {
        folio_num,
        h_date: d,
        to_date: d,
        rate_amount: 1500000.0,
        rate_override: 1,
        ..FolioRate::default()
    };
    let rates = vec![rate(day(2026, 6, 12)), rate(day(2026, 6, 13))];
    let mut an = AdditionalName::new();
    an.folio_num = folio_num;
    an.last_name = f.last_name.clone();
    an.arrival_time = f.arrival_date;
    an.departure_time = f.departure_date;
    an.confirm_num = f.confirm_num;
    (f, rates, an)
}

fn insert(repo: &SqlRepo, (f, rates, an): &(Folio, Vec<FolioRate>, AdditionalName)) {
    repo.insert_folio_with_rates(f, rates, an).expect("insert");
}

#[test]
fn insert_folio_round_trip() {
    let Some(db) = db() else { return };
    insert(&db.repo, &sample_folio(100001.0));
    let got = db.repo.get_folios("123456").expect("get_folios");
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].folio_num, got[0].confirm_num, got[0].book_status),
        (100001.0, 500001.0, 1)
    );
    assert_eq!(
        scalar(
            &db.repo,
            "SELECT COUNT(*) FROM FolioRate WHERE FolioNum = @p1",
            100001.0
        ),
        2.0
    );
    assert_eq!(
        scalar(
            &db.repo,
            "SELECT COUNT(*) FROM AdditionalName WHERE FolioNum = @p1",
            100001.0
        ),
        1.0
    );
}

#[test]
fn insert_folio_atomic_rollback() {
    let Some(db) = db() else { return };
    let (f, rates, mut an) = sample_folio(100002.0);
    an.last_name = "X".repeat(300);
    assert!(
        db.repo.insert_folio_with_rates(&f, &rates, &an).is_err(),
        "too long a name must fail"
    );
    assert_eq!(
        scalar(
            &db.repo,
            "SELECT COUNT(*) FROM Folio WHERE FolioNum = @p1",
            100002.0
        ),
        0.0
    );
    assert_eq!(
        scalar(
            &db.repo,
            "SELECT COUNT(*) FROM FolioRate WHERE FolioNum = @p1",
            100002.0
        ),
        0.0
    );
}

#[test]
fn cancel_folios_sets_both_tables() {
    let Some(db) = db() else { return };
    insert(&db.repo, &sample_folio(100003.0));
    let folios = db.repo.get_folios("123456").expect("get");
    db.repo.cancel_folios(&folios).expect("cancel");
    let cancel = scalar(
        &db.repo,
        "SELECT CancelNum FROM Folio WHERE FolioNum = @p1",
        100003.0,
    );
    let a_cancel = scalar(
        &db.repo,
        "SELECT ACancelNum FROM AdditionalName WHERE FolioNum = @p1",
        100003.0,
    );
    assert!(
        cancel != 0.0 && cancel == a_cancel,
        "CancelNum={cancel} ACancelNum={a_cancel}"
    );
    assert!(db.repo.get_folios("123456").expect("get").is_empty());
}

#[test]
fn procs_return_numbers() {
    let Some(db) = db() else { return };
    assert!(db.repo.get_next_folio_num().expect("folio num") > 0.0);
    assert!(db.repo.get_new_confirm_num().expect("confirm num") > 0.0);
}

#[test]
fn global_param() {
    let Some(db) = db() else { return };
    assert_eq!(
        db.repo
            .get_global_param_int("HLInventorySync")
            .expect("absent"),
        0
    );
    run(
        &db.repo,
        "INSERT INTO GlobalParameter VALUES ('HLInventorySync', '1')",
    );
    assert_eq!(
        db.repo
            .get_global_param_int("HLInventorySync")
            .expect("set"),
        1
    );
}

#[test]
fn seed_global_param_if_absent_inserts_and_never_overwrites() {
    let Some(db) = db() else { return };
    db.repo
        .seed_global_param_if_absent("HLInventorySync", "1")
        .expect("seed");
    assert_eq!(
        db.repo
            .get_global_param_int("HLInventorySync")
            .expect("get"),
        1
    );
    run(
        &db.repo,
        "UPDATE GlobalParameter SET ParaString = '0' WHERE ParaName = 'HLInventorySync'",
    );
    db.repo
        .seed_global_param_if_absent("HLInventorySync", "1")
        .expect("seed again");
    assert_eq!(
        db.repo
            .get_global_param_int("HLInventorySync")
            .expect("get"),
        0,
        "an explicit 0 stays"
    );
}

#[test]
fn room_type_mapped_and_availability() {
    let Some(db) = db() else { return };
    run(
        &db.repo,
        "INSERT INTO RoomTypeMapped VALUES ('r-100', 'DLX01', NULL, NULL, 1), (NULL, 'STD', 0, 2, 0)",
    );
    let rows = db.repo.get_room_type_mapped().expect("mapping");
    assert_eq!(rows.len(), 2);
    let std = rows
        .iter()
        .find(|m| m.room_type_code == "STD")
        .expect("STD");
    assert_eq!(
        (std.room_id.as_str(), std.split_part, std.total_part),
        ("", Some(0), Some(2))
    );
    run(
        &db.repo,
        "INSERT INTO AvailabilitySeed (RoomTypeCode, sDate, Definite, NumRoom) VALUES
         ('DLX01', '2026-06-12', 3, 10), ('DLX01', '2026-06-13', 0, 10), ('DLX01', '2026-07-01', 0, 10)",
    );
    let from = Date::parse_iso("2026-06-12").expect("date");
    let avail = db.repo.get_availability(from, 5).expect("availability");
    assert_eq!(avail.len(), 2, "{avail:?}");
    assert_eq!(
        (avail[0].date.as_str(), avail[0].available),
        ("2026-06-12", 7)
    );
}

#[test]
fn mapping_writes_round_trip() {
    let Some(db) = db() else { return };
    let row = crate::jobmodel::MappingRow {
        room_id: "r-1".into(),
        room_type_code: "DLX01".into(),
        ..Default::default()
    };
    db.repo.insert_mapping(&row).expect("insert");
    let mut moved = row.clone();
    moved.room_type_code = "STD".into();
    db.repo.update_mapping(&row, &moved).expect("update");
    let got = db.repo.get_room_type_mapped().expect("read");
    assert_eq!(
        got.iter()
            .map(|m| m.room_type_code.as_str())
            .collect::<Vec<_>>(),
        ["STD"]
    );
    db.repo.delete_mapping(&moved).expect("delete");
    assert!(db.repo.get_room_type_mapped().expect("read").is_empty());
    db.repo.test_connection().expect("ping");
}

#[test]
fn get_folios_dedup_by_external_confirm_and_isnull() {
    let Some(db) = db() else { return };
    for (num, booker) in [
        (200001.0, "BookerA"),
        (200002.0, "BookerB"),
        (200003.0, "BookerC"),
    ] {
        let mut s = sample_folio(num);
        s.0.booker = booker.into();
        s.0.external_confirm_number = Some("EXT-9".into());
        insert(&db.repo, &s);
    }
    let before = db.repo.get_folios("EXT-9").expect("get");
    assert_eq!(before.len(), 3);
    let third: Vec<Folio> = before
        .into_iter()
        .filter(|f| f.folio_num == 200003.0)
        .collect();
    db.repo.cancel_folios(&third).expect("cancel");
    let got = db.repo.get_folios("EXT-9").expect("get");
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|f| f.folio_num != 200003.0));
}

#[test]
fn cancel_folios_rolls_back_on_second_table_error() {
    let Some(db) = db() else { return };
    insert(&db.repo, &sample_folio(100010.0));
    let folios = db.repo.get_folios("123456").expect("get");
    assert_eq!(folios.len(), 1);
    run(
        &db.repo,
        "EXEC sp_rename 'AdditionalName', 'AdditionalName_bak'",
    );
    let r = db.repo.cancel_folios(&folios);
    run(
        &db.repo,
        "EXEC sp_rename 'AdditionalName_bak', 'AdditionalName'",
    );
    assert!(r.is_err(), "cancel must fail without AdditionalName");
    assert_eq!(
        scalar(
            &db.repo,
            "SELECT CancelNum FROM Folio WHERE FolioNum = @p1",
            100010.0
        ),
        0.0
    );
}

#[test]
fn update_folio_fields_applies_when_cancel_num_null() {
    let Some(db) = db() else { return };
    let mut s = sample_folio(4242.0);
    s.0.num_adult = 1;
    insert(&db.repo, &s);
    run(
        &db.repo,
        "ALTER TABLE Folio ALTER COLUMN CancelNum DECIMAL(18,2) NULL",
    );
    run(
        &db.repo,
        "UPDATE Folio SET CancelNum = NULL WHERE FolioNum = 4242",
    );
    let mut set = BTreeMap::new();
    set.insert("NumAdult", Val::I32(3));
    let r = db.repo.update_folio_fields(4242.0, &set);
    run(
        &db.repo,
        "UPDATE Folio SET CancelNum = 0 WHERE CancelNum IS NULL",
    );
    run(
        &db.repo,
        "ALTER TABLE Folio ALTER COLUMN CancelNum DECIMAL(18,2) NOT NULL",
    );
    r.expect("update");
    assert_eq!(
        scalar(
            &db.repo,
            "SELECT NumAdult FROM Folio WHERE FolioNum = @p1",
            4242.0
        ),
        3.0
    );
}

#[test]
fn client_lookups() {
    let Some(db) = db() else { return };
    run(&db.repo, "INSERT INTO CLIENT VALUES (77, 'Booking.com')");
    run(
        &db.repo,
        "INSERT INTO ClientContacts VALUES (77, 'Agoda Contact')",
    );
    run(
        &db.repo,
        "INSERT INTO ClientSts (ClientCode, Origin, MSegment, UDef1) VALUES (77, 'OTA', 'ONLINE', 'U1')",
    );
    let c = db
        .repo
        .get_client_by_name("Booking.com")
        .expect("by name")
        .expect("found");
    assert_eq!(c.client_folio_num, 77.0);
    assert!(
        db.repo
            .get_client_by_name("Không có")
            .expect("miss")
            .is_none()
    );
    let contact = db
        .repo
        .get_client_contact_by_name("Agoda Contact")
        .expect("contact")
        .expect("found");
    assert_eq!(contact.client_folio_num, 77.0);
    let by_num = db
        .repo
        .get_client_by_folio_num(77.0)
        .expect("by num")
        .expect("found");
    assert_eq!(by_num.client_name, "Booking.com");
    let sts = db.repo.get_client_sts(77.0).expect("sts").expect("found");
    assert_eq!(
        (
            sts.origin.as_str(),
            sts.m_segment.as_str(),
            sts.u_def[0].as_str()
        ),
        ("OTA", "ONLINE", "U1")
    );
}

#[test]
fn get_hotel_date_and_empty_table() {
    let Some(db) = db() else { return };
    run(
        &db.repo,
        "IF OBJECT_ID('HotelParameter') IS NULL CREATE TABLE HotelParameter (HotelDate DATETIME NULL)",
    );
    run(&db.repo, "DELETE FROM HotelParameter");
    assert!(
        db.repo.get_hotel_date().is_err(),
        "an empty HotelParameter is an error"
    );
    run(
        &db.repo,
        "INSERT INTO HotelParameter (HotelDate) VALUES ('2026-06-08')",
    );
    assert_eq!(
        db.repo
            .get_hotel_date()
            .expect("hotel date")
            .date()
            .to_string(),
        "2026-06-08"
    );
}

#[test]
fn has_user_cancelled_folio_distinguishes_origin() {
    let Some(db) = db() else { return };
    insert(&db.repo, &sample_folio(100004.0));
    let folios = db.repo.get_folios("123456").expect("get");
    db.repo.cancel_folios(&folios).expect("cancel");
    assert!(
        !db.repo
            .has_user_cancelled_folio("123456", "")
            .expect("system cancel")
    );
    run(
        &db.repo,
        "UPDATE Folio SET LastChangeClerkID = 'frontdesk1' WHERE FolioNum = 100004",
    );
    assert!(
        db.repo
            .has_user_cancelled_folio("123456", "")
            .expect("user cancel")
    );
}
