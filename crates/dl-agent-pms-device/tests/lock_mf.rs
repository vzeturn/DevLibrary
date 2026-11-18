//! Port of Lock/MfLockAdapterTests.cs (I-03b MF .NET lock bridge over the LockSDK TP_*
//! family), on the fake TP native.

mod lock_fakes;

use std::time::Duration;

use dl_agent_pms_device::adapters::DeviceAdapter;
use dl_agent_pms_device::adapters::lock::MfLockAdapter;
use dl_agent_pms_device::config::LockOptions;
use dl_agent_pms_device::contracts::{
    CancelAllRequest, EraseRequest, KeyCardRequest, LockAddress, ReadRequest, codes,
};
use dl_json::Value;
use lock_fakes::*;

const FROM: &str = "2026-10-14T14:00:00";
const TO: &str = "2026-10-16T21:30:00";

fn request(mf_lock_name: &str, address: Option<LockAddress>, cards: i32) -> KeyCardRequest {
    key_card(
        "101",
        FROM,
        TO,
        cards,
        123_456,
        Some(address.unwrap_or_else(address_101)),
        Some(vendor("MFCARD", mf_lock_name)),
    )
}

fn hls() -> KeyCardRequest {
    request("hls", None, 1)
}

fn adapter(options: LockOptions, tp: &std::sync::Arc<FakeTp>) -> MfLockAdapter {
    MfLockAdapter::new(options, tp.native())
}

#[test]
fn i03b_tp_write_passes_raddress_and_24_hour_times_flags_0() {
    let tp = FakeTp::new();
    let json = adapter(LockOptions::default(), &tp)
        .issue(&request("goodum", None, 1), &cx())
        .unwrap();
    assert_eq!(
        tp.state().last_write,
        Some((
            "0101".into(),
            "2026-10-14 14:00:00".into(),
            "2026-10-16 21:30:00".into(),
            0
        ))
    );
    let card = &json["cards"][0];
    assert_eq!(card["cardNumber"], "A1B2C3D4");
    assert_eq!(card["room"], "101");
    assert_eq!(card["from"], "2026-10-14T14:00:00");
    assert_eq!(card["to"], "2026-10-16T21:30:00");
}

#[test]
fn i03b_tp_configuration_called_with_5_for_hls() {
    let tp = FakeTp::new();
    let options = LockOptions {
        mf_lock_door_type: 4,
        ..LockOptions::default()
    };
    adapter(options.clone(), &tp).issue(&hls(), &cx()).unwrap();
    adapter(options, &tp)
        .issue(&request("ezibee", None, 1), &cx())
        .unwrap();
    assert_eq!(tp.state().configured, [5, 4]);
    assert_eq!(
        tp.calls(),
        [
            "TP_Configuration",
            "TP_MakeGuestCardEx",
            "TP_Configuration",
            "TP_MakeGuestCardEx"
        ]
    );
}

#[test]
fn i03b_tp_minus_one_maps_to_device_no_card() {
    let tp = FakeTp::new();
    tp.state().make_rc = -1;
    let e = refusal(adapter(LockOptions::default(), &tp).issue(&hls(), &cx()));
    assert_eq!(e.code, codes::NO_CARD);
    assert_eq!(e.message, "TP_MakeGuestCardEx failed: NO_CARD (-1)");
}

#[test]
fn i03b_tp_codes_map_to_the_device_error_codes() {
    for (rc, code) in [
        (-2, codes::UNAVAILABLE),
        (-6, codes::UNAVAILABLE),
        (-11, codes::UNAVAILABLE),
        (-12, codes::UNAVAILABLE),
        (-29, codes::ERROR),
        (-99, codes::ERROR),
    ] {
        let tp = FakeTp::new();
        tp.state().make_rc = rc;
        let e = refusal(adapter(LockOptions::default(), &tp).issue(&hls(), &cx()));
        assert_eq!(e.code, code, "{rc}");
        assert!(e.message.contains(&format!("({rc})")), "{}", e.message);
    }
}

#[test]
fn i03b_unknown_mf_lock_name_refuses_lock_x_to_write_is_not_defined() {
    let tp = FakeTp::new();
    let e = refusal(adapter(LockOptions::default(), &tp).issue(&request("Salto", None, 1), &cx()));
    assert_eq!(e.code, codes::NOT_CONFIGURED);
    assert_eq!(e.message, "Lock Salto to Write is not defined!");
    assert!(tp.calls().is_empty());
}

#[test]
fn i03b_vendors_without_a_dll_refuse_lock_sdk_not_installed() {
    for name in ["adel", "hikvision", "v9rf", "dormakaba", "xeeder"] {
        let tp = FakeTp::new();
        let e = refusal(adapter(LockOptions::default(), &tp).issue(&request(name, None, 1), &cx()));
        assert_eq!(e.code, codes::SDK_NOT_INSTALLED);
        assert_eq!(e.message, format!("lock SDK not installed for {name}"));
    }
}

#[test]
fn i03b_a_build_without_locksdk_refuses_instead_of_succeeding() {
    let e = refusal(MfLockAdapter::new(LockOptions::default(), None).issue(&hls(), &cx()));
    assert_eq!(e.code, codes::SDK_NOT_INSTALLED);
    assert_eq!(e.message, "lock SDK not installed for hls");
}

#[test]
fn i03b_missing_room_lock_def_refuses_no_setup_for_room() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp);
    let mut no_row = key_card("101", FROM, TO, 1, 1, None, Some(vendor("MFCARD", "hls")));
    no_row.lock_address = None;
    let mut blank = address_101();
    blank.address = some(" ");
    let no_address = request("hls", Some(blank), 1);
    let mut null_address = address_101();
    null_address.address = None;
    let null_address = request("hls", Some(null_address), 1);
    for r in [no_row, no_address, null_address] {
        let e = refusal(a.issue(&r, &cx()));
        assert_eq!(e.code, codes::NOT_CONFIGURED);
        assert_eq!(e.message, "No setup for room 101. (RoomLockDef/RAddress)");
    }
    assert!(tp.calls().is_empty());
}

#[test]
fn i03b_the_agent_mf_lock_name_is_the_fallback_when_the_payload_has_none() {
    let tp = FakeTp::new();
    let r = key_card("101", FROM, TO, 1, 1, Some(address_101()), None);
    let options = LockOptions {
        mf_lock_name: "HLS".into(),
        ..LockOptions::default()
    };
    adapter(options.clone(), &tp).issue(&r, &cx()).unwrap();
    assert_eq!(tp.state().configured, [5]);
    // A blank payload name falls back too.
    let r = key_card(
        "101",
        FROM,
        TO,
        1,
        1,
        Some(address_101()),
        Some(vendor("MFCARD", " ")),
    );
    adapter(options, &tp).issue(&r, &cx()).unwrap();
    assert_eq!(tp.state().configured, [5, 5]);
    // Neither set: not configured.
    let e = refusal(adapter(LockOptions::default(), &tp).issue(&r, &cx()));
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::NOT_CONFIGURED,
            "MF lock name is not set (WSParameter MFLockName)"
        )
    );
}

#[test]
fn i03b_a_tp_encoder_writes_one_card_per_command() {
    let tp = FakeTp::new();
    let e = refusal(adapter(LockOptions::default(), &tp).issue(&request("hls", None, 2), &cx()));
    assert_eq!(e.code, codes::BAD_PAYLOAD);
    assert_eq!(
        e.message,
        "The hls encoder writes one card per command, not 2"
    );
    assert!(tp.calls().is_empty());
}

#[test]
fn i03b_erase_checks_the_serial_on_the_encoder_before_tp_cancel_card() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp);
    let erase = |card: &str| EraseRequest {
        room: some("101"),
        card_number: some(card),
        lock_address: Some(address_101()),
        vendor: Some(vendor("MFCARD", "hls")),
    };

    let wrong = refusal(a.erase(&erase("FFFF"), &cx()));
    assert_eq!(wrong.code, codes::ERROR);
    assert_eq!(
        wrong.message,
        "The card on the encoder is A1B2C3D4, not FFFF"
    );
    assert!(!tp.calls().contains(&"TP_CancelCard".to_owned()));

    let json = a.erase(&erase("a1b2c3d4"), &cx()).unwrap();
    assert_eq!(json["erased"], true);
    assert_eq!(json["cardNumber"], "a1b2c3d4");
    assert_eq!(tp.calls().last().unwrap(), "TP_CancelCard");
}

#[test]
fn i03b_read_returns_the_serial_and_the_lock_address_and_cancel_all_reports_no_room_wide_cancel() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp);
    a.issue(&hls(), &cx()).unwrap();

    let read = a
        .read(
            &ReadRequest {
                vendor: Some(vendor("MFCARD", "hls")),
            },
            &cx(),
        )
        .unwrap();
    assert_eq!(read["cardNumber"], "A1B2C3D4");
    assert_eq!(read["address"], "0101");
    assert_eq!(read["from"], "2026-10-14 14:00:00");
    assert!(read.as_object().unwrap().get("room").is_none());

    let cancel = a
        .cancel_all(
            &CancelAllRequest {
                room: some("101"),
                lock_address: Some(address_101()),
                vendor: Some(vendor("MFCARD", "hls")),
            },
            &cx(),
        )
        .unwrap();
    assert_eq!(cancel["cancelled"], false);
    assert_eq!(cancel["room"], "101");
    assert_eq!(
        cancel["reason"],
        "LockSDK has no room-wide cancel; erase each card"
    );
}

fn duplicate_of(r: KeyCardRequest) -> KeyCardRequest {
    KeyCardRequest {
        add_guest: Some(true),
        ..r
    }
}

#[test]
fn i03b_chained_duplicate_waits_for_a_different_card_on_the_encoder_before_writing() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp).with_next_card_poll(Duration::from_millis(1));
    a.issue(&hls(), &cx()).unwrap();
    tp.state()
        .inserted
        .extend(["A1B2C3D4", "a1b2c3d4", "E5F6A7B8"].map(String::from));

    a.duplicate(&duplicate_of(hls()), &cx()).unwrap();

    assert_eq!(tp.count("TP_GetCardSnr"), 3);
    assert_eq!(tp.count("TP_MakeGuestCardEx"), 2);
    assert_eq!(tp.calls().last().unwrap(), "TP_MakeGuestCardEx");
}

#[test]
fn i03b_chained_duplicate_with_the_same_card_left_on_the_encoder_times_out_please_insert_card() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp)
        .with_next_card_wait(Duration::from_millis(30))
        .with_next_card_poll(Duration::from_millis(1));
    a.issue(&hls(), &cx()).unwrap();

    let e = refusal(a.duplicate(&duplicate_of(hls()), &cx()));

    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::NO_CARD, "Please Insert Card")
    );
    assert_eq!(tp.count("TP_MakeGuestCardEx"), 1);
}

#[test]
fn i03b_a_duplicate_with_no_recent_write_does_not_wait() {
    let tp = FakeTp::new();
    adapter(LockOptions::default(), &tp)
        .duplicate(&hls(), &cx())
        .unwrap();
    assert!(!tp.calls().contains(&"TP_GetCardSnr".to_owned()));
    assert_eq!(tp.calls().last().unwrap(), "TP_MakeGuestCardEx");
}

#[test]
fn i03b_a_duplicate_for_another_room_right_after_a_write_is_not_a_chained_card() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp)
        .with_next_card_wait(Duration::from_millis(30))
        .with_next_card_poll(Duration::from_millis(1));
    a.issue(&hls(), &cx()).unwrap();

    let other = KeyCardRequest {
        room: some("102"),
        ..duplicate_of(hls())
    };
    a.duplicate(&other, &cx()).unwrap();

    assert!(!tp.calls().contains(&"TP_GetCardSnr".to_owned()));
    assert_eq!(tp.count("TP_MakeGuestCardEx"), 2);
}

// ---- Rust-side cases: C# null handling, cancellation and non-OK polls ----------------

#[test]
fn a_chained_wait_is_cancelled_by_the_command_timeout() {
    use dl_agent_pms_device::adapters::Cx;
    use dl_agent_pms_device::agent::StopToken;
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp).with_next_card_poll(Duration::from_millis(1));
    a.issue(&hls(), &cx()).unwrap();
    let short = Cx::new(Duration::from_millis(20), StopToken::new());
    let e = refusal(a.duplicate(&duplicate_of(hls()), &short));
    assert!(Cx::is_cancellation(&e), "{e}");
    assert_eq!(tp.count("TP_MakeGuestCardEx"), 1);
}

#[test]
fn a_chained_wait_keeps_waiting_on_no_card_and_reports_other_codes() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp)
        .with_next_card_wait(Duration::from_millis(20))
        .with_next_card_poll(Duration::from_millis(1));
    a.issue(&hls(), &cx()).unwrap();
    tp.state().read_rc = -1;
    let e = refusal(a.duplicate(&duplicate_of(hls()), &cx()));
    assert_eq!(e.message, "Please Insert Card");
    tp.state().read_rc = -6;
    let e = refusal(a.duplicate(&duplicate_of(hls()), &cx()));
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::UNAVAILABLE,
            "TP_GetCardSnr failed: PORT_NOT_OPEN (-6)"
        )
    );
}

#[test]
fn a_null_card_number_or_room_follows_the_csharp_null_handling() {
    let tp = FakeTp::new();
    let a = adapter(LockOptions::default(), &tp);
    // EraseRequest.CardNumber.Trim() on null: NullReferenceException after TP_GetCardSnr.
    let e = refusal(a.erase(
        &EraseRequest {
            room: some("101"),
            card_number: None,
            lock_address: None,
            vendor: Some(vendor("MFCARD", "hls")),
        },
        &cx(),
    ));
    assert_eq!(e.code, codes::ADAPTER_FAILED);
    assert_eq!(
        e.message,
        "NullReferenceException: Object reference not set to an instance of an object."
    );
    assert_eq!(tp.calls(), ["TP_Configuration", "TP_GetCardSnr"]);
    // A null room interpolates as "" and is written as null.
    let mut r = hls();
    r.lock_address = None;
    r.room = None;
    assert_eq!(
        refusal(a.issue(&r, &cx())).message,
        "No setup for room . (RoomLockDef/RAddress)"
    );
    let mut r = hls();
    r.room = None;
    let json = a.issue(&r, &cx()).unwrap();
    assert_eq!(json["cards"][0]["room"], Value::Null);
}

#[test]
fn a_failing_configuration_is_reported_before_any_card_call() {
    let tp = FakeTp::new();
    tp.state().configuration_rc = -2;
    let e = refusal(adapter(LockOptions::default(), &tp).issue(&hls(), &cx()));
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::UNAVAILABLE,
            "TP_Configuration failed: NO_RW_MACHINE (-2)"
        )
    );
    assert_eq!(tp.calls(), ["TP_Configuration"]);
}
