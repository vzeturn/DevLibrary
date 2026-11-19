//! B9: the combo verb (OW1d, ADR 0110), the KDS line states and the reservations live on
//! SQL Server as the `pda_sync` login of the real bootstrap, on the SMILE_POS fixture of
//! `live_support` with the `ComboStoreSchema.cs` shapes (port of
//! `Commands/SqlOrderComboIntegrationTests.cs` and the KDS / reservation cases of
//! `SqlSiteCommandStoreIntegrationTests.cs`). Each command is delivered twice: one write,
//! one `Pda_SyncCommandLog` row, the same answer.
//!
//! The legacy-batch parity half of the C# combo tests (the legacy text run first and
//! rolled back) is not ported: the batch text is pinned byte for byte to C#
//! (`commands/tests/sql.rs`), so these tests hold what the batch writes instead.
//!
//! Gated on `PDA_SYNC_TEST_SQL`; run with `--test-threads=1`.

mod live_support;

use dl_agent_pda_sync::commands::reasons;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, KdsLineStatePayload, KdsLineStateResult, OrderCheckOpenPayload,
    OrderComboPostPayload, OrderItemPostPayload, OrderSendPayload, ReservationCancelPayload,
    ReservationSavedResult, SyncCommand, check_aggregate, kds_line_actions as kds, order_aggregate,
    order_command_reasons as order, order_command_types as o, site_command_types as s,
};
use dl_agent_pda_sync::util::stj;
use dl_decimal::Decimal;
use dl_tds::Param;
use live_support::orders::{CLERK, Live, command, dec};

/// `ComboStoreSchema.Rvc`: GroupChkTrn on, a new order round per post.
const RVC: i32 = 3;
/// `ComboStoreSchema.RvcByCourse`: OrderByCourse, so a combo shares the round of the bill.
const RVC_BY_COURSE: i32 = 4;

fn open(rvc: i32) -> SyncCommand {
    let p = OrderCheckOpenPayload {
        rvc_no: rvc,
        table_no: "T4".to_owned(),
        num_guest: 2,
        cashier: CLERK.to_owned(),
    };
    command(
        o::CHECK_OPEN,
        p.to_element(),
        &order_aggregate::open(rvc, dl_uuid::Uuid::new_v4()),
        None,
    )
}

/// A check T4 in `rvc` holding one A line at the combo price of A, round 1, seat 0, sent
/// to the kitchen when `sent` (`ComboStoreSchema.SeedCheck`, posted by the verbs).
fn seed_check(t: &Live, rvc: i32, sent: bool) -> (i64, i64) {
    let check = t.applied(&open(rvc)).check_no;
    let p = OrderItemPostPayload {
        rvc_no: rvc,
        check_no: check,
        cashier: CLERK.to_owned(),
        item_code: "A".to_owned(),
        qty: Decimal::ONE,
        seat_no: 0,
        price: dec("40000"),
        note: None,
        open_name: None,
        allow_zero_open_price: false,
    };
    let key = check_aggregate::key(rvc, check);
    let x = t
        .applied(&command(o::ITEM_POST, p.to_element(), &key, None))
        .trn_seq
        .unwrap();
    if sent {
        let p = OrderSendPayload {
            rvc_no: rvc,
            check_no: check,
            table_no: None,
            cashier: CLERK.to_owned(),
        };
        t.applied(&command(o::SEND, p.to_element(), &key, None));
    }
    (check, x)
}

fn combo(
    rvc: i32,
    check: i64,
    code: &str,
    qty: &str,
    price: &str,
    note: Option<&str>,
    selected: Option<&[&str]>,
) -> SyncCommand {
    let p = OrderComboPostPayload {
        rvc_no: rvc,
        check_no: check,
        cashier: CLERK.to_owned(),
        combo_code: code.to_owned(),
        qty: dec(qty),
        seat_no: 0,
        price: dec(price),
        note: note.map(str::to_owned),
        selected_components: selected.map(|v| v.iter().map(|s| (*s).to_owned()).collect()),
    };
    command(
        o::COMBO_POST,
        p.to_element(),
        &check_aggregate::key(rvc, check),
        None,
    )
}

/// `item|qty|price` of the check's lines other than `x`, in line order.
fn lines(t: &Live, check: i64, x: i64) -> Vec<String> {
    t.rows(
        "SELECT RTRIM(ItemCode), CAST(TrnQty AS decimal(18, 2)), ItemPrice FROM dbo.tmpTrn \
         WHERE CheckNo = @c AND TrnSeq <> @x ORDER BY TrnSeq",
        &[Param::new("c", check), Param::new("x", x)],
    )
}

fn line(t: &Live, seq: i64) -> String {
    t.one(
        "SELECT RTRIM(ItemCode), CAST(TrnQty AS decimal(18, 2)), ItemPrice FROM dbo.tmpTrn WHERE TrnSeq = @s",
        &[Param::new("s", seq)],
    )
}

fn notes(t: &Live, check: i64) -> String {
    t.one(
        "SELECT COUNT(*) FROM dbo.CGTrn WHERE TrnNo IN (SELECT TrnSeq FROM dbo.tmpTrn WHERE CheckNo = @c)",
        &[Param::new("c", check)],
    )
}

#[test]
fn combos_post_their_components_exactly_once() {
    let Some(t) = Live::start() else { return };

    // A package component: A, then package B with its children and balance line, twice.
    let (check, x) = seed_check(&t, RVC, true);
    let last_trn = t.para(RVC, "LastTrn");
    let r = t.applied(&combo(RVC, check, "10", "2", "0", None, None));
    assert_eq!(
        lines(&t, check, x),
        ["A|2|40000", "B|4|30000", "B1|4|0", "B2|4|5000", "B|0|30000"]
    );
    assert_eq!(
        t.para(RVC, "LastTrn"),
        last_trn + 5,
        "the redelivery drew no line numbers"
    );
    assert_eq!(r.trn_seq, Some(last_trn + 1 + i64::from(RVC) * 100_000_000));
    assert_eq!(r.sub_total, dec("260000"));
    assert_eq!(r.fingerprint_after, t.seen(check));

    // A note: one note row per combo component.
    let before = notes(&t, check);
    t.applied(&combo(RVC, check, "10", "1", "0", Some("it cay"), None));
    assert_eq!(
        notes(&t, check).parse::<i64>().unwrap(),
        before.parse::<i64>().unwrap() + 2
    );

    // A selection posts only the chosen components at their package price; one outside
    // the combo is a logged item_not_found that writes nothing.
    let (check, x) = seed_check(&t, RVC, true);
    t.applied(&combo(RVC, check, "10", "1", "0", None, Some(&["B"])));
    assert_eq!(
        lines(&t, check, x),
        ["B|2|30000", "B1|2|0", "B2|2|5000", "B|0|30000"]
    );
    let before = (lines(&t, check, x), t.para(RVC, "LastTrn"));
    t.refused(
        &combo(RVC, check, "10", "1", "0", None, Some(&["A", "C"])),
        order::ITEM_NOT_FOUND,
    );
    assert_eq!((lines(&t, check, x), t.para(RVC, "LastTrn")), before);

    // A numeric code without components, and a code that is not a number, post the item
    // itself at the client's price.
    let (check, x) = seed_check(&t, RVC, true);
    t.applied(&combo(RVC, check, "77", "1", "99000", None, None));
    t.applied(&combo(RVC, check, "C", "1", "8000", None, None));
    assert_eq!(lines(&t, check, x), ["77|1|99000", "C|1|8000"]);

    // By course, the last component merges into the unsent A line of the round (GroupChkTrn),
    // and a component after A never takes over that line (the legacy loop bug).
    let (check, x) = seed_check(&t, RVC_BY_COURSE, false);
    t.applied(&combo(RVC_BY_COURSE, check, "20", "1", "0", None, None));
    assert_eq!(line(&t, x), "A|2|40000", "A merged into the unsent A line");
    assert_eq!(lines(&t, check, x), ["77|1|20000"]);
    let (check, x) = seed_check(&t, RVC_BY_COURSE, false);
    t.applied(&combo(RVC_BY_COURSE, check, "30", "1", "0", None, None));
    assert_eq!(line(&t, x), "A|2|40000", "the A line is kept");
    assert_eq!(lines(&t, check, x), ["C|1|6000"]);

    // A check missing from tmpInvoice is refused; nothing is created.
    let missing = i64::from(RVC) * 100_000_000 + 99_999;
    t.refused(
        &combo(RVC, missing, "10", "1", "0", None, None),
        order::CHECK_NOT_FOUND,
    );
    assert_eq!(
        t.one(
            "SELECT (SELECT COUNT(*) FROM dbo.tmpInvoice WHERE CheckNo = @c) + (SELECT COUNT(*) FROM dbo.tmpTrn WHERE CheckNo = @c)",
            &[Param::new("c", missing)]
        ),
        "0"
    );
}

fn kds_cmd(rvc: i32, check: i64, seqs: &[i64], action: &str) -> SyncCommand {
    let p = KdsLineStatePayload {
        rvc_no: rvc,
        check_no: check,
        line_seqs: seqs.to_vec(),
        action: action.to_owned(),
        cashier: "AN".to_owned(),
    };
    command(
        s::KDS_LINE_STATE,
        p.to_element(),
        &check_aggregate::key(rvc, check),
        None,
    )
}

fn kds_lines(t: &Live, c: &SyncCommand) -> i32 {
    let r = t.once(c);
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    KdsLineStateResult::from_element(r.result.as_ref().unwrap())
        .unwrap()
        .lines
}

#[test]
fn kds_line_states_and_reservations_apply_exactly_once() {
    let Some(t) = Live::start() else { return };
    let info = || {
        t.one(
            "SELECT ISNULL(KdsAcceptedBy, ''), CASE WHEN KdsAcceptedTime IS NULL THEN 0 ELSE 1 END, \
             ISNULL(IsDone, 0), CASE WHEN DoneTime IS NULL THEN 0 ELSE 1 END \
             FROM dbo.tmpTrnInfo WHERE CheckNo = 1001 AND TrnNo = 5002",
            &[],
        )
    };

    // Open check 1001 of the seed (RVC 1): accept, done and their undo on line 5002.
    assert_eq!(
        kds_lines(&t, &kds_cmd(1, 1001, &[5002, 5002], kds::ACCEPT)),
        1
    );
    assert_eq!(info(), "AN|1|0|0");
    assert_eq!(
        kds_lines(&t, &kds_cmd(1, 1001, &[5002], kds::ACCEPT)),
        0,
        "accept is idempotent"
    );
    assert_eq!(kds_lines(&t, &kds_cmd(1, 1001, &[5002], kds::DONE)), 1);
    assert_eq!(info(), "AN|1|1|1");
    assert_eq!(kds_lines(&t, &kds_cmd(1, 1001, &[5002], kds::UNDONE)), 1);
    t.refused(&kds_cmd(1, 1001, &[5002], kds::UNDONE), reasons::NOT_DONE);
    assert_eq!(kds_lines(&t, &kds_cmd(1, 1001, &[5002], kds::UNACCEPT)), 1);
    t.refused(
        &kds_cmd(1, 1001, &[5002], kds::UNACCEPT),
        reasons::NOT_ACCEPTED,
    );
    assert_eq!(info(), "|0|0|0");
    t.refused(
        &kds_cmd(2, 1001, &[5002], kds::DONE),
        reasons::CHECK_NOT_FOUND,
    );

    // reservation.save: one row and the same site identity on redelivery.
    let before = t.one("SELECT COUNT(*) FROM dbo.TableReservation", &[]);
    let save = command(
        s::RESERVATION_SAVE,
        stj::parse(
            r#"{"rvc_no":1,"guest_info":"Chị Hoa – kỷ niệm","num_adult":4,"num_child":0,"from_date_time":"2026-11-20T19:00:00","to_date_time":"2026-11-20T21:00:00","table_num":"T02","tel_num":"0900000000","booker":"AN"}"#,
        )
        .unwrap(),
        "reservation:1",
        None,
    );
    let saved = t.once(&save);
    assert_eq!(saved.status, CommandStatus::Applied, "{:?}", saved.reason);
    let id = ReservationSavedResult::from_element(saved.result.as_ref().unwrap())
        .unwrap()
        .id;
    assert_eq!(
        t.one("SELECT COUNT(*) FROM dbo.TableReservation", &[])
            .parse::<i64>()
            .unwrap(),
        before.parse::<i64>().unwrap() + 1
    );
    assert_eq!(
        t.one(
            "SELECT GuestInfo, NumAdult, CONVERT(varchar(19), FromDateTime, 126), TableNum, RVC \
             FROM dbo.TableReservation WHERE nID = @id",
            &[Param::new("id", id)]
        ),
        "Chị Hoa – kỷ niệm|4|2026-11-20T19:00:00|T02|1"
    );

    // reservation.cancel: only within the RVC, once.
    let cancel = |rvc: i32| {
        let p = ReservationCancelPayload {
            rvc_no: rvc,
            id,
            cancelled_by: "AN".to_owned(),
        };
        command(s::RESERVATION_CANCEL, p.to_element(), "reservation:1", None)
    };
    t.refused(&cancel(2), reasons::RESERVATION_NOT_FOUND);
    let cancelled = t.once(&cancel(1));
    assert_eq!(cancelled.status, CommandStatus::Applied);
    assert_eq!(
        t.one(
            "SELECT COUNT(*) FROM dbo.TableReservation WHERE nID = @id",
            &[Param::new("id", id)]
        ),
        "0"
    );
    t.refused(&cancel(1), reasons::RESERVATION_NOT_FOUND);
}
