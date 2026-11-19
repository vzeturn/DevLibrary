//! B9: the OW1b order verbs (ADR 0104) live on SQL Server as the `pda_sync` login the real
//! bootstrap `010`–`090` creates, on the SMILE_POS fixture of `live_support` (port of
//! `Commands/SqlOrderVerbsIntegrationTests.cs`). Every command goes through the production
//! executor and SQL store, is delivered twice, writes once, answers the same both times
//! and leaves one `Pda_SyncCommandLog` row; a check POS WIN closed, removed or changed
//! since the user saw it is refused without a write; a grant lost after bootstrap rolls
//! the business write back together with its journal row.
//!
//! Gated on `PDA_SYNC_TEST_SQL`; run with `--test-threads=1` (the `pda_sync` login is
//! server-wide).

mod live_support;

use dl_agent_pda_sync::bootstrap::read_script;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CheckStateFingerprint, CheckStateHeader, CheckStateLine, CommandStatus, OrderCheckOpenPayload,
    OrderCheckResult, OrderComboPostPayload, OrderCoversPayload, OrderItemPostPayload,
    OrderItemVoidPayload, OrderSendPayload, SyncCommand, check_aggregate, order_aggregate,
    order_command_reasons as order, order_command_types as o, site_command_reasons,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_tds::Param;
use live_support::batches;
use live_support::orders::{CLERK, Live, command, dec};

/// Above 21: `Rvc * 100000000` overflows int, so the batches must widen first.
const RVC: i32 = 25;
const RVC_BASE: i64 = RVC as i64 * 100_000_000;

fn open(table: &str, guests: i32) -> SyncCommand {
    let p = OrderCheckOpenPayload {
        rvc_no: RVC,
        table_no: table.to_owned(),
        num_guest: guests,
        cashier: CLERK.to_owned(),
    };
    command(
        o::CHECK_OPEN,
        p.to_element(),
        &order_aggregate::open(RVC, dl_uuid::Uuid::new_v4()),
        None,
    )
}

fn post(
    check: i64,
    expected: Option<&str>,
    item: &str,
    qty: &str,
    price: &str,
    seat: i32,
    note: Option<&str>,
) -> SyncCommand {
    let p = OrderItemPostPayload {
        rvc_no: RVC,
        check_no: check,
        cashier: CLERK.to_owned(),
        item_code: item.to_owned(),
        qty: dec(qty),
        seat_no: seat,
        price: dec(price),
        note: note.map(str::to_owned),
        open_name: None,
        allow_zero_open_price: false,
    };
    command(
        o::ITEM_POST,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        expected,
    )
}

fn send(check: i64, expected: Option<&str>) -> SyncCommand {
    let p = OrderSendPayload {
        rvc_no: RVC,
        check_no: check,
        table_no: None,
        cashier: CLERK.to_owned(),
    };
    command(
        o::SEND,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        expected,
    )
}

fn void_line(
    check: i64,
    expected: Option<&str>,
    seq: i64,
    sent: bool,
    reason: Option<&str>,
) -> SyncCommand {
    let p = OrderItemVoidPayload {
        rvc_no: RVC,
        check_no: check,
        trn_seq: seq,
        cashier: CLERK.to_owned(),
        sent,
        reason: reason.map(str::to_owned),
        author_cashier: None,
    };
    command(
        o::ITEM_VOID,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        expected,
    )
}

fn covers(check: i64, expected: Option<&str>, guests: i32) -> SyncCommand {
    let p = OrderCoversPayload {
        rvc_no: RVC,
        check_no: check,
        num_guest: guests,
        num_child: 0,
        cashier: CLERK.to_owned(),
    };
    command(
        o::COVERS,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        expected,
    )
}

fn combo(check: i64, expected: Option<&str>, code: &str) -> SyncCommand {
    let p = OrderComboPostPayload {
        rvc_no: RVC,
        check_no: check,
        cashier: CLERK.to_owned(),
        combo_code: code.to_owned(),
        qty: Decimal::ONE,
        seat_no: 0,
        price: Decimal::ZERO,
        note: None,
        selected_components: None,
    };
    command(
        o::COMBO_POST,
        p.to_element(),
        &check_aggregate::key(RVC, check),
        expected,
    )
}

/// Everything an order verb can write for `check` (header aside), as one value
/// (`WritesAsync` of the C# test).
fn writes(t: &Live, check: i64) -> String {
    t.one(
        "SELECT (SELECT ParaStr FROM dbo.Para WHERE RVC = @rvc AND ParaName = 'LastTrn'),
                (SELECT ParaStr FROM dbo.Para WHERE RVC = @rvc AND ParaName = 'LastCheck'),
                (SELECT COUNT(*) FROM dbo.tmpTrn WHERE CheckNo = @c),
                (SELECT CAST(SUM(TrnQty) AS decimal(18, 2)) FROM dbo.tmpTrn WHERE CheckNo = @c),
                (SELECT COUNT(*) FROM dbo.tmpTrnInfo WHERE CheckNo = @c),
                (SELECT COUNT(*) FROM dbo.tmpTrnInfo WHERE CheckNo = @c AND ISNULL(Void, 0) = 1),
                (SELECT COUNT(*) FROM dbo.VoidTrn),
                (SELECT COUNT(*) FROM dbo.CGTrn),
                (SELECT COUNT(*) FROM dbo.PDASendOrder WHERE CheckNo = @c),
                (SELECT COUNT(*) FROM dbo.checkopenlog WHERE CheckNo = @c),
                (SELECT COUNT(*) FROM dbo.Pda_SyncCommandLog)",
        &[Param::new("rvc", RVC), Param::new("c", check)],
    )
}

fn seqs(r: &OrderCheckResult) -> Vec<i64> {
    r.lines.iter().map(|l| l.line_seq).collect()
}

#[test]
fn open_post_send_and_both_voids_apply_exactly_once_as_pda_sync() {
    let Some(t) = Live::start() else { return };
    let c = |n: i64| [Param::new("c", n)];

    // order.check_open: the RVC-prefixed check number, header, info row and open log.
    let last_check = t.para(RVC, "LastCheck");
    let opened = t.applied(&open("T7", 3));
    let check = opened.check_no;
    assert_eq!(check, RVC_BASE + last_check + 1);
    assert_eq!(
        (
            opened.table_no.as_deref(),
            opened.guests,
            opened.lines.len()
        ),
        (Some("T7"), 3, 0)
    );
    assert_eq!(
        opened.fingerprint_after,
        CheckStateFingerprint::compute(
            &CheckStateHeader {
                check_no: check,
                table_no: Some("T7".into()),
                guests: 3,
                sub_total: Decimal::ZERO,
                total_paid: Decimal::ZERO,
            },
            &[]
        )
    );
    assert_eq!(opened.fingerprint_after, t.seen(check));
    assert_eq!(
        t.one(
            "SELECT IYear, IMonth, IDay, OpenBy, RVC, TableNo, NGuest, NChild, WSID, MealPeriod, Status, SubTotal, \
             TotalPaid, LastChgCashier, CASE WHEN OpenTime IS NULL OR LastChgTime IS NULL THEN 0 ELSE 1 END \
             FROM dbo.tmpInvoice WHERE CheckNo = @c",
            &c(check)
        ),
        "2026|11|14|clerk-7|25|T7|3|0|NTPDA|2|1|0|0|clerk-7|1"
    );
    assert_eq!(
        t.one(
            "SELECT CurPage, CurLine, Printed, Ordered, PrintType FROM dbo.tmpInvoiceInfo WHERE CheckNo = @c",
            &c(check)
        ),
        "1|0|0|0|0"
    );
    assert_eq!(
        t.one(
            "SELECT RVC, OpenBy, OpenPC, isOpen FROM dbo.checkopenlog WHERE CheckNo = @c",
            &c(check)
        ),
        "25|clerk-7|NTPDA|1"
    );
    assert_eq!(
        t.para(RVC, "LastCheck"),
        last_check + 1,
        "the redelivery opened no second check"
    );

    // order.item_post: a line with qty, seat, note and menu; then a package with its
    // components and balance line, each drawing its line numbers once.
    let last_trn = t.para(RVC, "LastTrn");
    let note = "it da, khong duong";
    let coffee = t.applied(&post(
        check,
        Some(&opened.fingerprint_after),
        "CF01",
        "2",
        "25000",
        1,
        Some(note),
    ));
    let coffee_seq = RVC_BASE + last_trn + 1;
    assert_eq!(coffee.trn_seq, Some(coffee_seq));
    let line = &coffee.lines[0];
    assert_eq!(coffee.lines.len(), 1);
    assert_eq!(
        (
            line.line_seq,
            line.item_code.as_deref(),
            line.qty,
            line.price,
            line.amount,
            line.seat_no,
            line.sent,
            line.note.as_deref()
        ),
        (
            coffee_seq,
            Some("CF01"),
            dec("2"),
            dec("25000"),
            dec("50000"),
            1,
            false,
            Some(note)
        )
    );
    assert_eq!(coffee.sub_total, dec("50000"));
    assert_eq!(
        t.one(
            "SELECT OrderNo, Rsv1, Rsv2 FROM dbo.tmpTrn WHERE TrnSeq = @c",
            &c(coffee_seq)
        ),
        "1||M01"
    );
    assert_eq!(
        t.one(
            "SELECT ItemCode, CGCode, CGName FROM dbo.CGTrn WHERE TrnNo = @c",
            &c(coffee_seq)
        ),
        format!("CF01|0|{note}")
    );
    let package = t.applied(&post(
        check,
        Some(&coffee.fingerprint_after),
        "CB01",
        "2",
        "60000",
        2,
        None,
    ));
    let pkg = coffee_seq + 1;
    assert_eq!(package.trn_seq, Some(pkg));
    assert_eq!(seqs(&package), [coffee_seq, pkg, pkg + 1, pkg + 2, pkg + 3]);
    assert_eq!(
        package.sub_total,
        package.lines.iter().map(|l| l.amount).sum::<Decimal>()
    );
    assert_eq!(
        t.para(RVC, "LastTrn"),
        last_trn + 5,
        "the redelivered package drew no line numbers"
    );
    assert_eq!(
        t.rows(
            "SELECT CAST(TrnSeq AS bigint), ItemCode, CAST(TrnQty AS decimal(18, 2)), ItemPrice, OrderNo, Rsv1 \
             FROM dbo.tmpTrn WHERE CheckNo = @c AND TrnSeq >= @s ORDER BY TrnSeq",
            &[Param::new("c", check), Param::new("s", pkg)]
        ),
        [
            format!("{pkg}|CB01|2|60000|2|"),
            format!("{}|CF01|2|0|2|{pkg}", pkg + 1),
            format!("{}|CK01|4|10000|2|{pkg}", pkg + 2),
            format!("{}|CB01|0|60000|2|{pkg}", pkg + 3),
        ]
    );

    // order.send: every unsent line flagged ordered once, one audit row.
    let sent = t.applied(&send(check, Some(&package.fingerprint_after)));
    assert_eq!(sent.sent_seqs.as_deref(), Some(seqs(&package).as_slice()));
    assert!(sent.lines.iter().all(|l| l.sent));
    assert_eq!(
        sent.fingerprint_after, package.fingerprint_after,
        "sending changes no fingerprinted column"
    );
    assert_eq!(
        t.rows(
            "SELECT RVC, SendBy FROM dbo.PDASendOrder WHERE CheckNo = @c",
            &c(check)
        ),
        ["25|clerk-7"]
    );
    assert_eq!(
        t.one(
            "SELECT COUNT(*) FROM dbo.tmpTrnInfo WHERE CheckNo = @c AND PToOrder = 1 AND POrderTime IS NOT NULL",
            &c(check)
        ),
        "5"
    );

    // order.item_void sent: archived once with its reason, the info row kept and flagged.
    let voided = t.applied(&void_line(
        check,
        Some(&sent.fingerprint_after),
        coffee_seq,
        true,
        Some("khach huy mon"),
    ));
    assert_eq!(seqs(&voided), [pkg, pkg + 1, pkg + 2, pkg + 3]);
    assert_eq!(
        t.rows(
            "SELECT CAST(TrnSeq AS bigint), ItemCode, CAST(TrnQty AS decimal(18, 2)), StsCode1, Rsv2 \
             FROM dbo.VoidTrn WHERE CheckNo = @c",
            &c(check)
        ),
        [format!("{coffee_seq}|CF01|2|khach huy mon|M01")]
    );
    assert_eq!(
        t.one(
            "SELECT PToOrder, Void FROM dbo.tmpTrnInfo WHERE TrnNo = @c",
            &c(coffee_seq)
        ),
        "1|1"
    );

    // The unsent removal never takes a line the kitchen has; it takes an unsent package
    // with its components and balance line, once.
    t.refused(
        &void_line(check, Some(&voided.fingerprint_after), pkg, false, None),
        order::ITEM_ALREADY_SENT,
    );
    let again = t.applied(&post(
        check,
        Some(&voided.fingerprint_after),
        "CB01",
        "1",
        "60000",
        0,
        Some("them da"),
    ));
    let pkg2 = again.trn_seq.unwrap();
    let removed = t.applied(&void_line(
        check,
        Some(&again.fingerprint_after),
        pkg2,
        false,
        Some("khach doi mon"),
    ));
    assert_eq!(seqs(&removed), seqs(&voided));
    assert_eq!(
        t.one(
            "SELECT COUNT(*) FROM dbo.VoidTrn WHERE CheckNo = @c AND TrnSeq >= @s",
            &[Param::new("c", check), Param::new("s", pkg2)]
        ),
        "4",
        "the package takes its components and balance line with it, once"
    );
    assert_eq!(
        t.one("SELECT COUNT(*) FROM dbo.CGTrn WHERE TrnNo = @c", &c(pkg2)),
        "0",
        "the note goes with the line"
    );
    assert_eq!(removed.fingerprint_after, t.seen(check));
}

#[test]
fn a_check_pos_win_closed_removed_or_changed_is_refused_without_a_write() {
    let Some(t) = Live::start() else { return };
    let c = |n: i64| [Param::new("c", n)];

    // POS WIN settles the bill, then moves it off the open table.
    let opened = t.applied(&open("T12", 2));
    let check = opened.check_no;
    let coffee = t.applied(&post(
        check,
        Some(&opened.fingerprint_after),
        "CF01",
        "1",
        "25000",
        0,
        None,
    ));
    let sent = t.applied(&send(check, Some(&coffee.fingerprint_after)));
    let seq = coffee.trn_seq.unwrap();
    t.admin(
        "UPDATE dbo.tmpInvoice SET Status = 2 WHERE CheckNo = @c",
        &c(check),
    );
    let before = writes(&t, check);
    t.refused(
        &void_line(check, Some(&sent.fingerprint_after), seq, true, Some("huy")),
        order::BILL_NOT_OPEN,
    );
    t.refused(
        &void_line(check, None, seq, false, None),
        order::BILL_NOT_OPEN,
    );
    t.refused(&covers(check, None, 4), order::BILL_NOT_OPEN);
    assert_eq!(
        writes(&t, check)
            .rsplit_once('|')
            .map(|(w, _)| w.to_owned()),
        before.rsplit_once('|').map(|(w, _)| w.to_owned()),
        "a closed check is not written (only the three refusals are journalled)"
    );
    t.admin("DELETE dbo.tmpInvoice WHERE CheckNo = @c", &c(check));
    let before = writes(&t, check);
    let refusals = [
        post(check, None, "CK01", "1", "10000", 0, None),
        post(
            check,
            Some(&sent.fingerprint_after),
            "CK01",
            "1",
            "10000",
            0,
            None,
        ),
        send(check, None),
        void_line(check, None, seq, true, Some("huy")),
        covers(check, None, 4),
        combo(check, None, "10"),
    ];
    for r in &refusals {
        t.refused(r, order::CHECK_NOT_FOUND);
    }
    let after = writes(&t, check);
    let (w0, l0) = before.rsplit_once('|').unwrap();
    let (w1, l1) = after.rsplit_once('|').unwrap();
    assert_eq!(w1, w0, "a missing check is never recreated");
    assert_eq!(
        l1.parse::<usize>().unwrap(),
        l0.parse::<usize>().unwrap() + refusals.len()
    );

    // A waiter edits the line on POS WIN: the verbs that carry what the user saw are
    // refused until the user sees the check again.
    let opened = t.applied(&open("T13", 2));
    let check = opened.check_no;
    let coffee = t.applied(&post(
        check,
        Some(&opened.fingerprint_after),
        "CF01",
        "1",
        "25000",
        0,
        None,
    ));
    let seq = coffee.trn_seq.unwrap();
    t.admin(
        "UPDATE dbo.tmpTrn SET TrnQty = 3 WHERE CheckNo = @c",
        &c(check),
    );
    let stale = Some(coffee.fingerprint_after.as_str());
    let before = writes(&t, check);
    let changed = [
        post(check, stale, "CK01", "1", "10000", 0, None),
        send(check, stale),
        void_line(check, stale, seq, false, None),
        covers(check, stale, 5),
        combo(check, stale, "CF01"),
    ];
    for r in &changed {
        t.refused(r, site_command_reasons::CHECK_CHANGED);
    }
    let after = writes(&t, check);
    assert_eq!(
        after.rsplit_once('|').unwrap().0,
        before.rsplit_once('|').unwrap().0,
        "nothing is written over a POS WIN change"
    );
    let seen = CheckStateFingerprint::compute(
        &CheckStateHeader {
            check_no: check,
            table_no: Some("T13".into()),
            guests: 2,
            sub_total: dec("25000"),
            total_paid: Decimal::ZERO,
        },
        &[CheckStateLine {
            line_seq: seq,
            transaction_code: Some("100".into()),
            item_code: Some("CF01".into()),
            qty: dec("3"),
            price: dec("25000"),
        }],
    );
    assert_eq!(seen, t.seen(check));
    let sent = t.applied(&send(check, Some(&seen)));
    assert_eq!(sent.sent_seqs, Some(vec![seq]));
    assert_eq!(sent.lines[0].qty, dec("3"));
    let covered = t.applied(&covers(check, Some(&sent.fingerprint_after), 5));
    assert_eq!(covered.guests, 5);
    assert_eq!(
        t.one(
            "SELECT NGuest, NChild, LastChgCashier, CASE WHEN LastChgTime IS NULL THEN 0 ELSE 1 END \
             FROM dbo.tmpInvoice WHERE CheckNo = @c",
            &c(check)
        ),
        "5|0|clerk-7|1"
    );
    assert_eq!(covered.fingerprint_after, t.seen(check));
}

#[test]
fn a_grant_lost_after_bootstrap_rolls_the_verb_back_with_its_journal_row_and_the_replay_applies_once()
 {
    let Some(t) = Live::start() else { return };
    let c = |n: i64| [Param::new("c", n)];
    let opened = t.applied(&open("T15", 2));
    let check = opened.check_no;
    let coffee = t.applied(&post(
        check,
        Some(&opened.fingerprint_after),
        "CF01",
        "1",
        "25000",
        0,
        None,
    ));
    let sent = t.applied(&send(check, Some(&coffee.fingerprint_after)));
    let voided = void_line(
        check,
        Some(&sent.fingerprint_after),
        coffee.trn_seq.unwrap(),
        true,
        Some("khach huy mon"),
    );

    // A business write denied halfway: the batch's earlier statements roll back.
    let before = writes(&t, check);
    t.admin("REVOKE INSERT ON OBJECT::dbo.VoidTrn FROM [pda_sync];", &[]);
    let refused = t.run(&voided);
    assert_eq!(
        (refused.status, refused.reason.as_deref()),
        (CommandStatus::Rejected, Some(order::SQL_GRANTS_MISSING))
    );
    assert_eq!(
        writes(&t, check),
        before,
        "nothing written, nothing journalled"
    );
    assert_eq!(
        t.executor()
            .find_recorded(voided.command_id, &Ctx::background())
            .unwrap(),
        None,
        "a grants refusal is not logged"
    );
    for b in batches(read_script("Sql.070_order_grants.sql").unwrap()) {
        t.admin(b, &[]);
    }
    let replayed = t.applied(&voided);
    assert!(replayed.lines.is_empty());
    assert_eq!(
        t.one(
            "SELECT COUNT(*) FROM dbo.VoidTrn WHERE CheckNo = @c",
            &c(check)
        ),
        "1"
    );

    // The journal row denied after the business write: the write rolls back with it, so
    // a redelivery cannot find the line posted without its log row.
    let tea = post(
        check,
        Some(&replayed.fingerprint_after),
        "CK01",
        "1",
        "10000",
        0,
        None,
    );
    let before = writes(&t, check);
    t.admin(
        "REVOKE INSERT ON OBJECT::dbo.Pda_SyncCommandLog FROM [pda_sync];",
        &[],
    );
    let refused = t.run(&tea);
    assert_eq!(
        (refused.status, refused.reason.as_deref()),
        (CommandStatus::Rejected, Some(order::SQL_GRANTS_MISSING))
    );
    assert_eq!(
        writes(&t, check),
        before,
        "the posted line and its line number roll back with the journal row"
    );
    t.admin(
        "GRANT INSERT ON OBJECT::dbo.Pda_SyncCommandLog TO [pda_sync];",
        &[],
    );
    let posted = t.applied(&tea);
    assert_eq!(posted.lines.len(), 1);
    assert_eq!(posted.fingerprint_after, t.seen(check));
}
