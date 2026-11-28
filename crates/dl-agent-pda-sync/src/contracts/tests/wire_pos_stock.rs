//! `pos.stock_balance` wire contract (RT-1024, ADR 0165): the snake_case shape the cloud's
//! `PosStockBalance*` records (System.Text.Json, `SiteCommandJson`) write and read, the
//! `Problem` messages (same text), and that the Rust agent does not execute it yet.

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

// The exact text `SiteCommandContractTests.PosStockBalance_*` asserts on the .NET side.
const PAYLOAD: &str = r#"{"store_code":4,"item_codes":["OIL-01","TOWEL"]}"#;
const RESULT: &str = r#"{"store_code":4,"database":"SMILE_BO","store_rvc":12,"items":[{"item_code":"OIL-01","qty":3.75}]}"#;

fn payload(store_code: i32, codes: &[&str], reason: Option<&str>) -> PosStockBalancePayload {
    PosStockBalancePayload {
        store_code,
        item_codes: codes.iter().map(|c| (*c).to_string()).collect(),
        reason: reason.map(str::to_string),
    }
}

#[test]
fn names_match_the_dotnet_constants_and_the_agent_delivers_it_durably() {
    assert_eq!(pos_stock_types::STOCK_BALANCE, "pos.stock_balance");
    assert_eq!(PosStockBalancePayload::aggregate_key(4), "pos-stock:4");
    let spec = site_command_contract::find(Some(pos_stock_types::STOCK_BALANCE)).unwrap();
    assert_eq!(
        (spec.delivery, spec.max_attempts),
        (SiteCommandDelivery::Durable, 3)
    );
}

#[test]
fn payload_and_result_round_trip_in_the_dotnet_shape() {
    let p = PosStockBalancePayload::from_json(PAYLOAD).unwrap();
    assert_eq!(p, payload(4, &["OIL-01", "TOWEL"], None));
    assert_eq!(p.problem(), None);
    assert_eq!(p.to_json(), PAYLOAD);

    let r = PosStockBalanceResult::from_json(RESULT).unwrap();
    assert_eq!(r.items[0].qty, "3.75".parse::<Decimal>().unwrap());
    assert_eq!(r.store_rvc, 12);
    assert_eq!(r.to_json(), RESULT);
}

#[test]
fn problems_use_the_dotnet_messages() {
    let too_many: Vec<String> = (0..=PosStockBalancePayload::MAX_ITEMS)
        .map(|i| format!("I{i}"))
        .collect();
    let long = "X".repeat(21);
    let reason = "r".repeat(301);
    let cases: Vec<(PosStockBalancePayload, &str)> = vec![
        (payload(0, &["A"], None), "store_code must be 1..255"),
        (payload(256, &["A"], None), "store_code must be 1..255"),
        (payload(4, &[], None), "item_codes must hold 1..1000 codes"),
        (
            PosStockBalancePayload {
                store_code: 4,
                item_codes: too_many,
                reason: None,
            },
            "item_codes must hold 1..1000 codes",
        ),
        (
            payload(4, &["  "], None),
            "each item code must be 1..20 characters",
        ),
        (
            payload(4, &[long.as_str()], None),
            "each item code must be 1..20 characters",
        ),
        (
            payload(4, &["oil-01", "OIL-01 "], None),
            "an item code is repeated",
        ),
        (
            payload(4, &["A"], Some(reason.as_str())),
            "reason must be at most 300 characters",
        ),
    ];
    for (p, message) in cases {
        assert_eq!(p.problem().as_deref(), Some(message));
    }
}
