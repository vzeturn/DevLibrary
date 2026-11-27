//! `order.po_number` wire contract (RT-1413): the snake_case shape of the cloud's
//! `OrderPoNumberPayload` (the same literal is pinned against System.Text.Json in
//! `PoNumberPayloadTests`), the `Problem` messages (same text, same order), the aggregate
//! and the delivery.

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const PAYLOAD: &str = r#"{"rvc_no":4,"check_no":7001,"po_number":"101-Nguyen Van An"}"#;

fn payload() -> OrderPoNumberPayload {
    OrderPoNumberPayload {
        rvc_no: 4,
        check_no: 7001,
        po_number: Some("101-Nguyen Van An".into()),
    }
}

#[test]
fn the_verb_is_a_durable_table_verb_queued_apart_from_the_check() {
    assert_eq!(table_command_types::PO_NUMBER, "order.po_number");
    assert!(table_command_types::ALL.contains(&table_command_types::PO_NUMBER));
    let spec = site_command_contract::find(Some(table_command_types::PO_NUMBER)).unwrap();
    assert_eq!(spec.delivery, SiteCommandDelivery::Durable);
    assert_eq!(spec.max_attempts, 8);
    assert_eq!(
        OrderPoNumberPayload::aggregate_key(4, 7001),
        "checkguest:4:7001"
    );
}

#[test]
fn the_payload_reads_and_writes_the_dotnet_json() {
    assert_eq!(payload().to_json(), PAYLOAD);
    assert_eq!(OrderPoNumberPayload::from_json(PAYLOAD).unwrap(), payload());
    let cleared =
        OrderPoNumberPayload::from_json(r#"{"rvc_no":4,"check_no":7001,"po_number":""}"#).unwrap();
    assert_eq!(cleared.po_number.as_deref(), Some(""));
    assert_eq!(cleared.problem(), None);
}

#[test]
fn problems_match_the_dotnet_text_in_order() {
    let with = |f: fn(&mut OrderPoNumberPayload)| {
        let mut p = payload();
        f(&mut p);
        p.problem()
    };
    assert_eq!(payload().problem(), None);
    assert_eq!(with(|p| p.rvc_no = 0), Some("rvc_no must be positive"));
    assert_eq!(with(|p| p.check_no = 0), Some("check_no must be positive"));
    assert_eq!(
        with(|p| p.po_number = None),
        Some("po_number is required (empty clears it)")
    );
    assert_eq!(
        with(|p| p.po_number = Some("x".repeat(51))),
        Some("po_number must be at most 50 characters")
    );
    assert_eq!(with(|p| p.po_number = Some("ễ".repeat(50))), None);
    assert_eq!(
        with(|p| p.po_number = Some("101-An\n".into())),
        Some("po_number must not carry control characters")
    );
    let missing = OrderPoNumberPayload::from_json(r#"{"rvc_no":4,"check_no":7001}"#).unwrap();
    assert_eq!(
        missing.problem(),
        Some("po_number is required (empty clears it)")
    );
}
