//! `txn.stock_issue` wire contract (ADR 0168, RT-1112): the snake_case shape the cloud's
//! `TxnStockIssue*` records (System.Text.Json, nulls omitted) write and read, the `Problem`
//! messages (same text, same order) and the result hash lines. The same literals are asserted
//! against the .NET serializer in `TxnStockIssueRequestsTests`.

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const ISSUE: &str = r#"{"rvc":4,"issue_no":12,"version":2,"business_date":"2026-11-24","store_code":4,"with_store_code":7,"lines":[{"item_code":"TOWEL","qty":2},{"item_code":"OIL-01","qty":0.25}],"reason":"to room 7","actor":"AN"}"#;
const MINIMAL: &str = r#"{"rvc":4,"issue_no":12,"version":1,"business_date":"2026-11-24","store_code":4,"lines":[{"item_code":"TOWEL","qty":2}],"actor":"AN"}"#;
const RESULT: &str = r#"{"aggregate":"stockissue:12","version":2,"outcome":"applied","applied_version":2,"voucher_no":"SX0000000123","lines":2,"net_qty":-2.25,"hash":"289e03e735711d77ab00da4e897609237500afb8b748d28b810c8a84157519c6"}"#;

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

type Change = Box<dyn Fn(&mut TxnStockIssuePayload)>;

fn issue() -> TxnStockIssuePayload {
    TxnStockIssuePayload::from_json(ISSUE).unwrap()
}

#[test]
fn names_match_the_dotnet_constants() {
    assert_eq!(txn_stock_issue_types::STOCK_ISSUE, "txn.stock_issue");
    assert!(txn_command_types::is_txn(Some(
        txn_stock_issue_types::STOCK_ISSUE
    )));
    assert_eq!(txn_stock_issue_scope::aggregate(12), "stockissue:12");
    assert_eq!(
        txn_stock_issue_scope::aggregate_key(12),
        "txn:stockissue:12"
    );
    assert_eq!(txn_stock_issue_scope::MAX_LINES, 500);
    assert_eq!(TxnStockIssuePayload::MAX_REASON_LENGTH, 150);
    assert_eq!(TxnStockIssuePayload::MAX_ISSUE_NO, 999_999_999_999_999_999);
    // Like the other stock verbs: durable, ten attempts.
    let spec = site_command_contract::find(Some(txn_stock_issue_types::STOCK_ISSUE)).unwrap();
    assert_eq!(
        (spec.delivery, spec.max_attempts),
        (SiteCommandDelivery::Durable, 10)
    );
}

#[test]
fn payload_and_result_round_trip_in_the_dotnet_shape() {
    let p = issue();
    assert_eq!(p.lines.len(), 2);
    assert_eq!(p.lines[1].qty, dec("0.25"));
    assert_eq!(p.with_store_code, Some(7));
    assert_eq!(p.reason.as_deref(), Some("to room 7"));
    assert_eq!(p.problem(), None);
    assert!(!p.to_consumption());
    assert_eq!(p.to_json(), ISSUE);

    let m = TxnStockIssuePayload::from_json(MINIMAL).unwrap();
    assert_eq!(m.with_store_code, None);
    assert_eq!(m.reason, None);
    assert!(
        m.to_consumption(),
        "no destination is the consumption location"
    );
    assert_eq!(
        m.to_json(),
        MINIMAL,
        "absent options are omitted, not written as null"
    );

    let explicit_null = MINIMAL.replace(
        r#""lines""#,
        r#""with_store_code":null,"reason":null,"lines""#,
    );
    assert_eq!(TxnStockIssuePayload::from_json(&explicit_null).unwrap(), m);

    let mut same = issue();
    same.with_store_code = Some(4);
    assert!(
        same.to_consumption(),
        "the outlet itself is the consumption location"
    );

    let res = TxnStockIssueResult::from_json(RESULT).unwrap();
    assert_eq!(res.net_qty, dec("-2.25"));
    assert_eq!(res.voucher_no, "SX0000000123");
    assert_eq!(res.to_json(), RESULT);
}

#[test]
fn hash_lines_follow_the_dotnet_format() {
    assert_eq!(
        stock_issue_hash_line("TOWEL", 4, Some(7), dec("2")),
        "TOWEL|4|7|-2\n"
    );
    assert_eq!(
        stock_issue_hash_line("OIL-01", 4, None, dec("0.2500")),
        "OIL-01|4|4|-0.25\n",
        "no destination hashes as the store itself"
    );
    assert_eq!(
        stock_issue_hash_line("X", 1, Some(1), dec("1.5")),
        "X|1|1|-1.5\n"
    );
    assert_eq!(
        stock_issue_hash_line("Y", 1, None, dec("0.12345")),
        "Y|1|1|-0.12345\n",
        "0.##### keeps five decimals"
    );
}

#[test]
fn problems_use_the_dotnet_messages_in_the_dotnet_order() {
    let cases: Vec<(Change, &str)> = vec![
        (Box::new(|p| p.rvc = 0), "Rvc must be 1..255."),
        (
            Box::new(|p| p.issue_no = 0),
            "IssueNo must be 1..999999999999999999.",
        ),
        (
            Box::new(|p| p.issue_no = 1_000_000_000_000_000_000),
            "IssueNo must be 1..999999999999999999.",
        ),
        (Box::new(|p| p.version = 0), "Version must be positive."),
        (
            Box::new(|p| p.business_date = "2026-02-30".into()),
            "BusinessDate must be yyyy-MM-dd.",
        ),
        (
            Box::new(|p| p.store_code = 256),
            "StoreCode must be 1..255.",
        ),
        (
            Box::new(|p| p.with_store_code = Some(0)),
            "WithStoreCode must be 1..255 when given.",
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
            "An item is issued twice.",
        ),
        (
            Box::new(|p| p.reason = Some("x".repeat(151))),
            "Reason must be at most 150 characters.",
        ),
        (Box::new(|p| p.actor = " ".into()), "Actor is required."),
    ];
    for (change, message) in cases {
        let mut p = issue();
        change(&mut p);
        assert_eq!(p.problem().as_deref(), Some(message));
    }

    let mut many = issue();
    many.lines = (0..501)
        .map(|i| TxnStockIssueLine {
            item_code: format!("I{i}"),
            qty: dec("1"),
        })
        .collect();
    assert_eq!(many.problem().as_deref(), Some("Too many lines."));

    let mut edge = issue();
    edge.issue_no = TxnStockIssuePayload::MAX_ISSUE_NO;
    edge.reason = Some(format!("  {}  ", "ở".repeat(150)));
    assert_eq!(
        edge.problem(),
        None,
        "the reason is measured trimmed, in UTF-16 units"
    );
    edge.reason = Some(String::new());
    assert_eq!(edge.problem(), None, "an empty reason is no reason");

    let null_line = ISSUE.replace(r#"{"item_code":"TOWEL","qty":2}"#, "null");
    assert_eq!(
        TxnStockIssuePayload::from_json(&null_line)
            .unwrap()
            .problem()
            .as_deref(),
        Some("ItemCode must be 1..20 characters.")
    );
}
