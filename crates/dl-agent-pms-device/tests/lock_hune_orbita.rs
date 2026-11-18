//! Port of Lock/HuneOrbitaAdapterTests.cs (I-03 Hune HSmart V8 and Orbita CLock), on the
//! fake natives.

mod lock_fakes;

use dl_agent_pms_device::adapters::DeviceAdapter;
use dl_agent_pms_device::adapters::lock::{
    HuneCard, HuneHSmartV8Adapter, OrbitaCLockAdapter, OrbitaCard,
};
use dl_agent_pms_device::config::LockOptions;
use dl_agent_pms_device::contracts::{
    CancelAllRequest, EraseRequest, KeyCardRequest, LockAddress, ReadRequest, codes,
};
use dl_json::Value;
use lock_fakes::*;

const FROM: &str = "2026-10-14T14:05:09";
const TO: &str = "2026-10-16T21:30:00";

fn options() -> LockOptions {
    LockOptions {
        hune_com_port: 3,
        hune_card_code: "PASS".into(),
        hune_system_code: "SYS".into(),
        hune_hotel_code: "HOTEL".into(),
        orbita_version: 3,
        ..LockOptions::default()
    }
}

fn request(address: Option<LockAddress>, cards: i32) -> KeyCardRequest {
    key_card(
        "101",
        FROM,
        TO,
        cards,
        1,
        address,
        Some(vendor("HUNE2013", "")),
    )
}

fn orbita_address() -> LockAddress {
    address("1", "2", "105", "", "0105", "", "", "ORBITA")
}

fn erase(card: &str) -> EraseRequest {
    EraseRequest {
        room: some("101"),
        card_number: some(card),
        lock_address: None,
        vendor: None,
    }
}

#[test]
fn i03_hsmart_keycard_gets_syscode_hotelcode_cardpass_and_yy_mm_dd_times() {
    let hune = FakeHune::new();
    let json = HuneHSmartV8Adapter::new(options(), hune.native())
        .issue(&request(Some(address_101()), 1), &cx())
        .unwrap();
    assert_eq!(
        hune.state().written,
        [HuneCard {
            com_port: 3,
            card_pass: "PASS".into(),
            sys_code: "SYS".into(),
            hotel_code: "HOTEL".into(),
            address: "0101".into(),
            start: "26-10-14 14:05:09".into(),
            end: "26-10-16 21:30:00".into(),
        }]
    );
    assert_eq!(json["cards"][0]["cardNumber"], "H1");
    assert_eq!(json["rmm"], "RMM-7");
}

#[test]
fn i03_a_non_zero_keycard_code_maps_to_device_error_make_card_error() {
    let e = refusal(
        HuneHSmartV8Adapter::new(options(), FakeHune::with_rc(7).native())
            .issue(&request(Some(address_101()), 1), &cx()),
    );
    assert_eq!(e.code, codes::ERROR);
    assert_eq!(e.message, "Make Card Error. Code=7");
}

#[test]
fn i03_hsmart_read_error_keeps_the_lock_codes_out_of_the_message() {
    let e = refusal(
        HuneHSmartV8Adapter::new(options(), FakeHune::with_rc(4).native())
            .read(&ReadRequest::default(), &cx()),
    );
    assert_eq!(e.message, "Reading Error. COM Port=3, Err Code=4");
    assert!(!e.message.contains("PASS"));
}

#[test]
fn i03_orbita_version_3_pads_building_00_floor_0000() {
    let orbita = FakeOrbita::new();
    OrbitaCLockAdapter::new(options(), orbita.native())
        .issue(&request(Some(orbita_address()), 1), &cx())
        .unwrap();
    assert_eq!(
        orbita.state().written,
        [OrbitaCard {
            building: "01".into(),
            floor: "0002".into(),
            room: "105".into(),
            start: "2026-10-14 14:05:09".into(),
            end: "2026-10-16 21:30:00".into(),
        }]
    );
    assert_eq!(
        orbita.calls(),
        ["dv_connect(1)", "dv_write_card", "dv_disconnect"]
    );
}

#[test]
fn i03_orbita_versions_1_and_2_keep_building_and_floor_as_stored() {
    let orbita = FakeOrbita::new();
    let options = LockOptions {
        orbita_version: 1,
        ..LockOptions::default()
    };
    OrbitaCLockAdapter::new(options, orbita.native())
        .issue(&request(Some(orbita_address()), 1), &cx())
        .unwrap();
    let s = orbita.state();
    assert_eq!(
        (s.written[0].building.as_str(), s.written[0].floor.as_str()),
        ("1", "2")
    );
}

#[test]
fn i03_orbita_disconnects_after_a_failed_write() {
    let orbita = FakeOrbita::with_rc(5);
    let e = refusal(
        OrbitaCLockAdapter::new(options(), orbita.native())
            .issue(&request(Some(address_101()), 1), &cx()),
    );
    assert_eq!(e.message, "Make Card Error. Code=5");
    assert_eq!(orbita.calls().last().unwrap(), "dv_disconnect");
}

#[test]
fn i03_orbita_erase_deletes_only_the_named_card() {
    let orbita = FakeOrbita::new();
    let adapter = OrbitaCLockAdapter::new(options(), orbita.native());
    let wrong = refusal(adapter.erase(&erase("OB9"), &cx()));
    assert_eq!(
        (wrong.code.as_str(), wrong.message.as_str()),
        (codes::ERROR, "The card on the encoder is OB1, not OB9")
    );
    assert!(!orbita.calls().contains(&"dv_delete_card".to_owned()));
    let json = adapter.erase(&erase("ob1"), &cx()).unwrap();
    assert_eq!(json["erased"], true);
    assert_eq!(orbita.calls().last().unwrap(), "dv_disconnect");
}

#[test]
fn i03_a_missing_room_lock_def_refuses_room_roomlockdef_raddress() {
    let hune = FakeHune::new();
    let orbita = FakeOrbita::new();
    let calls: [Box<dyn Fn() -> Result<Value, dl_agent_pms_device::adapters::AdapterError>>; 2] = [
        Box::new(|| {
            HuneHSmartV8Adapter::new(options(), hune.native()).issue(&request(None, 1), &cx())
        }),
        Box::new(|| {
            OrbitaCLockAdapter::new(options(), orbita.native()).issue(&request(None, 1), &cx())
        }),
    ];
    for call in calls {
        let e = refusal(call());
        assert_eq!(e.code, codes::NOT_CONFIGURED);
        assert_eq!(e.message, "101. (RoomLockDef/RAddress)");
    }
    assert!(hune.state().written.is_empty());
    assert!(orbita.calls().is_empty());
}

#[test]
fn i03_the_old_orbita_ocx_path_refuses_as_not_installed() {
    let options = LockOptions {
        orbita_version: 0,
        ..LockOptions::default()
    };
    let e = refusal(
        OrbitaCLockAdapter::new(options, FakeOrbita::new().native())
            .issue(&request(Some(address_101()), 1), &cx()),
    );
    assert_eq!(e.code, codes::SDK_NOT_INSTALLED);
    assert_eq!(
        e.message,
        "lock SDK not installed for orbita OCX (OrbitaCLock=0)"
    );
}

// ---- Rust-side cases: results, null handling and card counts ------------------------

#[test]
fn hune_and_orbita_results_and_refusals() {
    let hune = FakeHune::new();
    let h = HuneHSmartV8Adapter::new(options(), hune.native());
    let json = h.issue(&request(Some(address_101()), 2), &cx()).unwrap();
    assert_eq!(json["cards"][1]["cardNumber"], "H2");
    assert_eq!(json["cards"][1]["from"], "2026-10-14T14:05:09");
    let read = h.read(&ReadRequest::default(), &cx()).unwrap();
    assert_eq!(
        (read["cardNumber"].as_str(), read["address"].as_str()),
        (Some("H1"), Some("0101"))
    );
    let e = refusal(h.erase(&erase("H1"), &cx()));
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::UNSUPPORTED_KIND,
            "erase is not supported by the Hune HSmart V8 lock"
        )
    );
    let cancel = h.cancel_all(&CancelAllRequest::default(), &cx()).unwrap();
    assert_eq!(cancel["room"], Value::Null);
    assert_eq!(cancel["reason"], "HSmart V8 has no room-wide cancel");
    let e = refusal(h.issue(&request(Some(address_101()), -1), &cx()));
    assert_eq!(
        e.message,
        "ArgumentOutOfRangeException: Non-negative number required. (Parameter 'capacity')"
    );

    let orbita = FakeOrbita::new();
    let o = OrbitaCLockAdapter::new(options(), orbita.native());
    let read = o.read(&ReadRequest::default(), &cx()).unwrap();
    assert_eq!(
        (read["floor"].as_str(), read["address"].as_str()),
        (Some("0001"), Some("101"))
    );
    // No RRoom: the RAddress is the room; a null RBuilding is the C# NullReferenceException.
    let mut no_room = orbita_address();
    no_room.room = some(" ");
    o.issue(&request(Some(no_room), 1), &cx()).unwrap();
    assert_eq!(orbita.state().written[0].room, "0105");
    let mut no_building = orbita_address();
    no_building.building = None;
    let e = refusal(o.issue(&request(Some(no_building), 1), &cx()));
    assert_eq!(e.code, codes::ADAPTER_FAILED);
    let e = refusal(o.issue(&request(Some(orbita_address()), -2), &cx()));
    assert_eq!(e.code, codes::ADAPTER_FAILED);
    assert_eq!(orbita.calls().last().unwrap(), "dv_disconnect");
}
