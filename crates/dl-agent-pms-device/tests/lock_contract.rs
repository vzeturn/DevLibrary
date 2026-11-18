//! The ContractTests.cs case that runs the lock.erase golden payload through the lock
//! router with the fake TP native (the fixture is read in place from
//! apps/device-agent/contract).

mod lock_fakes;

use dl_agent_pms_device::adapters::DeviceAdapter;
use dl_agent_pms_device::adapters::lock::{LockNatives, SmileLockRouter};
use dl_agent_pms_device::config::LockOptions;
use dl_agent_pms_device::contracts::{EraseRequest, decode_payload, parse_json};
use lock_fakes::*;

const LOCK_ERASE_PAYLOAD: &str = include_str!("contract/lock.erase.payload.json");

#[test]
fn contract_lock_erase_payload_parses_and_runs_on_the_lock_router_with_the_fake_native() {
    let payload = parse_json(LOCK_ERASE_PAYLOAD.as_bytes()).unwrap();
    let request: EraseRequest = decode_payload(&payload).unwrap();
    let a = request.lock_address.as_ref().unwrap();
    let v = request.vendor.as_ref().unwrap();
    assert_eq!(
        (
            request.room.as_deref(),
            request.card_number.as_deref(),
            a.address.as_deref(),
            v.mf_lock_name.as_deref()
        ),
        (Some("101"), Some("A1B2C3D4"), Some("0101"), Some("hls"))
    );
    let natives = LockNatives {
        tp: FakeTp::new().native(),
        ..LockNatives::default()
    };
    let router = SmileLockRouter::create(&LockOptions::default(), natives);
    let result = router.erase(&request, &cx()).unwrap();
    assert_eq!(
        result,
        parse_json(br#"{"cardNumber":"A1B2C3D4","erased":true}"#).unwrap()
    );
}
