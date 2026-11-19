use super::*;
use crate::jobs::beds::{MODE_FOLD_LEGACY, decide_fold};
use crate::jobs::payload::parse_payload;

fn id(n: u8) -> Uuid {
    format!("6f1c2d3e-0000-4000-8000-0000000001{n:02}")
        .parse()
        .unwrap()
}

fn rooms(json: &str) -> Vec<Room> {
    parse_payload(&format!(r#"{{"rooms":{json}}}"#))
        .unwrap()
        .rooms
}

fn room_json(n: u8, net: u32, removed: bool) -> String {
    format!(
        r#"{{"Id":"{}","RoomTypeCode":"dlx","Arrival":"2026-12-01","Departure":"2026-12-02",
            "NumAdult":2,"GuestLastName":"Tran","Removed":{removed},
            "nights":[{{"Date":"2026-12-01","Net":{net}}}]}}"#,
        id(n)
    )
}

fn folio(n: u8, num: i64) -> ExistingFolio {
    ExistingFolio {
        folio_num: Decimal::from(num),
        room_item_id: id(n),
        room_type_booked: "DLX".into(),
        arrival: Date::new(2026, 12, 1).unwrap(),
        departure: Date::new(2026, 12, 2).unwrap(),
        rate_amount: Decimal::from(1000),
        room_code: String::new(),
        folio_status: 1,
        cancel_num: Decimal::ZERO,
        guest: Some(ExistingGuest {
            num_adult: 2,
            last_name: Some("Tran".into()),
            ..ExistingGuest::default()
        }),
        beds: None,
    }
}

fn fold(r: &Room) -> BedDecision {
    decide_fold(r, MODE_FOLD_LEGACY)
}

fn kinds(r: &DiffResult) -> Vec<(DiffKind, Uuid)> {
    r.diffs.iter().map(|d| (d.kind, d.room_item_id)).collect()
}

#[test]
fn each_room_is_created_updated_cancelled_or_left() {
    let payload = rooms(&format!(
        "[{},{},{},{}]",
        room_json(1, 1000, false),
        room_json(2, 1200, false),
        room_json(3, 1000, true),
        room_json(4, 1000, false)
    ));
    let existing = vec![folio(1, 11), folio(2, 12), folio(3, 13), folio(5, 15)];
    let r = diff(&existing, &payload, &fold);
    assert!(!r.requires_manual);
    assert_eq!(
        kinds(&r),
        vec![
            (DiffKind::Unchanged, id(1)),
            (DiffKind::Update, id(2)),
            (DiffKind::Cancel, id(3)),
            (DiffKind::Create, id(4)),
            (DiffKind::Cancel, id(5)),
        ]
    );
}

#[test]
fn cancelled_folios_are_ignored_and_a_guest_change_is_an_update() {
    let mut gone = folio(1, 10);
    gone.cancel_num = Decimal::from(3);
    let mut renamed = folio(1, 11);
    renamed.guest.as_mut().unwrap().last_name = Some("Le".into());
    let r = diff(
        &[gone, renamed],
        &rooms(&format!("[{}]", room_json(1, 1000, false))),
        &fold,
    );
    assert_eq!(kinds(&r), vec![(DiffKind::Update, id(1))]);
    assert_eq!(
        r.diffs[0].existing.as_ref().unwrap().folio_num,
        Decimal::from(11)
    );
}

#[test]
fn an_assigned_or_checked_in_folio_needs_the_operator() {
    let mut assigned = folio(1, 11);
    assigned.room_code = "101".into();
    let r = diff(
        &[assigned],
        &rooms(&format!("[{}]", room_json(1, 1200, false))),
        &fold,
    );
    assert!(r.requires_manual && r.diffs.is_empty() && !r.bed_locked);
    assert_eq!(
        r.manual_reason.as_deref(),
        Some("folio 11 đã xếp phòng (101)")
    );

    let mut checked_in = folio(5, 15);
    checked_in.folio_status = 2;
    let r = diff(
        &[checked_in],
        &rooms(&format!("[{}]", room_json(1, 1000, false))),
        &fold,
    );
    assert_eq!(
        r.manual_reason.as_deref(),
        Some("folio 15 đã check-in (status 2)")
    );
}

#[test]
fn blank_payload_contact_fields_never_count_as_a_change() {
    let mut f = folio(1, 11);
    f.guest.as_mut().unwrap().phone = Some("0900".into());
    let r = diff(
        &[f],
        &rooms(&format!("[{}]", room_json(1, 1000, false))),
        &fold,
    );
    assert_eq!(kinds(&r), vec![(DiffKind::Unchanged, id(1))]);
}
