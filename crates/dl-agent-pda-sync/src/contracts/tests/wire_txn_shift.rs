//! Shift and cash verbs of ADR 0160 (RT-0935): allow-list, shape validation (`Problem`) and the
//! wire. Port of `tests/MM.PdaOrder.Sync.Tests/TxnShiftContractTests.cs` (same cases, same
//! expected values).

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::*;

const CLOSED: &str = r#"{"shift_key":9,"version":1,"rvc":4,"cashier":"cashier-1","start_time":"2026-11-24T08:00:00","open_date":"2026-11-24T00:00:00","shift_status":2,"cash_open":2000000,"cash_in":0,"cash_out":0,"cash_end":2300000,"end_time":"2026-11-24T17:00:00","close_date":"2026-11-24T00:00:00","notes":"Ca sang","money":[{"cr_code":1,"note_name":"500000","qty":4,"amount":2000000,"trn_code":1,"trn_time":"2026-11-24T08:00:00.123"}],"actor":"cashier-1"}"#;
const CASH: &str = r#"{"shift_key":9,"version":2,"movement_key":71,"rvc":4,"cashier":"cashier-1","amount":300000,"reason":"Thu them","business_date":"2026-11-24T00:00:00","at":"2026-11-24T10:30:15","shift_cash_in":300000,"shift_cash_out":0,"actor":"cashier-1"}"#;

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn open() -> TxnShiftPayload {
    TxnShiftPayload {
        shift_key: 9,
        version: 1,
        rvc: 4,
        cashier: "cashier-1".into(),
        start_time: "2026-11-24T08:00:00".into(),
        open_date: "2026-11-24T00:00:00".into(),
        shift_status: 1,
        cash_open: dec("2000000"),
        cash_in: dec("0"),
        cash_out: dec("0"),
        cash_end: None,
        end_time: None,
        close_date: None,
        notes: Some("Ca sáng".into()),
        money: Some(vec![line("500000", 4, "2026-11-24T08:00:00.123")]),
        actor: "cashier-1".into(),
    }
}

fn line(note_name: &str, qty: i32, trn_time: &str) -> TxnShiftMoneyLine {
    TxnShiftMoneyLine {
        cr_code: 1,
        note_name: note_name.into(),
        qty,
        amount: 2_000_000,
        trn_code: 1,
        trn_time: trn_time.into(),
    }
}

fn closed() -> TxnShiftPayload {
    TxnShiftPayload {
        shift_status: 2,
        cash_end: Some(dec("2300000")),
        end_time: Some("2026-11-24T17:00:00".into()),
        close_date: Some("2026-11-24T00:00:00".into()),
        ..open()
    }
}

fn cash() -> TxnCashPayload {
    TxnCashPayload {
        shift_key: 9,
        version: 2,
        movement_key: 71,
        rvc: 4,
        cashier: "cashier-1".into(),
        amount: dec("300000"),
        reason: "Thu thêm".into(),
        business_date: "2026-11-24T00:00:00".into(),
        at: "2026-11-24T10:30:15".into(),
        shift_cash_in: dec("300000"),
        shift_cash_out: dec("0"),
        actor: "cashier-1".into(),
    }
}

#[test]
fn the_four_verbs_are_txn_commands_registered_as_durable_and_only_shift_state_verbs_cover_each_other()
 {
    use txn_command_types as x;
    for t in [x::SHIFT_OPEN, x::SHIFT_CLOSE, x::CASH_IN, x::CASH_OUT] {
        assert!(x::is_txn(Some(t)), "{t}");
        assert_eq!(
            site_command_contract::find(Some(t)).map(|s| s.delivery),
            Some(SiteCommandDelivery::Durable),
            "{t}"
        );
    }
    assert_eq!(
        (x::SHIFT_OPEN, x::SHIFT_CLOSE, x::CASH_IN, x::CASH_OUT),
        (
            "txn.shift_open",
            "txn.shift_close",
            "txn.cash_in",
            "txn.cash_out"
        )
    );
    assert!(x::is_covering(Some(x::SNAPSHOT)));
    assert!(x::is_covering(Some(x::SHIFT_OPEN)));
    assert!(x::is_covering(Some(x::SHIFT_CLOSE)));
    assert!(
        !x::is_covering(Some(x::CASH_IN)),
        "a cash movement is an event: skipping it would drop money"
    );
    assert!(!x::is_covering(Some(x::CASH_OUT)));
    assert!(!x::is_covering(Some("txn.other")));
    assert!(!x::is_covering(None));
}

#[test]
fn shift_is_not_a_snapshot_scope_so_a_generic_snapshot_can_never_write_an_identity_table() {
    assert!(txn_scopes::find(txn_scopes::SHIFT).is_none());
    assert!(txn_scopes::find(txn_scopes::BILL).is_some());
    assert_eq!(
        txn_scopes::aggregate_key(txn_scopes::SHIFT, 9),
        "txn:shift:9"
    );
    let p = TxnSnapshotPayload {
        scope: txn_scopes::SHIFT.into(),
        scope_key: 9,
        version: 1,
        tables: vec![],
        actor: "a".into(),
    };
    assert_eq!(p.problem().as_deref(), Some("Scope is not allowed."));
}

#[test]
fn a_well_formed_open_close_and_cash_payload_has_no_problem_for_its_own_verbs_only() {
    use txn_command_types as x;
    assert_eq!(open().problem(Some(x::SHIFT_OPEN)), None);
    assert_eq!(closed().problem(Some(x::SHIFT_CLOSE)), None);
    assert_eq!(cash().problem(Some(x::CASH_IN)), None);
    assert_eq!(cash().problem(Some(x::CASH_OUT)), None);
    let not_shift = Some("Type is not a shift verb.");
    assert_eq!(open().problem(Some(x::CASH_IN)).as_deref(), not_shift);
    assert_eq!(open().problem(Some(x::SNAPSHOT)).as_deref(), not_shift);
    assert_eq!(open().problem(None).as_deref(), not_shift);
    assert_eq!(
        cash().problem(Some(x::SHIFT_OPEN)).as_deref(),
        Some("Type is not a cash verb.")
    );
}

type ShiftChange = Box<dyn Fn(&mut TxnShiftPayload)>;

#[test]
fn a_malformed_shift_payload_names_its_problem() {
    use txn_command_types::{SHIFT_CLOSE as CLOSE, SHIFT_OPEN as OPEN};
    let cases: Vec<(&str, bool, ShiftChange, &str, &str)> = vec![
        (
            "zero shift key",
            false,
            Box::new(|p| p.shift_key = 0),
            OPEN,
            "ShiftKey",
        ),
        (
            "zero version",
            false,
            Box::new(|p| p.version = 0),
            OPEN,
            "Version",
        ),
        ("no rvc", false, Box::new(|p| p.rvc = 0), OPEN, "Rvc"),
        (
            "blank cashier",
            false,
            Box::new(|p| p.cashier = " ".into()),
            OPEN,
            "Cashier",
        ),
        (
            "long cashier",
            false,
            Box::new(|p| p.cashier = "c".repeat(51)),
            OPEN,
            "Cashier",
        ),
        (
            "no actor",
            false,
            Box::new(|p| p.actor = String::new()),
            OPEN,
            "Actor",
        ),
        (
            "offset in start",
            false,
            Box::new(|p| p.start_time = "2026-11-24T08:00:00+07:00".into()),
            OPEN,
            "StartTime",
        ),
        (
            "date only open date",
            false,
            Box::new(|p| p.open_date = "2026-11-24".into()),
            OPEN,
            "OpenDate",
        ),
        (
            "negative status",
            false,
            Box::new(|p| p.shift_status = -1),
            OPEN,
            "ShiftStatus",
        ),
        (
            "three decimals",
            false,
            Box::new(|p| p.cash_open = dec("1.005")),
            OPEN,
            "CashOpen",
        ),
        (
            "negative cash in",
            false,
            Box::new(|p| p.cash_in = dec("-1")),
            OPEN,
            "CashOpen",
        ),
        (
            "long notes",
            false,
            Box::new(|p| p.notes = Some("n".repeat(201))),
            OPEN,
            "Notes",
        ),
        (
            "open with end time",
            false,
            Box::new(|p| p.end_time = Some("2026-11-24T17:00:00".into())),
            OPEN,
            "open shift",
        ),
        (
            "open with cash end",
            false,
            Box::new(|p| p.cash_end = Some(dec("1"))),
            OPEN,
            "open shift",
        ),
        (
            "close without cash end",
            true,
            Box::new(|p| p.cash_end = None),
            CLOSE,
            "CashEnd",
        ),
        (
            "close without end time",
            true,
            Box::new(|p| p.end_time = None),
            CLOSE,
            "EndTime",
        ),
        (
            "close without close date",
            true,
            Box::new(|p| p.close_date = None),
            CLOSE,
            "CloseDate",
        ),
        (
            "no money list",
            false,
            Box::new(|p| p.money = None),
            OPEN,
            "Money",
        ),
        (
            "too many lines",
            false,
            Box::new(|p| p.money = Some(vec![line("500000", 4, "2026-11-24T08:00:00"); 101])),
            OPEN,
            "Too many",
        ),
        (
            "blank note name",
            false,
            Box::new(|p| p.money = Some(vec![line(" ", 1, "2026-11-24T08:00:00")])),
            OPEN,
            "NoteName",
        ),
        (
            "negative qty",
            false,
            Box::new(|p| p.money = Some(vec![line("1000", -1, "2026-11-24T08:00:00")])),
            OPEN,
            "negative",
        ),
        (
            "bad line time",
            false,
            Box::new(|p| p.money = Some(vec![line("1000", 1, "yesterday")])),
            OPEN,
            "TrnTime",
        ),
    ];
    for (name, from_closed, change, type_, mentions) in cases {
        let mut p = if from_closed { closed() } else { open() };
        change(&mut p);
        let problem = p.problem(Some(type_)).unwrap_or_default();
        assert!(problem.contains(mentions), "{name}: {problem}");
    }
}

type CashChange = Box<dyn Fn(&mut TxnCashPayload)>;

#[test]
fn a_malformed_cash_payload_names_its_problem() {
    let cases: Vec<(&str, CashChange, &str)> = vec![
        ("zero amount", Box::new(|p| p.amount = dec("0")), "Amount"),
        (
            "negative amount",
            Box::new(|p| p.amount = dec("-5")),
            "Amount",
        ),
        (
            "three decimals",
            Box::new(|p| p.amount = dec("1.001")),
            "Amount",
        ),
        (
            "no movement key",
            Box::new(|p| p.movement_key = 0),
            "MovementKey",
        ),
        ("no reason", Box::new(|p| p.reason = " ".into()), "Reason"),
        (
            "long reason",
            Box::new(|p| p.reason = "r".repeat(201)),
            "Reason",
        ),
        (
            "bad business date",
            Box::new(|p| p.business_date = "24/11/2026".into()),
            "BusinessDate",
        ),
        (
            "bad time",
            Box::new(|p| p.at = "2026-11-24T10:30:15Z".into()),
            "At",
        ),
        (
            "negative total",
            Box::new(|p| p.shift_cash_out = dec("-1")),
            "ShiftCash",
        ),
        ("no actor", Box::new(|p| p.actor = String::new()), "Actor"),
        ("no shift", Box::new(|p| p.shift_key = -3), "ShiftKey"),
    ];
    for (name, change, mentions) in cases {
        let mut p = cash();
        change(&mut p);
        let problem = p
            .problem(Some(txn_command_types::CASH_IN))
            .unwrap_or_default();
        assert!(problem.contains(mentions), "{name}: {problem}");
    }
}

#[test]
fn payloads_and_results_round_trip_in_snake_case_through_the_wire_context() {
    let closed_ascii = TxnShiftPayload {
        notes: Some("Ca sang".into()),
        ..closed()
    };
    assert_eq!(closed_ascii.to_json(), CLOSED);
    let read = TxnShiftPayload::from_json(CLOSED).unwrap();
    assert_eq!(read, closed_ascii);
    assert_eq!(read.problem(Some(txn_command_types::SHIFT_CLOSE)), None);
    assert_eq!(
        TxnShiftPayload::from_element(&closed().to_element()),
        Some(closed())
    );

    let cash_ascii = TxnCashPayload {
        reason: "Thu them".into(),
        ..cash()
    };
    assert_eq!(cash_ascii.to_json(), CASH);
    assert_eq!(TxnCashPayload::from_json(CASH).unwrap(), cash_ascii);
    assert_eq!(
        TxnCashPayload::from_element(&cash().to_element()),
        Some(cash())
    );

    let res = TxnShiftResult {
        aggregate: "shift:9".into(),
        version: 2,
        outcome: txn_outcomes::APPLIED.into(),
        applied_version: 2,
        hash: "a".repeat(64),
        row_counts: vec![("CashierShift".into(), 1), ("ShiftMoney".into(), 1)],
        shift_no: 17,
        trn_seq: Some(33),
        cash_log_hash: Some("b".repeat(64)),
    };
    let json = res.to_json();
    assert!(json.contains(r#""row_counts":{"CashierShift":1,"ShiftMoney":1},"shift_no":17,"trn_seq":33,"cash_log_hash":"bbbb"#));
    assert_eq!(TxnShiftResult::from_json(&json).unwrap(), res);
    let stale = TxnShiftResult {
        aggregate: "a".into(),
        version: 1,
        outcome: txn_outcomes::STALE.into(),
        applied_version: 1,
        hash: "h".into(),
        row_counts: vec![],
        shift_no: 1,
        trn_seq: None,
        cash_log_hash: None,
    };
    assert_eq!(
        stale.to_json(),
        r#"{"aggregate":"a","version":1,"outcome":"stale","applied_version":1,"hash":"h","row_counts":{},"shift_no":1}"#,
        "nulls are omitted"
    );
}

#[test]
fn a_shift_payload_with_a_wrong_field_type_deserializes_to_an_error_rather_than_panicking() {
    assert!(TxnShiftPayload::from_json(r#"{"shift_key":"nine","version":1}"#).is_err());
    assert!(TxnCashPayload::from_json(r#"{"shift_key":9,"amount":"lots"}"#).is_err());
}

#[test]
fn moments_and_amounts_follow_the_dotnet_rules() {
    assert!(txn_is_moment(Some("2026-11-24T08:00:00")));
    assert!(txn_is_moment(Some("2026-11-24T08:00:00.1")));
    assert!(txn_is_moment(Some("2026-11-24T08:00:00.123")));
    assert!(!txn_is_moment(Some("2026-11-24T08:00:00.1234")));
    assert!(!txn_is_moment(Some("2026-11-24T08:00:00.")));
    assert!(!txn_is_moment(Some("2026-11-24T08:00:00Z")));
    assert!(!txn_is_moment(Some("2026-11-24")));
    assert!(!txn_is_moment(None));
    assert!(txn_is_amount(dec("0")));
    assert!(txn_is_amount(dec("1.50")));
    assert!(txn_is_amount(dec("9999999999999999.99")));
    assert!(!txn_is_amount(dec("10000000000000000")));
    assert!(!txn_is_amount(dec("-0.01")));
    assert!(!txn_is_amount(dec("0.001")));
}
