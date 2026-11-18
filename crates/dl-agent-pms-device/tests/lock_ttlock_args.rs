//! Port of Lock/TtLockArgsTests.cs (H-10 TTLock command line and VB6 `Val`).

mod lock_fakes;

use dl_agent_pms_device::adapters::lock::{failures, tt_lock_args};
use dl_agent_pms_device::contracts::codes;
use dl_time::{Offset, OffsetDateTime};
use lock_fakes::*;

fn ict() -> Offset {
    Offset::from_hms(7, 0, 0).unwrap()
}

#[test]
fn h10_ttlock_args_are_2_type_val_bld_val_floor_raddress_without_colon_end_epoch_hotel_info() {
    let a = address("01A", " 3", "305", "", "12:34:56", "", "", "TTLOCK");
    // 2026-10-16 12:00:45 ICT → truncated to 12:00 → 2026-10-16 05:00:00 UTC.
    let args = tt_lock_args::build(
        1,
        Some(&a),
        "305",
        at("2026-10-16T12:00:45"),
        ict(),
        "HINFO",
    )
    .unwrap();
    let epoch = OffsetDateTime::parse_rfc3339("2026-10-16T05:00:00Z")
        .unwrap()
        .unix_seconds();
    assert_eq!(args, format!("2 1 1 3 123456 {epoch} HINFO"));
}

#[test]
fn h10_ttlock_refuses_invalid_hotel_info_and_a_missing_room_lock_def() {
    let t = at("2026-10-16T12:00:00");
    let e = tt_lock_args::build(1, Some(&address_101()), "101", t, ict(), " ").unwrap_err();
    assert_eq!(e.message, "Invalid HotelInfo!");
    assert_eq!(e.code, codes::NOT_CONFIGURED);
    let e = tt_lock_args::build(1, None, "101", t, ict(), "HINFO").unwrap_err();
    assert_eq!(e.message, "101. (RoomLockDef/RAddress)");
    let e = tt_lock_args::card_id_of(0).unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::ERROR, "Error Writing Card!")
    );
    assert_eq!(tt_lock_args::card_id_of(4711).unwrap(), "4711");
    assert_eq!(tt_lock_args::HOTEL_INFO_MAX_AGE.as_secs(), 570);
}

#[test]
fn vb6_val_reads_the_leading_number() {
    for (text, expected) in [("01", 1), (" 12ab", 12), ("ab", 0), ("", 0), ("-3", -3)] {
        assert_eq!(failures::val(Some(text)), Ok(expected), "{text:?}");
    }
}
