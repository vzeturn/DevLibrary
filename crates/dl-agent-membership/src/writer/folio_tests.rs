//! `FolioWriter` against the recording session: statement order, transactions, the
//! duplicate guards, and golden SQL (`tests/fixtures/golden/sql/*.json`). Set
//! `GOLDEN_SQL_WRITE=1` to rewrite a fixture after a reviewed change.

use dl_tds::Value;

use super::*;
use crate::jobs::payload::parse_payload;
use crate::jobs::plan::{BookingPlan, WriteContext};
use crate::writer::applock::{CANCEL_NUMBER_LOCK, FOLIO_NUMBER_LOCK};
use crate::writer::recorder::Recorder;
use crate::writer::schema::OPTIONAL_COLUMNS;
use crate::writer::session::{Called, Rec};

const ROOM_ID: &str = "11111111-2222-3333-4444-555555555555";
const BOOKING_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn room(extra: &str) -> Room {
    let json = format!(
        r#"{{"rooms":[{{"Id":"{ROOM_ID}","RoomTypeCode":"DLX","Arrival":"2026-12-01",
        "Departure":"2026-12-03","NumAdult":2,"NumChild":0,"GuestFirstName":"An",
        "GuestLastName":"Nguyễn Test","nights":[{{"Date":"2026-12-01","Net":1000000}},
        {{"Date":"2026-12-02","Net":1200000}}]{extra}}}]}}"#
    );
    parse_payload(&json).unwrap().rooms.remove(0)
}

fn ctx(force: bool, note: Option<&str>) -> WriteContext {
    WriteContext {
        confirmation_code: "MM-TEST-1".into(),
        booking_id: BOOKING_ID.parse().unwrap(),
        origin_code: "MEM".into(),
        member_id: None,
        force_create: force,
        note: note.map(str::to_owned),
        travel_agent: None,
    }
}

fn plan(ctx: WriteContext) -> BookingPlan {
    BookingPlan {
        ctx,
        shared_confirm_num: None,
        profile_number: Decimal::from(77i64),
        creates: Vec::new(),
        updates: Vec::new(),
        cancels: Vec::new(),
        note_folios: None,
    }
}

fn dec(n: i64) -> Value {
    Value::Decimal(Decimal::from(n))
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

/// A recorder whose schema probe sees every whitelisted column (plus `IsChild`).
fn full_install() -> Recorder {
    let mut r = Recorder::default();
    r.on("INFORMATION_SCHEMA.COLUMNS", |st| {
        let table = st.params[0].2.clone().unwrap_or_default();
        let mut cols: Vec<&str> = OPTIONAL_COLUMNS
            .iter()
            .find(|(t, _)| *t == table)
            .map(|(_, c)| c.to_vec())
            .unwrap_or_default();
        if table == "AdditionalName" {
            cols.push("IsChild");
        }
        Ok(Called {
            rows: cols
                .into_iter()
                .map(|c| Rec::new(vec![("Name", s(c)), ("MaxLength", Value::Null)]))
                .collect(),
            status: Some(0),
        })
    });
    r.rows(
        "GetFolioNumber",
        vec![Rec::new(vec![
            ("FolioNumber", dec(5001)),
            ("FolioSubNumber", Value::I32(0)),
        ])],
    );
    r.rows(
        "GetConfirmNum",
        vec![Rec::new(vec![("ConfNumber", dec(9001))])],
    );
    r.rows(
        "GetCancelNum",
        vec![Rec::new(vec![("CancelNumber", dec(301))])],
    );
    r
}

fn golden(name: &str, r: &Recorder) {
    let path = format!(
        "{}/tests/fixtures/golden/sql/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let got = dl_json::to_string_pretty(&r.to_json());
    if std::env::var_os("GOLDEN_SQL_WRITE").is_some() {
        std::fs::write(&path, format!("{got}\n")).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(got.trim_end(), want.trim_end(), "golden SQL {name}");
}

fn position(r: &Recorder, needle: &str) -> usize {
    r.texts()
        .iter()
        .position(|t| t.contains(needle))
        .unwrap_or_else(|| panic!("no statement containing {needle}"))
}

#[test]
fn create_runs_in_one_locked_transaction_in_dotnet_order() {
    let mut r = full_install();
    let mut p = plan(ctx(false, None));
    p.creates.push(room(""));
    let w = FolioWriter::default().write_booking(&mut r, &p).unwrap();
    assert_eq!(
        w.created,
        vec![(ROOM_ID.parse().unwrap(), Decimal::from(5001i64))]
    );
    assert_eq!(w.confirm_num, Some(Decimal::from(9001i64)));
    let texts = r.texts();
    assert!(texts.contains(&"SET LOCK_TIMEOUT 10000"));
    let order = [
        "BEGIN TRANSACTION",
        "sp_getapplock",
        "ExternalConfirmNumber = @Ext AND UserInfo1 = @Room",
        "ISNULL(ExternalConfirmNumber, '') = ''",
        "GetFolioNumber",
        "GetConfirmNum",
        "INSERT INTO Folio ",
        "INSERT INTO FolioRate",
        "INSERT INTO AdditionalName",
        "COMMIT TRANSACTION",
        "SET LOCK_TIMEOUT -1",
    ];
    let idx: Vec<usize> = order.iter().map(|n| position(&r, n)).collect();
    assert!(idx.windows(2).all(|w| w[0] < w[1]), "{idx:?}");
    let applock = &r.log[position(&r, "sp_getapplock")];
    assert!(
        applock
            .params
            .iter()
            .any(|(_, _, v)| v.as_deref() == Some(FOLIO_NUMBER_LOCK))
    );
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.contains("INSERT INTO FolioRate"))
            .count(),
        2
    );
    assert_eq!(
        texts
            .iter()
            .filter(|t| t.contains("INSERT INTO AdditionalName"))
            .count(),
        2
    );
    golden("create_booking", &r);
}

#[test]
fn the_folio_insert_keeps_names_in_tcvn3_and_first_night_rate() {
    let mut r = full_install();
    let mut p = plan(ctx(false, None));
    p.creates.push(room(""));
    FolioWriter::default().write_booking(&mut r, &p).unwrap();
    let ins = &r.log[position(&r, "INSERT INTO Folio ")];
    let param = |n: &str| {
        ins.params
            .iter()
            .find(|(name, _, _)| name == n)
            .unwrap_or_else(|| panic!("{n}"))
            .clone()
    };
    let last = param("LastName");
    assert_eq!(last.1, "AnsiString");
    assert_eq!(
        last.2.as_deref(),
        Some(tcvn3::encode("Nguyễn Test").as_str())
    );
    assert_eq!(param("RateAmount").2.as_deref(), Some("1000000"));
    assert_eq!(
        param("ExternalConfirmNumber").2.as_deref(),
        Some("MM-TEST-1")
    );
    assert_eq!(param("UserInfo1").2.as_deref(), Some(ROOM_ID));
}

#[test]
fn a_replayed_room_updates_instead_of_inserting_twice() {
    let mut r = full_install();
    r.rows(
        "ExternalConfirmNumber = @Ext AND UserInfo1 = @Room",
        vec![Rec::new(vec![
            ("FolioNum", dec(4000)),
            ("ConfirmNum", dec(8000)),
        ])],
    );
    r.rows(
        "SELECT ConfirmNum, OriginCode FROM Folio",
        vec![Rec::new(vec![
            ("ConfirmNum", dec(8000)),
            ("OriginCode", s("MEM")),
        ])],
    );
    let mut p = plan(ctx(false, None));
    p.creates.push(room(""));
    let w = FolioWriter::default().write_booking(&mut r, &p).unwrap();
    assert_eq!(w.created[0].1, Decimal::from(4000i64));
    assert_eq!(w.confirm_num, Some(Decimal::from(8000i64)));
    let texts = r.texts();
    assert!(!texts.iter().any(|t| t.contains("GetFolioNumber")));
    assert!(!texts.iter().any(|t| t.starts_with("INSERT INTO Folio ")));
    assert!(texts.iter().any(|t| t.starts_with("UPDATE Folio SET")));
    assert!(texts.contains(&"DELETE FROM FolioRate WHERE FolioNum = @FolioNum"));
}

#[test]
fn a_hand_entered_twin_refuses_the_create_and_rolls_back() {
    let mut r = full_install();
    r.rows(
        "ISNULL(ExternalConfirmNumber, '') = ''",
        vec![Rec::new(vec![
            ("FolioNum", dec(3000)),
            ("LastName", s(&tcvn3::encode("NGUYEN  test"))),
        ])],
    );
    let mut p = plan(ctx(false, None));
    p.creates.push(room(""));
    let e = FolioWriter::default()
        .write_booking(&mut r, &p)
        .unwrap_err();
    match e {
        WriteError::Refused { code, message } => {
            assert_eq!(code, crate::jobs::result::POSSIBLE_DUPLICATE);
            assert!(message.starts_with("Folio 3000 trên SMILE"));
        }
        other => panic!("{other:?}"),
    }
    let texts = r.texts();
    assert!(texts.iter().any(|t| t.contains("ROLLBACK")));
    assert!(!texts.iter().any(|t| t.contains("GetFolioNumber")));
}

#[test]
fn force_skips_the_hand_entered_guard() {
    let mut r = full_install();
    r.rows(
        "ISNULL(ExternalConfirmNumber, '') = ''",
        vec![Rec::new(vec![
            ("FolioNum", dec(3000)),
            ("LastName", s("NGUYEN TEST")),
        ])],
    );
    let mut p = plan(ctx(true, None));
    p.creates.push(room(""));
    FolioWriter::default().write_booking(&mut r, &p).unwrap();
    assert!(
        !r.texts()
            .iter()
            .any(|t| t.contains("ISNULL(ExternalConfirmNumber, '') = ''"))
    );
}

#[test]
fn cancel_takes_the_cancel_lock_and_marks_guests() {
    let mut r = full_install();
    let mut p = plan(ctx(false, None));
    p.cancels.push(Decimal::from(4000i64));
    FolioWriter::default().write_booking(&mut r, &p).unwrap();
    let locks: Vec<Option<String>> = r
        .log
        .iter()
        .filter(|st| st.text == "sp_getapplock")
        .map(|st| st.params[0].2.clone())
        .collect();
    assert_eq!(
        locks,
        vec![
            Some(FOLIO_NUMBER_LOCK.to_owned()),
            Some(CANCEL_NUMBER_LOCK.to_owned())
        ]
    );
    let cancel = position(&r, "GetCancelNum");
    let upd = position(&r, "UPDATE Folio SET");
    let guests = position(&r, "UPDATE AdditionalName SET ACancelNum = @CancelNum");
    assert!(cancel < upd && upd < guests);
    golden("cancel_folio", &r);
}

#[test]
fn nothing_to_write_returns_the_shared_number_without_sql() {
    let mut r = full_install();
    let mut p = plan(ctx(false, None));
    p.shared_confirm_num = Some(Decimal::from(12i64));
    let w = FolioWriter::default().write_booking(&mut r, &p).unwrap();
    assert_eq!(w.confirm_num, Some(Decimal::from(12i64)));
    assert!(r.log.is_empty());
}

#[test]
fn discount_calls_post_one_transaction_with_twenty_parameters() {
    let mut r = Recorder::default();
    let now = dl_time::DateTime::parse_iso("2026-12-01T14:05:09").unwrap();
    FolioWriter::default()
        .post_discount(
            &mut r,
            now,
            Decimal::from(5001i64),
            Decimal::from(150000i64),
            "DSC",
            "MMV:abc",
        )
        .unwrap();
    assert_eq!(r.log.len(), 1);
    assert_eq!(r.log[0].kind, "proc");
    assert_eq!(r.log[0].params.len(), 20);
    golden("post_discount", &r);
}

#[test]
fn posted_ref_guard_cuts_the_ref_and_uses_varchar() {
    let mut r = Recorder::default();
    r.rows(
        "FROM FolioTransaction",
        vec![Rec::new(vec![("", Value::I32(1))])],
    );
    let long = "R".repeat(40);
    let hit = FolioWriter::default()
        .has_posted_ref(
            &mut r,
            &[Decimal::from(5001i64), Decimal::from(5002i64)],
            &long,
        )
        .unwrap();
    assert!(hit);
    let st = &r.log[0];
    assert!(st.text.contains("t.FolioNum IN (@Folios1,@Folios2)"));
    assert!(st.params.iter().all(|(_, t, _)| *t == "AnsiString"));
    let rf = st.params.iter().find(|(n, _, _)| n == "Ref").unwrap();
    assert_eq!(rf.2.as_deref().map(str::len), Some(30));
    assert!(
        !FolioWriter::default()
            .has_posted_ref(&mut Recorder::default(), &[], "x")
            .unwrap()
    );
}

#[test]
fn a_note_is_written_only_when_the_booking_has_one() {
    let mut r = full_install();
    let mut p = plan(ctx(false, Some("Tầng cao")));
    p.note_folios = Some(vec![Decimal::from(4000i64)]);
    FolioWriter::default().write_booking(&mut r, &p).unwrap();
    assert!(r.texts().iter().any(|t| t.contains("UserInfo3")));

    let mut r = full_install();
    let mut p = plan(ctx(false, None));
    p.note_folios = Some(vec![Decimal::from(4000i64)]);
    FolioWriter::default().write_booking(&mut r, &p).unwrap();
    assert!(r.log.is_empty());
}

#[test]
fn fold_name_matches_dotnet() {
    use crate::writer::lookup::fold_name;
    assert_eq!(fold_name(Some("  Đặng   văn  ")), "DANG VAN");
    assert_eq!(fold_name(Some("nguyễn")), "NGUYEN");
    assert_eq!(fold_name(None), "");
    assert_eq!(fold_name(Some("   ")), "");
}
