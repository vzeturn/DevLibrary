//! `txn.stock_deplete` / `txn.stock_restore` wire contract (ADR 0164, RT-1022): the
//! snake_case shape the cloud's `TxnStock*` records (System.Text.Json, `SiteCommandJson`)
//! write and read, the `Problem` messages (same text) and the result hash lines.

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const DEPLETE: &str = r#"{"rvc":4,"check_no":97,"version":2,"business_date":"2026-11-24","lines":[{"item_code":"TOWEL","store_code":4,"qty":2},{"item_code":"OIL-01","store_code":4,"qty":0.25}],"actor":"AN"}"#;
const RESTORE: &str =
    r#"{"rvc":4,"check_no":97,"version":3,"reason":"bill reopened","actor":"AN"}"#;
const RESULT: &str = r#"{"aggregate":"stock:97","version":2,"outcome":"applied","applied_version":2,"lines":2,"net_qty":2.25,"hash":"269a4e37989f6d0fda1d619d4c554d39377da3fe5242e9829ab13fcb27350bb6"}"#;

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

type Change = Box<dyn Fn(&mut TxnStockDepletePayload)>;

fn deplete() -> TxnStockDepletePayload {
    TxnStockDepletePayload::from_json(DEPLETE).unwrap()
}

#[test]
fn names_match_the_dotnet_constants() {
    assert_eq!(txn_stock_types::DEPLETE, "txn.stock_deplete");
    assert_eq!(txn_stock_types::RESTORE, "txn.stock_restore");
    assert!(txn_command_types::is_txn(Some(txn_stock_types::DEPLETE)));
    assert_eq!(txn_stock_scope::aggregate(97), "stock:97");
    assert_eq!(txn_stock_scope::aggregate_key(97), "txn:stock:97");
    assert_eq!(
        txn_stock_reasons::COST_METHOD_UNSUPPORTED,
        "txn_stock_cost_method_unsupported"
    );
    assert_eq!(
        txn_stock_reasons::ACCOUNT_INVALID,
        "txn_stock_account_invalid"
    );
}

#[test]
fn payloads_and_result_round_trip_in_the_dotnet_shape() {
    let p = deplete();
    assert_eq!(p.lines.len(), 2);
    assert_eq!(p.lines[1].qty, dec("0.25"));
    assert_eq!(p.problem(), None);
    assert_eq!(p.to_json(), DEPLETE);

    let r = TxnStockRestorePayload::from_json(RESTORE).unwrap();
    assert_eq!(r.problem(), None);
    assert_eq!(r.to_json(), RESTORE);

    let res = TxnStockResult::from_json(RESULT).unwrap();
    assert_eq!(res.net_qty, dec("2.25"));
    assert_eq!(res.to_json(), RESULT);
}

#[test]
fn hash_lines_follow_the_dotnet_format() {
    assert_eq!(stock_hash_line("TOWEL", 4, dec("2")), "TOWEL|4|-2\n");
    assert_eq!(
        stock_hash_line("OIL-01", 4, dec("0.2500")),
        "OIL-01|4|-0.25\n"
    );
    assert_eq!(stock_hash_line("X", 1, dec("1.5")), "X|1|-1.5\n");
}

#[test]
fn deplete_problems_use_the_dotnet_messages() {
    let cases: Vec<(Change, &str)> = vec![
        (Box::new(|p| p.rvc = 0), "Rvc must be 1..255."),
        (Box::new(|p| p.check_no = 0), "CheckNo must be positive."),
        (Box::new(|p| p.version = 0), "Version must be positive."),
        (
            Box::new(|p| p.business_date = "2026-02-30".into()),
            "BusinessDate must be yyyy-MM-dd.",
        ),
        (
            Box::new(|p| p.business_date = "24/11/2026".into()),
            "BusinessDate must be yyyy-MM-dd.",
        ),
        (Box::new(|p| p.lines.clear()), "Lines must not be empty."),
        (
            Box::new(|p| p.lines[0].item_code = "  ".into()),
            "ItemCode must be 1..20 characters.",
        ),
        (
            Box::new(|p| p.lines[0].item_code = "X".repeat(21)),
            "ItemCode must be 1..20 characters.",
        ),
        (
            Box::new(|p| p.lines[0].store_code = 256),
            "StoreCode must be 1..255.",
        ),
        (
            Box::new(|p| p.lines[0].qty = dec("0")),
            "Qty must be positive with at most 4 decimals.",
        ),
        (
            Box::new(|p| p.lines[0].qty = dec("0.00001")),
            "Qty must be positive with at most 4 decimals.",
        ),
        (
            Box::new(|p| p.lines[0].qty = dec("1000000000")),
            "Qty must be positive with at most 4 decimals.",
        ),
        (
            Box::new(|p| p.lines[1].item_code = " TOWEL".into()),
            "A line is repeated for the same item and store.",
        ),
        (Box::new(|p| p.actor = " ".into()), "Actor is required."),
    ];
    for (change, message) in cases {
        let mut p = deplete();
        change(&mut p);
        assert_eq!(p.problem().as_deref(), Some(message));
    }

    let mut many = deplete();
    many.lines = (0..501)
        .map(|i| TxnStockLine {
            item_code: format!("I{i}"),
            store_code: 4,
            qty: dec("1"),
        })
        .collect();
    assert_eq!(many.problem().as_deref(), Some("Too many lines."));

    let null_line = DEPLETE.replace(r#"{"item_code":"TOWEL","store_code":4,"qty":2}"#, "null");
    assert_eq!(
        TxnStockDepletePayload::from_json(&null_line)
            .unwrap()
            .problem()
            .as_deref(),
        Some("ItemCode must be 1..20 characters.")
    );
}

#[test]
fn restore_problems_use_the_dotnet_messages() {
    let mut r = TxnStockRestorePayload::from_json(RESTORE).unwrap();
    r.reason = "x".repeat(201);
    assert_eq!(
        r.problem().as_deref(),
        Some("Reason must be 1..200 characters.")
    );
    r.reason = "ở".repeat(200);
    assert_eq!(r.problem(), None);
    r.reason = String::new();
    assert_eq!(
        r.problem().as_deref(),
        Some("Reason must be 1..200 characters.")
    );
}
