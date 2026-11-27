//! B9: the table and multi-check verbs (OW1e, ADR 0112) live on SQL Server as the
//! `pda_sync` login of the real bootstrap (`090` grants included), on the SMILE_POS
//! fixture of `live_support` (port of `Commands/SqlTableCommandsIntegrationTests.cs`). The
//! checks are opened, posted and sent by the order verbs themselves; POS WIN's payments
//! and discounts are admin writes. Each command is delivered twice: one write, one
//! `Pda_SyncCommandLog` row, the same answer; a paid, discounted or changed check is a
//! logged refusal that writes nothing.
//!
//! Gated on `PDA_SYNC_TEST_SQL`; run with `--test-threads=1`.

mod live_support;

use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, OrderCheckExpectation, OrderCheckMergePayload, OrderCheckOpenPayload,
    OrderItemPostPayload, OrderItemsMovePayload, OrderSendPayload, OrderTableChangePayload,
    OrderTableTransferPayload, SyncCommand, check_aggregate, multi_check_aggregate,
    order_aggregate, order_command_reasons as order, order_command_types as o,
    site_command_reasons, table_command_reasons as table, table_command_types as tt,
};
use dl_tds::Param;
use live_support::orders::{CLERK, Live, command, dec};

const RVC: i32 = 3;

/// An open check on `table` with two guests and one sent item line per price.
fn seed(t: &Live, table: &str, prices: &[&str]) -> (i64, Vec<i64>) {
    let open = OrderCheckOpenPayload {
        rvc_no: RVC,
        table_no: table.to_owned(),
        num_guest: 2,
        cashier: CLERK.to_owned(),
    };
    let check = t
        .applied(&command(
            o::CHECK_OPEN,
            open.to_element(),
            &order_aggregate::open(RVC, dl_uuid::Uuid::new_v4()),
            None,
        ))
        .check_no;
    let key = check_aggregate::key(RVC, check);
    let mut seqs = Vec::new();
    for price in prices {
        let p = OrderItemPostPayload {
            rvc_no: RVC,
            check_no: check,
            cashier: CLERK.to_owned(),
            item_code: "C".to_owned(),
            qty: dec("1"),
            seat_no: 0,
            price: dec(price),
            note: None,
            open_name: None,
            allow_zero_open_price: false,
        };
        let posted = t.applied(&command(o::ITEM_POST, p.to_element(), &key, None));
        seqs.push(posted.trn_seq.unwrap());
    }
    let send = OrderSendPayload {
        rvc_no: RVC,
        check_no: check,
        table_no: None,
        cashier: CLERK.to_owned(),
        pda_handled: None,
    };
    t.applied(&command(o::SEND, send.to_element(), &key, None));
    (check, seqs)
}

fn expect(t: &Live, check: i64) -> OrderCheckExpectation {
    OrderCheckExpectation {
        check_no: check,
        expected: t.seen(check),
    }
}

fn table_change(t: &Live, check: i64, table: &str, guests: i32) -> SyncCommand {
    let p = OrderTableChangePayload {
        rvc_no: RVC,
        check_no: check,
        table_no: table.to_owned(),
        num_guest: guests,
        cashier: CLERK.to_owned(),
    };
    command(
        tt::TABLE_CHANGE,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        Some(&t.seen(check)),
    )
}

fn transfer(t: &Live, check: i64, table: &str) -> SyncCommand {
    let p = OrderTableTransferPayload {
        rvc_no: RVC,
        check_no: check,
        to_table_no: table.to_owned(),
        cashier: CLERK.to_owned(),
    };
    command(
        tt::TABLE_TRANSFER,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        Some(&t.seen(check)),
    )
}

fn merge(target: OrderCheckExpectation, source: OrderCheckExpectation) -> SyncCommand {
    let (t, s) = (target.check_no, source.check_no);
    let p = OrderCheckMergePayload {
        rvc_no: RVC,
        target_check_no: t,
        source_check_no: s,
        cashier: CLERK.to_owned(),
        checks: vec![target, source],
    };
    command(
        tt::CHECK_MERGE,
        p.to_element(),
        &multi_check_aggregate::keys(RVC, t, s).0,
        None,
    )
}

fn move_lines(t: &Live, from: i64, to: i64, seqs: &[i64]) -> SyncCommand {
    let p = OrderItemsMovePayload {
        rvc_no: RVC,
        from_check_no: from,
        to_check_no: to,
        trn_seqs: seqs.to_vec(),
        cashier: CLERK.to_owned(),
        checks: vec![expect(t, from), expect(t, to)],
    };
    command(
        tt::ITEMS_MOVE,
        p.to_element(),
        &multi_check_aggregate::keys(RVC, from, to).0,
        None,
    )
}

fn lines_of(t: &Live, check: i64) -> Vec<String> {
    t.rows(
        "SELECT CAST(TrnSeq AS bigint) FROM dbo.tmpTrn WHERE CheckNo = @c ORDER BY TrnSeq",
        &[Param::new("c", check)],
    )
}

fn info_of(t: &Live, check: i64) -> Vec<String> {
    t.rows(
        "SELECT CAST(TrnNo AS bigint) FROM dbo.tmpTrnInfo WHERE CheckNo = @c ORDER BY TrnNo",
        &[Param::new("c", check)],
    )
}

fn texts(seqs: &[i64]) -> Vec<String> {
    let mut v: Vec<i64> = seqs.to_vec();
    v.sort_unstable();
    v.iter().map(i64::to_string).collect()
}

#[test]
fn table_change_and_transfer_apply_exactly_once() {
    let Some(t) = Live::start() else { return };
    let c = |n: i64| [Param::new("c", n)];
    let (check, _) = seed(&t, "T4", &["50000"]);

    let changed = t.applied(&table_change(&t, check, "T12", 5));
    assert_eq!(
        (changed.table_no.as_deref(), changed.guests),
        (Some("T12"), 5)
    );
    assert_eq!(changed.fingerprint_after, t.seen(check));
    assert_eq!(
        t.one(
            "SELECT TableNo, NGuest, NChild FROM dbo.tmpInvoice WHERE CheckNo = @c",
            &c(check)
        ),
        "T12|5|0"
    );
    assert_eq!(
        t.one(
            "SELECT TableStatus FROM dbo.TableList WHERE TableNum = 'T12' AND RVC = 3",
            &[]
        ),
        "1",
        "the new table is marked busy"
    );

    let moved = t.applied(&transfer(&t, check, "  T7 "));
    assert_eq!(moved.table_no.as_deref().map(str::trim), Some("T7"));
    assert_eq!(moved.fingerprint_after, t.seen(check));
    assert_eq!(
        t.one(
            "SELECT TableNo FROM dbo.tmpInvoice WHERE CheckNo = @c",
            &c(check)
        ),
        "T7"
    );
    assert_eq!(
        t.rows(
            "SELECT TableNo FROM dbo.tmpTrnInfo WHERE CheckNo = @c",
            &c(check)
        ),
        ["T7"]
    );
    t.refused(&transfer(&t, check, "t7"), table::SAME_TABLE);

    // A check POS WIN settled is not moved.
    t.admin(
        "UPDATE dbo.tmpInvoice SET Status = 2 WHERE CheckNo = @c",
        &c(check),
    );
    t.refused(&table_change(&t, check, "T5", 2), order::BILL_NOT_OPEN);
    t.refused(&transfer(&t, check, "T5"), order::BILL_NOT_OPEN);
    assert_eq!(
        t.one(
            "SELECT TableNo, NGuest FROM dbo.tmpInvoice WHERE CheckNo = @c",
            &c(check)
        ),
        "T7|5"
    );
}

#[test]
fn merge_and_move_apply_exactly_once_like_pos_win() {
    let Some(t) = Live::start() else { return };
    let c = |n: i64| [Param::new("c", n)];

    // Merge: every line and its info row to the target; the source archived and closed.
    let (source, source_seqs) = seed(&t, "T4", &["30000"]);
    let (target, target_seqs) = seed(&t, "T5", &["50000", "20000"]);
    let merged = t.applied_multi(&merge(expect(&t, target), expect(&t, source)));
    assert_eq!(merged.removed, [source]);
    assert_eq!(merged.checks.len(), 1);
    assert_eq!(merged.checks[0].fingerprint_after, t.seen(target));
    assert_eq!(merged.checks[0].sub_total, dec("100000"));
    let all: Vec<i64> = target_seqs.iter().chain(&source_seqs).copied().collect();
    assert_eq!(lines_of(&t, target), texts(&all));
    assert_eq!(info_of(&t, target), texts(&all));
    assert!(lines_of(&t, source).is_empty());
    assert_eq!(
        t.one(
            "SELECT (SELECT COUNT(*) FROM dbo.tmpInvoice WHERE CheckNo = @c) + \
             (SELECT COUNT(*) FROM dbo.tmpInvoiceInfo WHERE CheckNo = @c)",
            &c(source)
        ),
        "0"
    );
    assert_eq!(
        t.rows(
            "SELECT PaymentMode, CloseBy, TableNo FROM dbo.InvoiceCxl WHERE CheckNo = @c",
            &c(source)
        ),
        ["Z_CANCEL|clerk-7|T4"]
    );
    assert_eq!(
        t.one(
            "SELECT isOpen, CASE WHEN CloseTime IS NULL THEN 0 ELSE 1 END FROM dbo.checkopenlog WHERE CheckNo = @c",
            &c(source)
        ),
        "0|1"
    );

    // Move: the chosen lines (deduplicated) with their info rows, one audit row per line.
    let (to, to_seqs) = seed(&t, "T5", &["20000"]);
    let (from, from_seqs) = seed(&t, "T4", &["10000", "30000", "40000"]);
    let (kept, a, b) = (from_seqs[0], from_seqs[1], from_seqs[2]);
    let moved = t.applied_multi(&move_lines(&t, from, to, &[b, a, a]));
    assert!(moved.removed.is_empty());
    assert_eq!(
        moved.check(from).map(|r| r.fingerprint_after.clone()),
        Some(t.seen(from))
    );
    assert_eq!(
        moved.check(to).map(|r| r.fingerprint_after.clone()),
        Some(t.seen(to))
    );
    assert_eq!(lines_of(&t, from), texts(&[kept]));
    assert_eq!(info_of(&t, from), texts(&[kept]));
    assert_eq!(lines_of(&t, to), texts(&[to_seqs[0], a, b]));
    assert_eq!(info_of(&t, to), texts(&[to_seqs[0], a, b]));
    assert_eq!(
        t.rows(
            "SELECT CAST(CheckNo AS bigint), CAST(ToCheckNo AS bigint), CAST(TrnNo AS bigint), ActCode, ActName, \
             CAST(TrnAmount AS int), Cashier FROM dbo.TrnActivities WHERE ToCheckNo = @c ORDER BY TrnNo",
            &c(to)
        ),
        [
            format!("{from}|{to}|{a}|3|MOVE|30000|{CLERK}"),
            format!("{from}|{to}|{b}|3|MOVE|40000|{CLERK}"),
        ],
        "the redelivery wrote no second audit"
    );
}

#[test]
fn paid_discounted_or_changed_checks_are_logged_refusals_that_write_nothing() {
    let Some(t) = Live::start() else { return };
    let (target, _) = seed(&t, "T5", &["20000"]);
    let (tendered, _) = seed(&t, "T4", &["30000"]);
    let (paid, paid_seqs) = seed(&t, "T6", &["30000"]);
    let (discounted, discounted_seqs) = seed(&t, "T8", &["30000"]);
    let (plain, _) = seed(&t, "T9", &["30000"]);
    // POS WIN takes cash on one, a deposit on another, and a discount on a third.
    t.admin(
        "INSERT dbo.tmpTrn (CheckNo, TrnSeq, RVC, TrnCode, ItemCode, ItemPrice, TrnQty) VALUES (@c, @c * 10 + 1, 3, 200, 'CASH', 30000, 1)",
        &[Param::new("c", tendered)],
    );
    t.admin(
        "UPDATE dbo.tmpInvoice SET TotalPaid = 10000 WHERE CheckNo = @c",
        &[Param::new("c", paid)],
    );
    t.admin(
        "INSERT dbo.tmpTrn (CheckNo, TrnSeq, RVC, TrnCode, ItemCode, ItemPrice, TrnQty) VALUES (@c, @c * 10 + 1, 3, 150, 'DSC10', 3000, 1)",
        &[Param::new("c", discounted)],
    );
    let all = [target, tendered, paid, discounted, plain];
    let state = || -> Vec<String> {
        all.iter()
            .flat_map(|&n| {
                let mut v = lines_of(&t, n);
                v.extend(info_of(&t, n));
                v.push(t.one(
                    "SELECT TableNo, Status, TotalPaid FROM dbo.tmpInvoice WHERE CheckNo = @c",
                    &[Param::new("c", n)],
                ));
                v
            })
            .collect()
    };
    let before = state();

    t.refused(
        &merge(expect(&t, target), expect(&t, tendered)),
        table::BILL_HAS_PAYMENT,
    );
    t.refused(
        &move_lines(&t, paid, target, &paid_seqs),
        table::BILL_HAS_PAYMENT,
    );
    t.refused(
        &move_lines(&t, discounted, target, &discounted_seqs),
        table::DISCOUNT_PRESENT,
    );
    let stale = OrderCheckExpectation {
        check_no: target,
        expected: "cs1:0000".to_owned(),
    };
    t.refused(
        &merge(stale, expect(&t, plain)),
        site_command_reasons::CHECK_CHANGED,
    );
    t.refused(
        &move_lines(&t, plain, target, &[999_999]),
        order::ITEM_NOT_FOUND,
    );
    // A move onto the source itself is refused before the store is opened: not journalled.
    let logged = t.logged();
    let onto_itself = t.run(&move_lines(&t, plain, plain, &[1]));
    assert_eq!(
        (onto_itself.status, onto_itself.reason.as_deref()),
        (CommandStatus::Rejected, Some(table::INVALID_MOVE_TARGET))
    );
    assert_eq!(t.logged(), logged);

    assert_eq!(state(), before);
    assert_eq!(
        t.one(
            "SELECT (SELECT COUNT(*) FROM dbo.TrnActivities) + (SELECT COUNT(*) FROM dbo.InvoiceCxl)",
            &[]
        ),
        "0"
    );
}
