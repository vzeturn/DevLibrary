//! Port of SimulatedLockAdapterTests.cs (simulated lock, not-connected adapter, factory).

use dl_agent_pms_device::adapters::{
    self, Cx, DeviceAdapter, NOT_CONNECTED, NotConnectedAdapter, SIMULATED_LOCK, SMILE_LOCK,
    SimulatedLockAdapter, pos,
};
use dl_agent_pms_device::config::LockOptions;
use dl_agent_pms_device::contracts::{
    CancelAllRequest, EraseRequest, KeyCardRequest, PosKickRequest, ReadRequest, codes,
    parse_wall_clock,
};
use dl_json::Value;

fn request(room: &str, cards: i32) -> KeyCardRequest {
    KeyCardRequest {
        room: Some(room.into()),
        from: parse_wall_clock("2026-10-14T14:00:00").unwrap(),
        to: parse_wall_clock("2026-10-16T12:00:00").unwrap(),
        cards,
        ..KeyCardRequest::default()
    }
}

fn cancel(room: &str) -> CancelAllRequest {
    CancelAllRequest {
        room: Some(room.into()),
        ..CancelAllRequest::default()
    }
}

fn card_numbers(v: &Value) -> Vec<String> {
    v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["cardNumber"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn i0_a1_issue_numbers_cards_sequentially_per_adapter() {
    let a = SimulatedLockAdapter::new();
    let json = a.issue(&request("101", 2), &Cx::unbounded()).unwrap();
    assert_eq!(card_numbers(&json), ["SIM-101-1", "SIM-101-2"]);
}

#[test]
fn i0_a2_read_before_any_issue_fails_with_device_no_card() {
    let e = SimulatedLockAdapter::new()
        .read(&ReadRequest::default(), &Cx::unbounded())
        .unwrap_err();
    assert_eq!(e.code, codes::NO_CARD);
    assert_eq!(e.message, "No card on the simulated encoder");
}

#[test]
fn i0_a3_read_returns_the_last_encoded_card() {
    let cx = Cx::unbounded();
    let a = SimulatedLockAdapter::new();
    a.issue(&request("205", 1), &cx).unwrap();
    let read = a.read(&ReadRequest::default(), &cx).unwrap();
    assert_eq!(read["cardNumber"], "SIM-205-1");
    assert_eq!(read["valid"], true);
    assert_eq!(read["room"], "205");
    assert_eq!(read["from"], "2026-10-14T14:00:00");
    assert_eq!(read["to"], "2026-10-16T12:00:00");
}

#[test]
fn i0_a4_cancel_all_echoes_the_room() {
    let json = SimulatedLockAdapter::new()
        .cancel_all(&cancel("101"), &Cx::unbounded())
        .unwrap();
    assert_eq!(json["cancelled"], true);
    assert_eq!(json["room"], "101");
}

#[test]
fn i1_a5_a_new_key_invalidates_the_rooms_earlier_cards_but_a_copy_does_not() {
    let cx = Cx::unbounded();
    let a = SimulatedLockAdapter::new();
    a.issue(&request("101", 1), &cx).unwrap(); // SIM-101-1
    a.duplicate(&request("101", 1), &cx).unwrap(); // SIM-101-2
    a.issue(&request("102", 1), &cx).unwrap(); // SIM-102-3
    assert!(a.is_valid("SIM-101-1"));
    assert!(a.is_valid("SIM-101-2"));

    a.issue(&request("101", 1), &cx).unwrap(); // SIM-101-4
    assert!(!a.is_valid("SIM-101-1"));
    assert!(!a.is_valid("SIM-101-2"));
    assert!(a.is_valid("SIM-101-4"));
    assert!(a.is_valid("SIM-102-3"));
}

#[test]
fn i1_a6_cancel_all_invalidates_every_card_of_the_room_and_read_reports_it() {
    let cx = Cx::unbounded();
    let a = SimulatedLockAdapter::new();
    a.issue(&request("101", 2), &cx).unwrap();
    a.cancel_all(&cancel("101"), &cx).unwrap();
    assert!(!a.is_valid("SIM-101-1"));
    assert!(!a.is_valid("SIM-101-2"));
    assert_eq!(
        a.read(&ReadRequest::default(), &cx).unwrap()["valid"],
        false
    );
}

#[test]
fn i1_a7_not_connected_adapter_refuses_every_verb_with_the_clerk_reason() {
    let cx = Cx::unbounded();
    let a = NotConnectedAdapter;
    let r = request("101", 1);
    let results = [
        a.issue(&r, &cx),
        a.duplicate(&r, &cx),
        a.extend(&r, &cx),
        a.read(&ReadRequest::default(), &cx),
        a.cancel_all(&cancel("101"), &cx),
        a.erase(&EraseRequest::default(), &cx),
    ];
    for result in results {
        let e = result.unwrap_err();
        assert_eq!(e.code, codes::NOT_CONNECTED);
        assert_eq!(e.message, "Chưa kết nối thiết bị khoá");
    }
}

#[test]
fn i1_a8_the_adapter_is_chosen_by_name() {
    let lock = LockOptions::default();
    for name in [NOT_CONNECTED, SIMULATED_LOCK, SMILE_LOCK] {
        assert_eq!(adapters::create(name, &lock).unwrap().family(), name);
    }
}

#[test]
fn i1_a9_an_unknown_adapter_name_is_refused() {
    let e = match adapters::create("simulated", &LockOptions::default()) {
        Err(e) => e,
        Ok(_) => panic!("an adapter for 'simulated'"),
    };
    assert_eq!(e.type_name, "System.ArgumentOutOfRangeException");
    assert!(e.message.starts_with(
        "Unknown device adapter; expected one of none, simulated-lock, smile-lock (Parameter 'name')"
    ));
    assert!(e.message.ends_with("Actual value was simulated."));
}

#[test]
fn erase_needs_an_issued_card_and_keeps_the_csharp_null_failure() {
    let cx = Cx::unbounded();
    let a = SimulatedLockAdapter::new();
    let erase = |n: Option<&str>| EraseRequest {
        card_number: n.map(str::to_owned),
        ..EraseRequest::default()
    };
    let e = a.erase(&erase(Some("SIM-101-1")), &cx).unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::NO_CARD,
            "Card SIM-101-1 is not on the simulated encoder"
        )
    );
    a.issue(&request("101", 1), &cx).unwrap();
    let ok = a.erase(&erase(Some("SIM-101-1")), &cx).unwrap();
    assert_eq!(
        ok,
        dl_json::json!({"cardNumber": "SIM-101-1", "erased": true})
    );
    assert!(!a.is_valid("SIM-101-1"));
    let e = a.erase(&erase(None), &cx).unwrap_err();
    assert_eq!(e.code, codes::ADAPTER_FAILED);
    let e = a.issue(&request("101", -1), &cx).unwrap_err();
    assert_eq!(e.code, codes::ADAPTER_FAILED);
}

#[test]
fn placeholders_refuse_with_the_csharp_codes() {
    let cx = Cx::unbounded();
    let lock = adapters::lock::smile_lock(&LockOptions::default());
    // The real router (DA2): no LockKeyType anywhere is "Lock type is not set".
    let e = lock.read(&ReadRequest::default(), &cx).unwrap_err();
    assert_eq!(e.code, codes::NOT_CONFIGURED);
    // The real POS router (DA3): a payload without a target is the C# NullReferenceException.
    let e = pos::router()
        .kick(&PosKickRequest::default(), &cx)
        .unwrap_err();
    assert_eq!(e.code, codes::ADAPTER_FAILED);
}
