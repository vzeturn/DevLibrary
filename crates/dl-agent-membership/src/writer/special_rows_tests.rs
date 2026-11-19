//! `SMILE_SPECIAL` rows against the recording session: the preflight inputs read, the
//! agent rows written in the folio transaction, and the golden SQL of a create and a modify
//! with an extra bed. Mirrors MM-MEMBERSHIP
//! `tests/MM.Membership.Agent.Tests/Smile/FolioWriterExtraBedSpecialTests.cs` where it can
//! run without SQL Server.

use dl_decimal::Decimal;
use dl_tds::Value;

use super::*;
use crate::jobs::beds::{FOLD_FINGERPRINT_DRIFT, MODE_FOLD_LEGACY};
use crate::jobs::payload::{Room, parse_payload};
use crate::jobs::preflight::decide_extra_bed;
use crate::writer::bed_terms_reader::tests::{VICTORY_FINGERPRINT, seed_victory};
use crate::writer::folio::WriteOptions;
use crate::writer::folio::tests::{ROOM_ID, ctx, full_install, golden, plan, position};
use crate::writer::recorder::Recorder;
use crate::writer::session::{Called, Rec};

/// DLX, 2 nights, 1 extra bed at 400,000 a night: Net 1,700,000 = 1,300,000 + bed.
fn bed_room(beds: i32) -> Room {
    let json = format!(
        r#"{{"rooms":[{{"Id":"{ROOM_ID}","RoomTypeCode":"DLX","Arrival":"2026-12-01",
        "Departure":"2026-12-03","NumAdult":3,"NumChild":0,"GuestFirstName":"An",
        "GuestLastName":"Nguyen","PricingModel":"OCCUPANCY_V1","ExtraBeds":{beds},
        "ExtraBedNightly":400000,"nights":[
        {{"Date":"2026-12-01","Net":1700000,"NetExclBed":1300000}},
        {{"Date":"2026-12-02","Net":1700000,"NetExclBed":1300000}}]}}]}}"#
    );
    parse_payload(&json).unwrap().rooms.remove(0)
}

fn victory() -> Recorder {
    let mut r = full_install();
    seed_victory(&mut r);
    r.rows(
        "SELECT ConfirmNum, OriginCode FROM Folio",
        vec![Rec::new(vec![
            ("ConfirmNum", Value::Decimal(Decimal::from(9001))),
            ("OriginCode", Value::String("MEM".into())),
        ])],
    );
    r
}

fn mm_row(qty: i32, price: i64, code: &str) -> Rec {
    Rec::new(vec![
        ("SpecialCode", Value::String(code.into())),
        ("Qty", Value::I32(qty)),
        ("Price", Value::Decimal(Decimal::from(price))),
        ("FromDay", Value::I32(1)),
        ("ToDay", Value::I32(2)),
        ("Cancelled", Value::Bool(false)),
    ])
}

fn count(r: &Recorder, needle: &str) -> usize {
    r.texts().iter().filter(|t| t.contains(needle)).count()
}

#[test]
fn create_writes_one_band_the_rate_without_the_bed_and_extra_bed() {
    let mut r = victory();
    let w = FolioWriter::default();
    let inputs = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap()
        .unwrap();
    let mut p = plan(ctx(false, None));
    p.creates.push(bed_room(1));
    p.extra_bed = Some(inputs);
    w.write_booking(&mut r, &p).unwrap();

    let insert = &r.log[position(&r, "INSERT INTO Folio_Special")];
    assert_eq!(
        insert.text,
        "INSERT INTO Folio_Special (FolioNum, SpecialCode, Qty, Price, FromDay, ToDay, SPNotes, CancelNum) VALUES (@FolioNum, @SpecialCode, @Qty, @Price, @FromDay, @ToDay, @SPNotes, 0)"
    );
    let value = |n: &str| {
        insert
            .params
            .iter()
            .find(|p| p.0 == n.trim_start_matches('@'))
            .and_then(|p| p.2.clone())
    };
    assert_eq!(value("@SpecialCode").as_deref(), Some("EB"));
    assert_eq!(value("@Price").as_deref(), Some("453600"));
    assert_eq!(value("@Qty").as_deref(), Some("1"));
    assert!(
        value("@SPNotes")
            .unwrap()
            .starts_with(&format!("MM:{ROOM_ID} "))
    );
    let folio = &r.log[position(&r, "INSERT INTO Folio ")];
    let p = |n: &str| {
        folio
            .params
            .iter()
            .find(|p| p.0 == n.trim_start_matches('@'))
            .and_then(|p| p.2.clone())
    };
    assert_eq!(p("@RateAmount").as_deref(), Some("1300000"));
    assert_eq!(p("@ExtraBed").as_deref(), Some("1"));
    assert_eq!(p("@UserInfo3"), None, "the bed is its own line, not a note");
    // The rows are written inside the folio transaction, after the rates.
    assert!(position(&r, "INSERT INTO FolioRate") < position(&r, "INSERT INTO Folio_Special"));
    assert!(position(&r, "INSERT INTO Folio_Special") < position(&r, "COMMIT TRANSACTION"));
    golden("create_booking_extra_bed", &r);
}

#[test]
fn modify_replaces_a_stale_agent_row_and_keeps_front_desk_rows() {
    let mut r = victory();
    r.rows(
        "FROM Folio_Special WITH (UPDLOCK)",
        vec![mm_row(2, 900_000, "EB")],
    );
    let w = FolioWriter::default();
    let inputs = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap();
    let mut p = plan(ctx(false, None));
    p.updates.push((Decimal::from(7001i64), bed_room(1)));
    p.extra_bed = inputs;
    w.write_booking(&mut r, &p).unwrap();
    let delete = &r.log[position(&r, "DELETE FROM Folio_Special")];
    assert!(delete.text.ends_with("SPNotes LIKE @Marker"));
    assert_eq!(
        delete.params[1].2.as_deref(),
        Some(format!("MM:{ROOM_ID} %").as_str())
    );
    assert!(position(&r, "DELETE FROM Folio_Special") < position(&r, "INSERT INTO Folio_Special"));
    golden("modify_booking_extra_bed", &r);
}

#[test]
fn rows_already_as_wanted_write_nothing() {
    let mut r = victory();
    r.rows(
        "FROM Folio_Special WITH (UPDLOCK)",
        vec![mm_row(1, 453_600, "eb")],
    );
    let w = FolioWriter::default();
    let mut p = plan(ctx(false, None));
    p.updates.push((Decimal::from(7001i64), bed_room(1)));
    p.extra_bed = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap();
    w.write_booking(&mut r, &p).unwrap();
    assert_eq!(count(&r, "DELETE FROM Folio_Special"), 0);
    assert_eq!(count(&r, "INSERT INTO Folio_Special"), 0);
}

#[test]
fn a_cancelled_or_differently_coded_row_is_replaced() {
    for row in [
        Rec::new(vec![
            ("SpecialCode", Value::String("EB".into())),
            ("Qty", Value::I32(1)),
            ("Price", Value::Decimal(Decimal::from(453_600))),
            ("FromDay", Value::I32(1)),
            ("ToDay", Value::I32(2)),
            ("Cancelled", Value::Bool(true)),
        ]),
        mm_row(1, 453_600, "XB"),
    ] {
        let mut r = victory();
        r.rows("FROM Folio_Special WITH (UPDLOCK)", vec![row]);
        let w = FolioWriter::default();
        let mut p = plan(ctx(false, None));
        p.updates.push((Decimal::from(7001i64), bed_room(1)));
        p.extra_bed = w
            .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
            .unwrap();
        w.write_booking(&mut r, &p).unwrap();
        assert_eq!(count(&r, "DELETE FROM Folio_Special"), 1);
        assert_eq!(count(&r, "INSERT INTO Folio_Special"), 1);
    }
}

#[test]
fn a_non_identity_nseq_takes_the_next_number_under_lock() {
    let mut r = victory();
    r.rows(
        "COLUMNPROPERTY(OBJECT_ID(N'dbo.Folio_Special'), 'nSeq', 'IsIdentity')",
        vec![Rec::new(vec![("", Value::I32(0))])],
    );
    let w = FolioWriter::default();
    let mut p = plan(ctx(false, None));
    p.creates.push(bed_room(1));
    p.extra_bed = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap();
    w.write_booking(&mut r, &p).unwrap();
    let insert = &r.texts()[position(&r, "INSERT INTO Folio_Special")];
    assert!(insert.contains(
        "(FolioNum, nSeq, SpecialCode, Qty, Price, FromDay, ToDay, SPNotes, CancelNum) VALUES (@FolioNum, (SELECT ISNULL(MAX(s.nSeq), 0) + 1 FROM Folio_Special s WITH (UPDLOCK, HOLDLOCK) WHERE s.FolioNum = @FolioNum), @SpecialCode"
    ));
}

#[test]
fn fold_legacy_reads_nothing_and_a_fold_deletes_the_agent_rows() {
    let mut r = victory();
    let w = FolioWriter::new(WriteOptions {
        extra_bed_mode: MODE_FOLD_LEGACY.into(),
        ..WriteOptions::default()
    });
    assert_eq!(
        w.read_extra_bed_inputs(&mut r, Some("x"), None).unwrap(),
        None
    );
    assert!(r.log.is_empty());
    r.rows(
        "FROM Folio_Special WITH (UPDLOCK)",
        vec![mm_row(1, 453_600, "EB")],
    );
    let mut p = plan(ctx(false, None));
    p.updates.push((Decimal::from(7001i64), bed_room(1)));
    w.write_booking(&mut r, &p).unwrap();
    assert_eq!(count(&r, "DELETE FROM Folio_Special"), 1);
    assert_eq!(count(&r, "INSERT INTO Folio_Special"), 0);
}

#[test]
fn a_special_table_without_the_columns_folds_as_drift() {
    let mut r = victory();
    r.on("INFORMATION_SCHEMA.COLUMNS", |st| {
        let table = st.params[0].2.clone().unwrap_or_default();
        let cols: &[&str] = if table == "Folio_Special" {
            &["Qty", "SPNotes"]
        } else {
            &[]
        };
        Ok(Called {
            rows: cols
                .iter()
                .map(|c| {
                    Rec::new(vec![
                        ("Name", Value::String((*c).into())),
                        ("MaxLength", Value::Null),
                    ])
                })
                .collect(),
            status: Some(0),
        })
    });
    let w = FolioWriter::default();
    let inputs = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap()
        .unwrap();
    assert_eq!(inputs.terms, None);
    assert!(!r.texts().iter().any(|t| t.contains("sys.columns")));
    let b = decide_extra_bed(&bed_room(1), Some(&inputs), MODE_SMILE_SPECIAL);
    assert_eq!(b.fold_reason, Some(FOLD_FINGERPRINT_DRIFT));
}

#[test]
fn rows_without_sp_notes_fail_unknown() {
    let mut r = victory();
    let w = FolioWriter::default();
    let inputs = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap();
    let bed = decide_extra_bed(&bed_room(1), inputs.as_ref(), MODE_SMILE_SPECIAL);
    let schema =
        crate::writer::schema::TableSchema::from_columns("Folio_Special", &[("Qty", None)]);
    let e = w
        .write_mm_bed_rows_in_tx(
            &mut r,
            &schema,
            Decimal::from(7001i64),
            bed_room(1).room_item_id,
            &bed,
        )
        .unwrap_err();
    assert!(matches!(e, WriteError::Coded { ref code, .. } if code == UNKNOWN));
}

#[test]
fn the_payload_or_hub_code_reads_pms_priced_terms_for_that_code() {
    let mut r = victory();
    let w = FolioWriter::default();
    w.opts.hub_special_code.set(Some(" EB "));
    assert_eq!(w.effective_special_code(), "EB");
    let inputs = w
        .read_extra_bed_inputs(&mut r, None, None)
        .unwrap()
        .unwrap();
    assert!(inputs.terms.as_ref().unwrap().pms_priced);

    let mut r = victory();
    let w = FolioWriter::default();
    let inputs = w
        .read_extra_bed_inputs(&mut r, None, Some(" XB "))
        .unwrap()
        .unwrap();
    let special = &r.log[position(&r, "FROM dbo.SpecialService")];
    assert_eq!(special.params[0].2.as_deref(), Some("XB"));
    let t = inputs.terms.unwrap();
    assert!(t.pms_priced);
    assert_ne!(t.structural_fingerprint, VICTORY_FINGERPRINT);
}

#[test]
fn the_hub_code_keeps_a_change_flag_and_ignores_blanks() {
    let c = HubSpecialCode::default();
    assert!(!c.set(Some("  ")));
    assert!(c.set(Some("XB")));
    assert!(!c.set(Some(" XB ")));
    assert!(c.set(None));
    assert_eq!(c.get(), None);
}

#[test]
fn decimal_text_of_the_marker_param_is_a_tcvn3_varchar() {
    let mut r = victory();
    let w = FolioWriter::default();
    let mut p = plan(ctx(false, None));
    p.creates.push(bed_room(1));
    p.extra_bed = w
        .read_extra_bed_inputs(&mut r, Some(VICTORY_FINGERPRINT), None)
        .unwrap();
    w.write_booking(&mut r, &p).unwrap();
    let insert = &r.log[position(&r, "INSERT INTO Folio_Special")];
    let notes = insert.params.iter().find(|p| p.0 == "SPNotes").unwrap();
    assert_eq!(notes.1, "AnsiString");
    assert_eq!(
        notes.2.as_deref(),
        Some(tcvn3::encode(&special_row_marker(ROOM_ID.parse().unwrap())).as_str())
    );
}
