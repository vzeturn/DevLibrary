//! `txn.stock_count`, `txn.stock_receipt` and `txn.stock_issue` on the in-memory SMILE_BO
//! stock (ADR 0167, 0168, 0169; port of `SiteCommandStockCountTests` / `...ReceiptTests` /
//! `...IssueTests`): one-shot vouchers, answered `stale` from a re-read of what the first
//! command wrote.

mod commands_support;

use std::sync::Arc;

use commands_support::FakeStore;
use commands_support::stock_cmd::{command, d, invalid, refused, result_of, run, stock_store};
use dl_agent_pda_sync::commands::{StockIssuePosted, txn_aggregate_hash};
use dl_agent_pda_sync::contracts::wire::{
    SyncCommand, TxnStockCountResult, TxnStockIssueResult, TxnStockReceiptResult,
    stock_count_hash_line, stock_issue_hash_line, stock_receipt_hash_line, txn_outcomes,
    txn_stock_count_types, txn_stock_issue_types, txn_stock_reasons as reasons,
    txn_stock_receipt_types,
};

type Setup = fn(&Arc<FakeStore>);

// ---- count

fn count_cmd(count_no: i64, version: i64, date: &str, lines: &str) -> SyncCommand {
    command(
        txn_stock_count_types::STOCK_COUNT,
        &format!("txn:stockcount:{count_no}"),
        &format!(
            r#"{{"rvc":1,"count_no":{count_no},"version":{version},"business_date":"{date}","store_code":1,"lines":{lines},"actor":"keeper"}}"#
        ),
    )
}

fn counted(a: &str, b: &str) -> String {
    format!(r#"[{{"item_code":"A","counted_qty":{a}}},{{"item_code":"B","counted_qty":{b}}}]"#)
}

#[test]
fn a_count_writes_the_session_and_reports_the_differences() {
    let store = stock_store();
    store.state().stock.balances.insert("A".into(), d("10"));
    store.state().stock.balances.insert("B".into(), d("4"));
    let c = count_cmd(7, 1, "2026-11-20", &counted("8", "4"));
    let r = run(&store, &c);
    let res: TxnStockCountResult = result_of(&r);
    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(res.aggregate, "stockcount:7");
    assert_eq!((res.lines, res.differences, res.net_qty), (2, 1, d("-2")));
    assert_eq!(
        (res.session_seq, res.voucher_no.as_deref()),
        (501, Some("CNT501"))
    );
    assert_eq!(
        res.hash,
        txn_aggregate_hash(vec![stock_count_hash_line("A", 1, d("-2"))])
    );

    let s = store.state();
    assert_eq!(s.stock.sessions.len(), 1);
    let session = &s.stock.sessions[0];
    assert_eq!(session.reference, "Cloud count 7");
    assert_eq!(
        (session.store_rvc, session.with_rvc, session.period),
        (10, 20, 202_611)
    );
    assert_eq!(
        (session.dept_code.as_str(), session.user.as_str()),
        ("D", "keeper")
    );
    let (version, hash, id, key) = s.stock.applied["stockcount:7"].clone();
    assert_eq!(
        (version, hash, id, key),
        (1, res.hash, c.command_id, Some(501))
    );
}

#[test]
fn a_count_without_differences_has_no_voucher() {
    let store = stock_store();
    let r = run(&store, &count_cmd(7, 1, "2026-11-20", &counted("0", "0")));
    let res: TxnStockCountResult = result_of(&r);
    assert_eq!((res.differences, res.voucher_no), (0, None));
    assert_eq!(res.hash, txn_aggregate_hash(Vec::new()));
}

#[test]
fn a_repeated_count_is_stale_and_carries_the_first_writes_facts() {
    let store = stock_store();
    store.state().stock.balances.insert("A".into(), d("10"));
    let first: TxnStockCountResult = result_of(&run(
        &store,
        &count_cmd(7, 1, "2026-11-20", &counted("8", "0")),
    ));
    // Another command id, other counted values, a later version: still the first session.
    let again: TxnStockCountResult = result_of(&run(
        &store,
        &count_cmd(7, 2, "2026-11-20", &counted("1", "1")),
    ));
    assert_eq!(again.outcome, txn_outcomes::STALE);
    assert_eq!(again.applied_version, 1);
    assert_eq!(
        (again.session_seq, again.hash.as_str()),
        (first.session_seq, first.hash.as_str())
    );
    assert_eq!(store.state().stock.sessions.len(), 1);
}

#[test]
fn a_repeat_whose_session_is_gone_or_belongs_to_another_store_is_refused() {
    let store = stock_store();
    run(&store, &count_cmd(7, 1, "2026-11-20", &counted("1", "0")));
    // The map moved to another BO store since the first write.
    store.state().stock.maps.get_mut(&1).unwrap().store_rvc = 11;
    refused(
        &run(&store, &count_cmd(7, 1, "2026-11-20", &counted("1", "0"))),
        reasons::COUNT_SESSION_MISMATCH,
    );
    store.state().stock.counts.clear();
    refused(
        &run(&store, &count_cmd(7, 1, "2026-11-20", &counted("1", "0"))),
        reasons::COUNT_SESSION_MISSING,
    );
}

#[test]
fn a_count_refusal_writes_nothing_and_leaves_no_log_row() {
    let cases: [(&str, &str, &str, Setup); 12] = [
        ("unmapped", "2026-11-20", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.maps.remove(&1);
        }),
        ("database", "2026-11-20", reasons::DATABASE_INVALID, |s| {
            s.state().stock.maps.get_mut(&1).unwrap().database = Some("Bad".into());
            s.state().stock.bad_databases.insert("Bad".into());
        }),
        ("ledger", "2026-11-20", reasons::LEDGER_MISSING, |s| {
            s.state().stock.ledger_ready = false;
        }),
        (
            "count objects",
            "2026-11-20",
            reasons::LEDGER_MISSING,
            |s| {
                s.state().stock.count_ready = false;
            },
        ),
        ("bo store", "2026-11-20", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.stores.remove(&20);
        }),
        (
            "no department",
            "2026-11-20",
            reasons::STORE_UNMAPPED,
            |s| {
                s.state().stock.maps.get_mut(&1).unwrap().dept_code = None;
            },
        ),
        ("target", "2026-11-20", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.count_target_ok = false;
        }),
        (
            "cost method",
            "2026-11-20",
            reasons::COST_METHOD_UNSUPPORTED,
            |s| {
                s.state().stock.settings.cost_method = Some(2);
            },
        ),
        ("closed", "2026-10-31", reasons::PERIOD_CLOSED, |_| {}),
        (
            "not open",
            "2026-12-01",
            reasons::COUNT_PERIOD_NOT_OPEN,
            |_| {},
        ),
        ("item", "2026-11-20", reasons::ITEM_UNKNOWN, |s| {
            s.state().stock.items.remove("B");
        }),
        ("gl closed", "2026-11-20", reasons::PERIOD_CLOSED, |s| {
            s.state().stock.settings.gl_period = Some(202_612);
        }),
    ];
    for (name, date, reason, setup) in cases {
        let store = stock_store();
        setup(&store);
        let c = count_cmd(7, 1, date, &counted("1", "1"));
        refused(&run(&store, &c), reason);
        assert!(!store.logged(c.command_id), "{name}: logged");
        let s = store.state();
        assert!(
            s.stock.applied.is_empty() && s.stock.sessions.is_empty(),
            "{name}"
        );
    }
}

#[test]
fn a_count_with_a_bad_payload_or_key_is_an_invalid_payload() {
    let store = stock_store();
    invalid(
        &run(&store, &count_cmd(7, 1, "2026-11-20", "[]")),
        "Lines must not be empty.",
    );
    let mut c = count_cmd(7, 1, "2026-11-20", &counted("1", "1"));
    c.aggregate_key = "txn:stockcount:8".into();
    invalid(&run(&store, &c), "AggregateKey does not match the count");
    let c = command(txn_stock_count_types::STOCK_COUNT, "", "3");
    invalid(&run(&store, &c), "payload is not a txn.stock_count object");
    assert_eq!(store.work_runs(), 0);
}

// ---- receipt

fn receipt_cmd(receipt_no: i64, vendor: i64, date: &str, lines: &str) -> SyncCommand {
    command(
        txn_stock_receipt_types::STOCK_RECEIPT,
        &format!("txn:stockreceipt:{receipt_no}"),
        &format!(
            r#"{{"rvc":1,"receipt_no":{receipt_no},"version":1,"business_date":"{date}","store_code":1,"vendor_code":{vendor},"lines":{lines},"actor":"keeper"}}"#
        ),
    )
}

fn received() -> String {
    r#"[{"item_code":"A","qty":2,"unit_price":10.5},{"item_code":"B","qty":1.5,"unit_price":4}]"#
        .into()
}

#[test]
fn a_receipt_is_written_with_its_amount_and_hash() {
    let store = stock_store();
    let c = receipt_cmd(5, 77, "2026-11-20", &received());
    let res: TxnStockReceiptResult = result_of(&run(&store, &c));
    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(res.aggregate, "stockreceipt:5");
    assert_eq!((res.receipt_seq, res.voucher_no.as_str()), (501, "PNK501"));
    assert_eq!((res.lines, res.net_qty, res.amount), (2, d("3.5"), d("27")));
    assert_eq!(
        res.hash,
        txn_aggregate_hash(vec![
            stock_receipt_hash_line("A", 1, d("2"), d("21")),
            stock_receipt_hash_line("B", 1, d("1.5"), d("6")),
        ])
    );
    let s = store.state();
    let w = &s.stock.receipts_written[0];
    assert_eq!((w.store_rvc, w.vendor_code, w.period), (10, 77, 202_611));
    assert_eq!(
        (w.reference.as_str(), w.user.as_str()),
        ("Cloud receipt 5", "keeper")
    );
    assert_eq!(s.stock.applied["stockreceipt:5"].3, Some(501));
}

#[test]
fn a_repeated_receipt_is_stale_and_a_foreign_vendor_or_missing_header_is_refused() {
    let store = stock_store();
    let first: TxnStockReceiptResult =
        result_of(&run(&store, &receipt_cmd(5, 77, "2026-11-20", &received())));
    let again: TxnStockReceiptResult =
        result_of(&run(&store, &receipt_cmd(5, 77, "2026-11-20", &received())));
    assert_eq!(again.outcome, txn_outcomes::STALE);
    assert_eq!(
        (again.receipt_seq, again.hash.as_str()),
        (first.receipt_seq, first.hash.as_str())
    );
    assert_eq!(store.state().stock.receipts_written.len(), 1);

    refused(
        &run(&store, &receipt_cmd(5, 78, "2026-11-20", &received())),
        reasons::RECEIPT_MISMATCH,
    );
    store.state().stock.receipts.clear();
    refused(
        &run(&store, &receipt_cmd(5, 77, "2026-11-20", &received())),
        reasons::RECEIPT_MISSING,
    );
}

#[test]
fn a_receipt_refusal_writes_nothing_and_leaves_no_log_row() {
    let cases: [(&str, &str, &str, Setup); 11] = [
        ("unmapped", "2026-11-20", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.maps.remove(&1);
        }),
        ("database", "2026-11-20", reasons::DATABASE_INVALID, |s| {
            s.state().stock.maps.get_mut(&1).unwrap().database = Some("Bad".into());
            s.state().stock.bad_databases.insert("Bad".into());
        }),
        ("ledger", "2026-11-20", reasons::LEDGER_MISSING, |s| {
            s.state().stock.ledger_ready = false;
        }),
        (
            "receipt objects",
            "2026-11-20",
            reasons::LEDGER_MISSING,
            |s| {
                s.state().stock.receipt_ledger.ready = false;
            },
        ),
        (
            "voucher type",
            "2026-11-20",
            reasons::VOUCHER_TYPE_UNKNOWN,
            |s| {
                s.state().stock.receipt_ledger.voucher_type = None;
            },
        ),
        ("bo store", "2026-11-20", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.stores.remove(&10);
        }),
        (
            "cost method",
            "2026-11-20",
            reasons::COST_METHOD_UNSUPPORTED,
            |s| {
                s.state().stock.settings.cost_method = Some(1);
            },
        ),
        ("closed", "2026-10-31", reasons::PERIOD_CLOSED, |_| {}),
        (
            "not open",
            "2026-12-01",
            reasons::RECEIPT_PERIOD_NOT_OPEN,
            |_| {},
        ),
        ("item", "2026-11-20", reasons::ITEM_UNKNOWN, |s| {
            s.state().stock.items.remove("A");
        }),
        (
            "target",
            "2026-11-20",
            reasons::RECEIPT_TARGET_INVALID,
            |s| {
                s.state().stock.receipt_target_ok = false;
            },
        ),
    ];
    for (name, date, reason, setup) in cases {
        let store = stock_store();
        setup(&store);
        let c = receipt_cmd(5, 77, date, &received());
        refused(&run(&store, &c), reason);
        assert!(!store.logged(c.command_id), "{name}: logged");
        let s = store.state();
        assert!(
            s.stock.applied.is_empty() && s.stock.receipts_written.is_empty(),
            "{name}"
        );
    }
}

#[test]
fn a_receipt_with_a_bad_payload_or_key_is_an_invalid_payload() {
    let store = stock_store();
    invalid(
        &run(&store, &receipt_cmd(5, 77, "2026-11-20", "[]")),
        "Lines must not be empty.",
    );
    let mut c = receipt_cmd(5, 77, "2026-11-20", &received());
    c.aggregate_key = "txn:stockreceipt:6".into();
    invalid(&run(&store, &c), "AggregateKey does not match the receipt");
    let c = command(txn_stock_receipt_types::STOCK_RECEIPT, "", "null");
    invalid(
        &run(&store, &c),
        "payload is not a txn.stock_receipt object",
    );
    assert_eq!(store.work_runs(), 0);
}

// ---- issue

fn issue_cmd(
    issue_no: i64,
    version: i64,
    with: Option<i32>,
    lines: &str,
    reason: &str,
) -> SyncCommand {
    let with = with.map_or(String::new(), |w| format!(r#""with_store_code":{w},"#));
    command(
        txn_stock_issue_types::STOCK_ISSUE,
        &format!("txn:stockissue:{issue_no}"),
        &format!(
            r#"{{"rvc":1,"issue_no":{issue_no},"version":{version},"business_date":"2026-11-20","store_code":1,{with}"lines":{lines},"reason":"{reason}","actor":"keeper"}}"#
        ),
    )
}

fn issued() -> String {
    r#"[{"item_code":"A","qty":2},{"item_code":"B","qty":0.5}]"#.into()
}

#[test]
fn an_issue_to_the_consumption_location_posts_a_200_50_voucher() {
    let store = stock_store();
    let c = issue_cmd(12, 1, None, &issued(), "broken");
    let res: TxnStockIssueResult = result_of(&run(&store, &c));
    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(res.aggregate, "stockissue:12");
    assert_eq!(
        (res.voucher_no.as_str(), res.lines, res.net_qty),
        ("SX501", 2, d("-2.5"))
    );
    assert_eq!(
        res.hash,
        txn_aggregate_hash(vec![
            stock_issue_hash_line("A", 1, None, d("2")),
            stock_issue_hash_line("B", 1, None, d("0.5")),
        ])
    );
    let s = store.state();
    let v = &s.stock.vouchers[0];
    assert_eq!(v.trn_sub, 50);
    assert_eq!((v.store_rvc, v.with_rvc), (10, 20));
    assert_eq!(v.reference, "Cloud issue 12");
    assert_eq!(v.description.as_deref(), Some("Cloud issue 12 - broken"));
    let moves: Vec<_> = v
        .lines
        .iter()
        .map(|l| (l.item_code.as_str(), l.trn_qty))
        .collect();
    assert_eq!(moves, [("A", d("-2")), ("B", d("-0.5"))]);
    assert_eq!(s.stock.applied["stockissue:12"].3, Some(501));
}

#[test]
fn an_issue_without_a_note_is_described_by_its_reference() {
    let store = stock_store();
    run(&store, &issue_cmd(12, 1, None, &issued(), "  "));
    let s = store.state();
    assert_eq!(
        s.stock.vouchers[0].description.as_deref(),
        Some("Cloud issue 12")
    );
}

#[test]
fn an_issue_to_another_stock_store_is_a_transfer() {
    let store = stock_store();
    let c = issue_cmd(12, 1, Some(2), &issued(), "");
    let res: TxnStockIssueResult = result_of(&run(&store, &c));
    assert_eq!(
        res.hash,
        txn_aggregate_hash(vec![
            stock_issue_hash_line("A", 1, Some(2), d("2")),
            stock_issue_hash_line("B", 1, Some(2), d("0.5")),
        ])
    );
    let s = store.state();
    assert_eq!(
        (s.stock.vouchers[0].store_rvc, s.stock.vouchers[0].with_rvc),
        (10, 11)
    );
}

#[test]
fn a_repeated_issue_is_stale_and_a_changed_payload_at_the_same_version_is_refused() {
    let store = stock_store();
    let first: TxnStockIssueResult =
        result_of(&run(&store, &issue_cmd(12, 1, None, &issued(), "")));
    let again: TxnStockIssueResult =
        result_of(&run(&store, &issue_cmd(12, 1, None, &issued(), "")));
    assert_eq!(again.outcome, txn_outcomes::STALE);
    assert_eq!(
        (again.voucher_no.as_str(), again.hash.as_str()),
        (first.voucher_no.as_str(), first.hash.as_str())
    );
    assert_eq!(store.state().stock.vouchers.len(), 1);

    // A newer version is stale whatever it carries (one-shot).
    let newer: TxnStockIssueResult = result_of(&run(
        &store,
        &issue_cmd(12, 2, None, r#"[{"item_code":"A","qty":9}]"#, ""),
    ));
    assert_eq!(
        (newer.outcome.as_str(), newer.applied_version),
        (txn_outcomes::STALE, 1)
    );
    // The same version with other lines or another destination is not a repeat.
    refused(
        &run(
            &store,
            &issue_cmd(
                12,
                1,
                None,
                r#"[{"item_code":"A","qty":9},{"item_code":"B","qty":0.5}]"#,
                "",
            ),
        ),
        reasons::ISSUE_PAYLOAD_MISMATCH,
    );
    refused(
        &run(&store, &issue_cmd(12, 1, Some(2), &issued(), "")),
        reasons::ISSUE_PAYLOAD_MISMATCH,
    );
    assert_eq!(store.state().stock.vouchers.len(), 1);
}

#[test]
fn a_repeat_whose_voucher_is_gone_or_is_not_this_issues_is_refused() {
    let store = stock_store();
    run(&store, &issue_cmd(12, 1, None, &issued(), ""));
    let kept: StockIssuePosted = store.state().stock.issues[&501].clone();
    store.state().stock.issues.get_mut(&501).unwrap().reference = Some("Cloud issue 99".into());
    refused(
        &run(&store, &issue_cmd(12, 1, None, &issued(), "")),
        reasons::ISSUE_VOUCHER_MISMATCH,
    );
    let mut moved = kept.clone();
    moved.lines.iter_mut().for_each(|l| l.rvc = 11);
    store.state().stock.issues.insert(501, moved);
    refused(
        &run(&store, &issue_cmd(12, 1, None, &issued(), "")),
        reasons::ISSUE_VOUCHER_MISMATCH,
    );
    store.state().stock.issues.clear();
    refused(
        &run(&store, &issue_cmd(12, 1, None, &issued(), "")),
        reasons::ISSUE_VOUCHER_MISSING,
    );
}

#[test]
fn an_issue_refusal_writes_nothing_and_leaves_no_log_row() {
    let cases: [(&str, Option<i32>, &str, Setup); 12] = [
        ("unmapped", None, reasons::STORE_UNMAPPED, |s| {
            s.state().stock.maps.remove(&1);
        }),
        ("database", None, reasons::DATABASE_INVALID, |s| {
            s.state().stock.maps.get_mut(&1).unwrap().database = Some("Bad".into());
            s.state().stock.bad_databases.insert("Bad".into());
        }),
        ("ledger", None, reasons::LEDGER_MISSING, |s| {
            s.state().stock.ledger_ready = false;
        }),
        ("destination map", Some(5), reasons::STORE_UNMAPPED, |_| {}),
        (
            "destination database",
            Some(2),
            reasons::DATABASE_INVALID,
            |s| {
                s.state().stock.maps.get_mut(&2).unwrap().database = Some("Bad".into());
                s.state().stock.bad_databases.insert("Bad".into());
            },
        ),
        ("other database", Some(2), reasons::STORE_UNMAPPED, |s| {
            s.state().stock.maps.get_mut(&2).unwrap().database = Some("SMILE_BO2".into());
        }),
        ("same bo store", Some(3), reasons::STORE_UNMAPPED, |_| {}),
        ("source type", None, reasons::STORE_UNMAPPED, |s| {
            s.state().stock.stores.insert(10, 1);
        }),
        ("destination type", None, reasons::STORE_UNMAPPED, |s| {
            s.state().stock.stores.insert(20, 0);
        }),
        (
            "transfer destination type",
            Some(2),
            reasons::STORE_UNMAPPED,
            |s| {
                s.state().stock.stores.insert(11, 1);
            },
        ),
        ("closed", None, reasons::PERIOD_CLOSED, |s| {
            s.state().stock.settings.inv_period = Some(202_612);
        }),
        ("item", None, reasons::ITEM_UNKNOWN, |s| {
            s.state().stock.items.remove("A");
        }),
    ];
    for (name, with, reason, setup) in cases {
        let store = stock_store();
        setup(&store);
        let c = issue_cmd(12, 1, with, &issued(), "");
        refused(&run(&store, &c), reason);
        assert!(!store.logged(c.command_id), "{name}: logged");
        let s = store.state();
        assert!(
            s.stock.applied.is_empty() && s.stock.vouchers.is_empty(),
            "{name}"
        );
    }
}

#[test]
fn an_issue_with_the_same_destination_as_the_source_store_is_a_consumption_issue() {
    let store = stock_store();
    // WithStoreCode = StoreCode is the consumption location, no other map is read.
    let res: TxnStockIssueResult =
        result_of(&run(&store, &issue_cmd(12, 1, Some(1), &issued(), "")));
    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(store.state().stock.vouchers[0].with_rvc, 20);
}

#[test]
fn a_vendor_post_that_posts_nothing_is_an_error_not_a_refusal() {
    let store = stock_store();
    store.state().stock.issue_post_empty = true;
    let c = issue_cmd(12, 1, None, &issued(), "");
    let e = commands_support::stock_cmd::executor(&store);
    let err = e.execute(&c, &dl_agent_pda_sync::util::ctx::Ctx::background());
    let message = err.expect_err("rolled back").to_string();
    assert!(
        message.contains("was not found with its 2 200/50 line(s)"),
        "{message}"
    );
    assert!(!store.logged(c.command_id));
    let s = store.state();
    assert!(s.stock.applied.is_empty() && s.stock.vouchers.is_empty());
}

#[test]
fn an_issue_with_a_bad_payload_or_key_is_an_invalid_payload() {
    let store = stock_store();
    invalid(
        &run(&store, &issue_cmd(12, 1, None, "[]", "")),
        "Lines must not be empty.",
    );
    let mut c = issue_cmd(12, 1, None, &issued(), "");
    c.aggregate_key = "txn:stockissue:13".into();
    invalid(
        &run(&store, &c),
        "AggregateKey does not match the issue voucher",
    );
    let c = command(txn_stock_issue_types::STOCK_ISSUE, "", "[]");
    invalid(&run(&store, &c), "payload is not a txn.stock_issue object");
    assert_eq!(store.work_runs(), 0);
}

#[test]
fn the_hash_lines_match_the_dotnet_text() {
    assert_eq!(stock_count_hash_line("A", 1, d("-2.50000")), "A|1|-2.5\n");
    assert_eq!(
        stock_receipt_hash_line("A", 1, d("2.0"), d("21.50")),
        "A|1|2|21.5\n"
    );
    assert_eq!(stock_issue_hash_line("A", 1, None, d("2")), "A|1|1|-2\n");
    assert_eq!(
        stock_issue_hash_line("A", 1, Some(2), d("0.5")),
        "A|1|2|-0.5\n"
    );
}
