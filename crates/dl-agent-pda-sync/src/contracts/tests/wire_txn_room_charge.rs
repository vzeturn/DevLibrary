//! `txn.room_charge` / `txn.room_charge_void` wire contract (ADR 0162, RT-0908): names,
//! `Problem` messages in the .NET order, the snake_case shape and the hash lines.

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const CHARGE: &str = r#"{"rvc":4,"check_no":70001,"version":1,"room_no":"0501","folio_num":0,"trn_code":9,"trn_sub":1,"amount":250000,"meal_period":0,"cashier":"AN","at":"2026-11-24T10:15:00","actor":"cloud"}"#;
const VOID: &str =
    r#"{"rvc":4,"check_no":70001,"version":2,"reason":"bill voided","actor":"cloud"}"#;
const RESULT: &str = r#"{"aggregate":"room_charge:70001","version":1,"outcome":"applied","applied_version":1,"folio_num":5001,"active_amount":250000,"active_rows":1,"hash":"h"}"#;

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn charge() -> TxnRoomChargePayload {
    TxnRoomChargePayload::from_json(CHARGE).unwrap()
}

fn void() -> TxnRoomChargeVoidPayload {
    TxnRoomChargeVoidPayload::from_json(VOID).unwrap()
}

#[test]
fn names_match_the_dotnet_constants() {
    use txn_command_types as x;
    assert_eq!(
        (x::ROOM_CHARGE, x::ROOM_CHARGE_VOID),
        ("txn.room_charge", "txn.room_charge_void")
    );
    for t in [x::ROOM_CHARGE, x::ROOM_CHARGE_VOID] {
        assert!(x::is_txn(Some(t)));
        assert!(!x::is_covering(Some(t)));
        assert_eq!(
            site_command_contract::find(Some(t)).map(|s| s.delivery),
            Some(SiteCommandDelivery::Durable)
        );
    }
    assert_eq!(txn_room_charge_scope::SCOPE, "room_charge");
    assert_eq!(txn_room_charge_scope::MAX_ROOM_LENGTH, 10);
    assert_eq!(txn_room_charge_scope::aggregate(70001), "room_charge:70001");
    assert_eq!(
        txn_room_charge_scope::aggregate_key(70001),
        "txn:room_charge:70001"
    );
    assert_eq!(TxnRoomChargePayload::MAX_COMMENT_LENGTH, 200);
}

#[test]
fn payloads_and_result_round_trip_in_the_dotnet_shape() {
    let p = charge();
    assert_eq!(p.amount, dec("250000"));
    assert_eq!(p.comment, None);
    assert_eq!(p.problem(), None);
    assert_eq!(p.to_json(), CHARGE, "a null comment is omitted");
    let commented = TxnRoomChargePayload {
        comment: Some("table 7".into()),
        ..charge()
    };
    let json = commented.to_json();
    assert!(json.contains(r#""meal_period":0,"comment":"table 7","cashier":"AN""#));
    assert_eq!(TxnRoomChargePayload::from_json(&json).unwrap(), commented);

    let v = void();
    assert_eq!(v.problem(), None);
    assert_eq!(v.to_json(), VOID);

    let r = TxnRoomChargeResult::from_json(RESULT).unwrap();
    assert_eq!(r.folio_num, 5001);
    assert_eq!(r.active_amount, dec("250000"));
    assert_eq!(r.to_json(), RESULT);
}

#[test]
fn hash_lines_follow_the_dotnet_format() {
    assert_eq!(
        room_charge_hash_line(1000, dec("250000")),
        "1000|250000.00\n"
    );
    assert_eq!(room_charge_hash_line(7, dec("1.5")), "7|1.50\n");
    assert_eq!(room_charge_hash_line(8, dec("1.0000")), "8|1.00\n");
    assert_eq!(
        room_charge_hash_line(9, dec("1.005")),
        "9|1.01\n",
        "0.00 rounds half away from zero"
    );
}

type Change = Box<dyn Fn(&mut TxnRoomChargePayload)>;

#[test]
fn charge_problems_use_the_dotnet_messages_in_the_dotnet_order() {
    let cases: Vec<(Change, &str)> = vec![
        (Box::new(|p| p.rvc = 0), "Rvc must be 1..255."),
        (Box::new(|p| p.check_no = 0), "CheckNo must be positive."),
        (Box::new(|p| p.version = 0), "Version must be positive."),
        (
            Box::new(|p| p.room_no = " ".into()),
            "RoomNo must be 1..10 characters.",
        ),
        (
            Box::new(|p| p.room_no = "12345678901".into()),
            "RoomNo must be 1..10 characters.",
        ),
        (
            Box::new(|p| p.folio_num = -1),
            "FolioNum must not be negative.",
        ),
        (
            Box::new(|p| p.trn_code = 0),
            "TrnCode and TrnSub must identify the room charge tender.",
        ),
        (
            Box::new(|p| p.trn_sub = -1),
            "TrnCode and TrnSub must identify the room charge tender.",
        ),
        (
            Box::new(|p| p.amount = dec("0")),
            "Amount must be positive with at most 2 decimals.",
        ),
        (
            Box::new(|p| p.amount = dec("1.234")),
            "Amount must be positive with at most 2 decimals.",
        ),
        (
            Box::new(|p| p.amount = dec("100000000000")),
            "Amount must be positive with at most 2 decimals.",
        ),
        (
            Box::new(|p| p.meal_period = 100),
            "MealPeriod must be 0..99.",
        ),
        (
            Box::new(|p| p.comment = Some("c".repeat(201))),
            "Comment is too long.",
        ),
        (Box::new(|p| p.cashier = " ".into()), "Cashier is required."),
        (
            Box::new(|p| p.at = "yesterday".into()),
            "At must be a local date-time.",
        ),
        (Box::new(|p| p.actor = String::new()), "Actor is required."),
    ];
    for (change, message) in cases {
        let mut p = charge();
        change(&mut p);
        assert_eq!(p.problem().as_deref(), Some(message));
    }
    let mut padded = charge();
    padded.room_no = " 0501 ".into();
    padded.amount = dec("99999999999");
    assert_eq!(padded.problem(), None, "the room is measured trimmed");
}

#[test]
fn void_problems_use_the_dotnet_messages_in_the_dotnet_order() {
    let mut p = void();
    p.version = 0;
    assert_eq!(p.problem().as_deref(), Some("Version must be positive."));
    let mut p = void();
    p.reason = " ".into();
    assert_eq!(
        p.problem().as_deref(),
        Some("Reason must be 1..200 characters.")
    );
    let mut p = void();
    p.reason = "r".repeat(201);
    assert_eq!(
        p.problem().as_deref(),
        Some("Reason must be 1..200 characters.")
    );
    let mut p = void();
    p.actor = " ".into();
    assert_eq!(p.problem().as_deref(), Some("Actor is required."));
}
