//! The site command executor on the in-memory fake (ports of the C#
//! `SiteCommandExecutorTests`, `SiteCommandStateGuardTests`, `SiteCommandOrderTests`,
//! `SiteCommandComboTests` and `SiteCommandTableTests`).

mod commands_support;

use std::sync::{Arc, Mutex};

use commands_support::{Check, CountingNudge, FakeStore, Line, Throw, clock, lock};
use dl_agent_pda_sync::commands::{SiteCommandExecutor, TableSchemaGuard, reasons, schema_columns};
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CheckStateFingerprint, CheckStateHeader, CheckStateLine, CommandStatus, KdsLineStatePayload,
    KdsLineStateResult, OrderCheckCancelEmptyPayload, OrderCheckExpectation, OrderCheckLine,
    OrderCheckMergePayload, OrderCheckOpenPayload, OrderCheckResult, OrderComboPostPayload,
    OrderCoversPayload, OrderItemPostPayload, OrderItemVoidPayload, OrderItemsMovePayload,
    OrderMultiCheckResult, OrderSendPayload, OrderTableChangePayload, OrderTableTransferPayload,
    ReservationCancelPayload, ReservationSavedResult, SyncCommand, SyncCommandResult,
    kds_line_actions as kds, multi_check_aggregate, order_aggregate,
    order_command_reasons as order, order_command_types as o, site_command_types as s,
    site_event_levels, table_command_reasons as table, table_command_types as t,
};
use dl_agent_pda_sync::reporting::{Logger, SiteEvents};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_decimal::Decimal;
use dl_json::Value as Json;
use dl_uuid::Uuid;

const RVC: i32 = 3;
const CHECK: i64 = 5001;
const NEVER_SEEN: &str = "cs1:0000";

#[derive(Default)]
struct RecordingEvents(Mutex<Vec<(String, String, Option<dl_json::Map>)>>);

impl SiteEvents for RecordingEvents {
    fn report(&self, level: &str, code: &str, _message: &str, detail: Option<dl_json::Map>) {
        lock(&self.0).push((level.to_owned(), code.to_owned(), detail));
    }
}

struct T {
    store: Arc<FakeStore>,
    nudge: Arc<CountingNudge>,
    events: Arc<RecordingEvents>,
    schema: Arc<TableSchemaGuard>,
}

impl T {
    fn new() -> T {
        let events = Arc::new(RecordingEvents::default());
        let schema = Arc::new(TableSchemaGuard::new(events.clone(), &Logger::none()));
        T {
            store: FakeStore::new(),
            nudge: Arc::new(CountingNudge::default()),
            events,
            schema,
        }
    }

    fn executor(&self) -> SiteCommandExecutor {
        SiteCommandExecutor::new(self.store.clone(), clock(1_794_729_600), &Logger::none())
            .with_nudge(self.nudge.clone())
            .with_schema(self.schema.clone())
    }

    fn exec(&self, c: &SyncCommand) -> SyncCommandResult {
        self.executor()
            .execute(c, &Ctx::background())
            .expect("the command runs")
    }

    fn seed_kds(&self, seqs: &[i64]) {
        let mut st = self.store.state();
        st.open_checks.insert((RVC, CHECK));
        for seq in seqs {
            st.lines.insert((CHECK, *seq), Line::default());
        }
    }

    fn seen(&self, check: i64) -> String {
        self.store
            .state()
            .fingerprints
            .get(&check)
            .cloned()
            .unwrap_or_else(|| NEVER_SEEN.to_owned())
    }

    fn open(&self, table: &str, items: &[&str]) -> i64 {
        let opened = self.exec(&open_cmd(table, 2));
        assert_eq!(opened.status, CommandStatus::Applied);
        let check = result(&opened).check_no;
        for item in items {
            assert_eq!(
                self.exec(&post(check, item, Decimal::ONE, None, None))
                    .status,
                CommandStatus::Applied
            );
        }
        check
    }

    fn close(&self, check: i64) {
        if let Some(c) = self.store.state().orders.checks.get_mut(&(RVC, check)) {
            c.status = 2;
        }
    }

    fn calls_starting(&self, prefix: &str) -> usize {
        self.store
            .calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

fn command(type_: &str, payload: Json, aggregate: &str, expected: Option<&str>) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: type_.to_owned(),
        aggregate_key: aggregate.to_owned(),
        expected_hash: expected.map(str::to_owned),
        payload,
        created_by: "clerk-7".to_owned(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn key(check: i64) -> String {
    format!("check:{RVC}:{check}")
}

fn dec(n: i64) -> Decimal {
    Decimal::from(n)
}

fn result(r: &SyncCommandResult) -> OrderCheckResult {
    OrderCheckResult::result_from(r.result.as_ref()).expect("an order check result")
}

fn multi(r: &SyncCommandResult) -> OrderMultiCheckResult {
    OrderMultiCheckResult::multi_result_from(r.result.as_ref()).expect("a multi-check result")
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn invalid(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Rejected);
    assert!(
        r.reason
            .as_deref()
            .unwrap_or("")
            .starts_with(reasons::INVALID_PAYLOAD),
        "{:?}",
        r.reason
    );
}

fn kds_cmd(action: &str, seqs: &[i64], expected: Option<&str>, aggregate: &str) -> SyncCommand {
    let p = KdsLineStatePayload {
        rvc_no: RVC,
        check_no: CHECK,
        line_seqs: seqs.to_vec(),
        action: action.to_owned(),
        cashier: "clerk-7".to_owned(),
    };
    command(s::KDS_LINE_STATE, p.to_element(), aggregate, expected)
}

fn kds_simple(action: &str, seqs: &[i64]) -> SyncCommand {
    kds_cmd(action, seqs, None, "check:3:5001")
}

fn reservation(rvc: i32) -> Json {
    stj::parse(&format!(
        r#"{{"rvc_no":{rvc},"guest_info":"Ms Lan","num_adult":2,"num_child":1,"from_date_time":"2026-10-28T19:00:00","to_date_time":"2026-10-28T21:00:00","table_num":"T4","tel_num":"0900000000","booker":"clerk-7"}}"#
    ))
    .unwrap()
}

fn open_cmd(table: &str, guests: i32) -> SyncCommand {
    let p = OrderCheckOpenPayload {
        rvc_no: RVC,
        table_no: table.to_owned(),
        num_guest: guests,
        cashier: "clerk-7".to_owned(),
    };
    command(
        o::CHECK_OPEN,
        p.to_element(),
        &order_aggregate::open(RVC, Uuid::new_v4()),
        None,
    )
}

fn post(
    check: i64,
    item: &str,
    qty: Decimal,
    open_name: Option<&str>,
    expected: Option<&str>,
) -> SyncCommand {
    let p = OrderItemPostPayload {
        rvc_no: RVC,
        check_no: check,
        cashier: "clerk-7".to_owned(),
        item_code: item.to_owned(),
        qty,
        seat_no: 0,
        price: dec(50_000),
        note: Some("ít đá".to_owned()),
        open_name: open_name.map(str::to_owned),
        allow_zero_open_price: false,
    };
    command(o::ITEM_POST, p.to_element(), &key(check), expected)
}

fn send(check: i64) -> SyncCommand {
    let p = OrderSendPayload {
        rvc_no: RVC,
        check_no: check,
        table_no: Some("T4".to_owned()),
        cashier: "clerk-7".to_owned(),
        pda_handled: None,
    };
    command(o::SEND, p.to_element(), &key(check), None)
}

fn void(check: i64, seq: i64, sent: bool, reason: Option<&str>) -> SyncCommand {
    let p = OrderItemVoidPayload {
        rvc_no: RVC,
        check_no: check,
        trn_seq: seq,
        cashier: "clerk-7".to_owned(),
        sent,
        reason: reason.map(str::to_owned),
        author_cashier: None,
    };
    command(o::ITEM_VOID, p.to_element(), &key(check), None)
}

fn covers(check: i64, guests: i32, children: i32, expected: Option<&str>) -> SyncCommand {
    let p = OrderCoversPayload {
        rvc_no: RVC,
        check_no: check,
        num_guest: guests,
        num_child: children,
        cashier: "clerk-7".to_owned(),
    };
    command(o::COVERS, p.to_element(), &key(check), expected)
}

fn combo(
    check: i64,
    code: &str,
    qty: Decimal,
    selected: Option<Vec<&str>>,
    expected: Option<&str>,
) -> SyncCommand {
    let p = OrderComboPostPayload {
        rvc_no: RVC,
        check_no: check,
        cashier: "clerk-7".to_owned(),
        combo_code: code.to_owned(),
        qty,
        seat_no: 0,
        price: Decimal::ZERO,
        note: None,
        selected_components: selected.map(|v| v.into_iter().map(str::to_owned).collect()),
    };
    command(o::COMBO_POST, p.to_element(), &key(check), expected)
}

fn table_change(check: i64, table_no: &str, guests: i32, expected: Option<&str>) -> SyncCommand {
    let p = OrderTableChangePayload {
        rvc_no: RVC,
        check_no: check,
        table_no: table_no.to_owned(),
        num_guest: guests,
        cashier: "clerk-7".to_owned(),
    };
    command(t::TABLE_CHANGE, p.to_element(), &key(check), expected)
}

fn transfer(check: i64, table_no: &str, expected: Option<&str>) -> SyncCommand {
    let p = OrderTableTransferPayload {
        rvc_no: RVC,
        check_no: check,
        to_table_no: table_no.to_owned(),
        cashier: "clerk-7".to_owned(),
    };
    command(t::TABLE_TRANSFER, p.to_element(), &key(check), expected)
}

fn expect(check_no: i64, expected: &str) -> OrderCheckExpectation {
    OrderCheckExpectation {
        check_no,
        expected: expected.to_owned(),
    }
}

impl T {
    fn merge(
        &self,
        target: i64,
        source: i64,
        target_seen: Option<&str>,
        source_seen: Option<&str>,
    ) -> SyncCommand {
        let checks = vec![
            expect(
                target,
                &target_seen.map_or_else(|| self.seen(target), str::to_owned),
            ),
            expect(
                source,
                &source_seen.map_or_else(|| self.seen(source), str::to_owned),
            ),
        ];
        let p = OrderCheckMergePayload {
            rvc_no: RVC,
            target_check_no: target,
            source_check_no: source,
            cashier: "clerk-7".to_owned(),
            checks,
        };
        command(
            t::CHECK_MERGE,
            p.to_element(),
            &multi_check_aggregate::keys(RVC, target, source).0,
            None,
        )
    }

    fn move_cmd(
        &self,
        from: i64,
        to: i64,
        seqs: &[i64],
        from_seen: Option<&str>,
        to_seen: Option<&str>,
    ) -> SyncCommand {
        let checks = vec![
            expect(
                from,
                &from_seen.map_or_else(|| self.seen(from), str::to_owned),
            ),
            expect(to, &to_seen.map_or_else(|| self.seen(to), str::to_owned)),
        ];
        let p = OrderItemsMovePayload {
            rvc_no: RVC,
            from_check_no: from,
            to_check_no: to,
            trn_seqs: seqs.to_vec(),
            cashier: "clerk-7".to_owned(),
            checks,
        };
        command(
            t::ITEMS_MOVE,
            p.to_element(),
            &multi_check_aggregate::keys(RVC, from, to).0,
            None,
        )
    }
}

// ---- SiteCommandExecutorTests

#[test]
fn kds_accept_changes_only_lines_not_yet_accepted_and_a_redelivery_does_not_write_again() {
    let t = T::new();
    t.seed_kds(&[1, 2, 3]);
    t.store.state().lines.insert(
        (CHECK, 2),
        Line {
            accepted: true,
            accepted_by: Some("other".into()),
            done: false,
        },
    );
    let cmd = kds_simple(kds::ACCEPT, &[1, 2, 3, 3]);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied);
    let r = KdsLineStateResult::from_element(first.result.as_ref().unwrap()).unwrap();
    assert_eq!(r.lines, 2);
    assert_eq!(
        t.store.state().lines[&(CHECK, 1)].accepted_by.as_deref(),
        Some("clerk-7")
    );
    assert_eq!(
        t.store.state().lines[&(CHECK, 2)].accepted_by.as_deref(),
        Some("other")
    );
    assert_eq!(replay, first);
    assert_eq!(t.store.work_runs(), 1);
}

#[test]
fn kds_done_then_undone_round_trips_and_undoing_a_line_that_is_not_done_is_rejected_and_logged() {
    let t = T::new();
    t.seed_kds(&[1]);
    assert_eq!(
        t.exec(&kds_simple(kds::DONE, &[1])).status,
        CommandStatus::Applied
    );
    assert!(t.store.state().lines[&(CHECK, 1)].done);
    assert_eq!(
        t.exec(&kds_simple(kds::UNDONE, &[1])).status,
        CommandStatus::Applied
    );
    assert!(!t.store.state().lines[&(CHECK, 1)].done);

    let again = kds_simple(kds::UNDONE, &[1]);
    refused(&t.exec(&again), reasons::NOT_DONE);
    assert!(
        t.store.logged(again.command_id),
        "a business refusal is logged"
    );

    refused(
        &t.exec(&kds_simple(kds::UNACCEPT, &[1])),
        reasons::NOT_ACCEPTED,
    );
}

#[test]
fn kds_command_for_a_check_of_another_rvc_or_without_the_accept_columns_is_rejected_without_writes()
{
    let t = T::new();
    t.seed_kds(&[1]);
    let p = KdsLineStatePayload {
        rvc_no: RVC + 1,
        check_no: CHECK,
        line_seqs: vec![1],
        action: kds::DONE.into(),
        cashier: "clerk-7".into(),
    };
    refused(
        &t.exec(&command(
            s::KDS_LINE_STATE,
            p.to_element(),
            "check:3:5001",
            None,
        )),
        reasons::CHECK_NOT_FOUND,
    );

    t.store.knobs().kds_accept_columns_missing = true;
    refused(
        &t.exec(&kds_simple(kds::ACCEPT, &[1])),
        reasons::KDS_ACCEPT_UNSUPPORTED,
    );
    assert_eq!(t.store.state().lines[&(CHECK, 1)], Line::default());
}

#[test]
fn reservation_save_returns_the_site_identity_and_cancel_deletes_only_within_the_rvc() {
    let t = T::new();
    let saved = t.exec(&command(
        s::RESERVATION_SAVE,
        reservation(RVC),
        "reservation:3",
        None,
    ));
    assert_eq!(saved.status, CommandStatus::Applied, "{:?}", saved.reason);
    let id = ReservationSavedResult::from_element(saved.result.as_ref().unwrap())
        .unwrap()
        .id;
    assert_eq!(
        t.store.state().reservations[&id],
        (RVC, "Ms Lan".to_owned())
    );

    let cancel = |rvc| ReservationCancelPayload {
        rvc_no: rvc,
        id,
        cancelled_by: "clerk-7".into(),
    };
    refused(
        &t.exec(&command(
            s::RESERVATION_CANCEL,
            cancel(RVC + 1).to_element(),
            "reservation:3",
            None,
        )),
        reasons::RESERVATION_NOT_FOUND,
    );
    assert!(t.store.state().reservations.contains_key(&id));
    let cancelled = t.exec(&command(
        s::RESERVATION_CANCEL,
        cancel(RVC).to_element(),
        "reservation:3",
        None,
    ));
    assert_eq!(cancelled.status, CommandStatus::Applied);
    assert!(!t.store.state().reservations.contains_key(&id));
}

#[test]
fn unknown_type_and_invalid_payload_are_rejected_without_opening_the_store() {
    let t = T::new();
    refused(
        &t.exec(&command("order.post", stj::parse("{}").unwrap(), "x", None)),
        reasons::UNKNOWN_TYPE,
    );
    invalid(&t.exec(&command(s::RESERVATION_SAVE, reservation(0), "x", None)));
    let not_object = t.exec(&command(
        s::KDS_LINE_STATE,
        stj::parse("[1]").unwrap(),
        "x",
        None,
    ));
    invalid(&not_object);
    assert_eq!(
        not_object.reason.as_deref(),
        Some("invalid_payload: payload is not a kds.line_state object")
    );
    assert_eq!(t.store.runs(), 0);
}

#[test]
fn a_failure_inside_the_transaction_rolls_back_the_write_and_the_log_row() {
    let t = T::new();
    t.store.knobs().throw_in_work = Some(Throw::Invalid("connection reset".into()));
    let cmd = command(s::RESERVATION_SAVE, reservation(RVC), "x", None);

    assert!(t.executor().execute(&cmd, &Ctx::background()).is_err());
    assert!(t.store.state().reservations.is_empty());
    assert!(!t.store.logged(cmd.command_id));

    t.store.knobs().throw_in_work = None;
    assert_eq!(
        t.exec(&cmd).status,
        CommandStatus::Applied,
        "the retry runs for real"
    );
    assert_eq!(t.store.state().reservations.len(), 1);
}

// ---- SiteCommandStateGuardTests

fn seen_state() -> String {
    CheckStateFingerprint::compute(
        &CheckStateHeader {
            check_no: CHECK,
            table_no: Some("T4".into()),
            guests: 2,
            sub_total: dec(100_000),
            total_paid: Decimal::ZERO,
        },
        &[CheckStateLine {
            line_seq: 1,
            transaction_code: Some("1".into()),
            item_code: Some("CF01".into()),
            qty: Decimal::ONE,
            price: dec(50_000),
        }],
    )
}

fn seed_guard(t: &T, fingerprint: &str) {
    t.seed_kds(&[1]);
    t.store
        .state()
        .fingerprints
        .insert(CHECK, fingerprint.to_owned());
}

#[test]
fn guard_runs_the_command_when_the_check_still_matches_what_the_user_saw() {
    let t = T::new();
    seed_guard(&t, &seen_state());
    let r = t.exec(&kds_cmd(
        kds::DONE,
        &[1],
        Some(&seen_state()),
        "check:3:5001",
    ));
    assert_eq!(r.status, CommandStatus::Applied);
    assert!(t.store.state().lines[&(CHECK, 1)].done);
}

#[test]
fn guard_refuses_a_check_changed_on_pos_win_and_a_redelivery_answers_the_same() {
    let t = T::new();
    seed_guard(&t, "cs1:changed");
    let cmd = kds_cmd(kds::DONE, &[1], Some(&seen_state()), "check:3:5001");

    let first = t.exec(&cmd);
    t.store.state().fingerprints.insert(CHECK, seen_state());
    let replay = t.exec(&cmd);

    refused(&first, reasons::CHECK_CHANGED);
    assert!(!t.store.state().lines[&(CHECK, 1)].done);
    assert_eq!(replay, first);
    assert_eq!(t.store.work_runs(), 1);
}

#[test]
fn guard_refuses_a_check_closed_on_pos_win_as_not_found() {
    let t = T::new();
    refused(
        &t.exec(&kds_cmd(
            kds::DONE,
            &[1],
            Some(&seen_state()),
            "check:3:5001",
        )),
        reasons::CHECK_NOT_FOUND,
    );
}

#[test]
fn guard_refuses_an_expected_state_on_an_aggregate_that_is_not_a_check_unexecuted() {
    let t = T::new();
    seed_guard(&t, &seen_state());
    refused(
        &t.exec(&kds_cmd(
            kds::DONE,
            &[1],
            Some(&seen_state()),
            "reservation:3",
        )),
        reasons::NOT_A_CHECK,
    );
    assert_eq!(t.store.runs(), 0);
}

#[test]
fn guard_is_skipped_without_an_expected_state() {
    let t = T::new();
    t.seed_kds(&[1]);
    assert_eq!(
        t.exec(&kds_simple(kds::DONE, &[1])).status,
        CommandStatus::Applied
    );
}

// ---- SiteCommandOrderTests

#[test]
fn open_answers_with_the_new_check_and_a_redelivery_does_not_open_a_second_one() {
    let t = T::new();
    let cmd = open_cmd("T9", 4);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied);
    let check = result(&first);
    assert_eq!(
        (
            check.rvc_no,
            check.check_no,
            check.table_no.as_deref(),
            check.guests
        ),
        (RVC, 7001, Some("T9"), 4)
    );
    assert_eq!(check.fingerprint_after, t.store.state().fingerprints[&7001]);
    assert_eq!(replay, first);
    assert_eq!(t.store.work_runs(), 1);
    assert_eq!(t.store.calls(), ["open 3 T9 4 clerk-7"]);
    assert_eq!(
        t.nudge.count(),
        2,
        "a redelivery answered from the log nudges again"
    );
}

#[test]
fn post_send_and_both_voids_apply_with_the_post_state_of_the_check() {
    let t = T::new();
    let check = t.open("T4", &[]);

    let posted = result(&t.exec(&post(check, "CF01", Decimal::ONE, None, None)));
    let second = result(&t.exec(&post(check, "TEA", dec(2), None, None)));
    assert_eq!(posted.trn_seq, Some(1));
    assert_eq!(second.lines.len(), 2);
    assert_eq!(second.sub_total, dec(150_000));

    let sent = result(&t.exec(&send(check)));
    assert_eq!(sent.sent_seqs, Some(vec![1, 2]));
    assert!(sent.lines.iter().all(|l| l.sent));

    let third = result(&t.exec(&post(check, "CF01", Decimal::ONE, None, None)));
    let unsent_void = result(&t.exec(&void(check, third.trn_seq.unwrap(), false, None)));
    assert_eq!(
        unsent_void
            .lines
            .iter()
            .map(|l| l.line_seq)
            .collect::<Vec<_>>(),
        [1, 2]
    );

    let sent_void = result(&t.exec(&void(check, 2, true, Some("khách đổi món"))));
    assert_eq!(
        sent_void
            .lines
            .iter()
            .map(|l| l.line_seq)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        sent_void.fingerprint_after,
        t.store.state().fingerprints[&check]
    );

    assert_eq!(
        t.store.calls(),
        [
            "open 3 T4 2 clerk-7".to_owned(),
            format!("post {check} CF01 x1"),
            format!("post {check} TEA x2"),
            format!("send {check}"),
            format!("post {check} CF01 x1"),
            format!("void-unsent {check} 3"),
            format!("void-sent {check} 2 khách đổi món"),
        ]
    );
    assert_eq!(t.nudge.count(), 7);
}

#[test]
fn post_to_a_check_missing_from_tmpinvoice_is_refused_and_never_recreates_the_header() {
    let t = T::new();
    let cmd = post(424_242, "CF01", Decimal::ONE, None, None);
    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);
    refused(&first, order::CHECK_NOT_FOUND);
    assert!(t.store.state().orders.checks.is_empty());
    assert!(t.store.calls().is_empty());
    assert_eq!(replay, first);
    assert_eq!(t.nudge.count(), 0);
}

#[test]
fn a_post_the_batch_refuses_is_a_logged_rejection() {
    for reason in [
        order::OPEN_NAME_REQUIRED,
        order::OPEN_PRICE_REQUIRED,
        order::CHECK_NOT_FOUND,
    ] {
        let t = T::new();
        let check = t.open("T4", &[]);
        t.store.knobs().post_refusal = Some(reason.to_owned());
        let cmd = post(check, "CF01", Decimal::ONE, None, None);
        let first = t.exec(&cmd);
        let replay = t.exec(&cmd);
        refused(&first, reason);
        assert_eq!(replay, first);
        assert!(t.store.state().orders.lines.is_empty());
    }
}

#[test]
fn post_of_an_item_not_in_itemdef_is_refused() {
    let t = T::new();
    let check = t.open("T4", &[]);
    refused(
        &t.exec(&post(check, "NOPE", Decimal::ONE, None, None)),
        order::ITEM_NOT_FOUND,
    );
}

#[test]
fn unsent_removal_of_a_sent_line_is_refused_and_the_line_stays() {
    let t = T::new();
    let check = t.open("T4", &["CF01"]);
    t.exec(&send(check));
    refused(
        &t.exec(&void(check, 1, false, None)),
        order::ITEM_ALREADY_SENT,
    );
    assert!(t.store.state().orders.lines.contains_key(&(check, 1)));
}

#[test]
fn void_refuses_a_missing_line_a_non_item_line_and_a_closed_check() {
    let t = T::new();
    let check = t.open("T4", &[]);
    t.store.state().orders.lines.insert(
        (check, 9),
        OrderCheckLine {
            line_seq: 9,
            transaction_code: Some("200".into()),
            item_code: None,
            item_name: Some("Giảm giá".into()),
            qty: Decimal::ONE,
            price: dec(-1),
            amount: dec(-1),
            seat_no: 0,
            cashier: Some("clerk-7".into()),
            sent: false,
            note: None,
        },
    );
    refused(
        &t.exec(&void(check, 77, false, None)),
        order::LINE_NOT_FOUND,
    );
    refused(
        &t.exec(&void(check, 9, false, None)),
        order::NOT_AN_ITEM_LINE,
    );
    t.close(check);
    refused(
        &t.exec(&void(check, 9, true, Some("x"))),
        order::BILL_NOT_OPEN,
    );
    assert_eq!(t.calls_starting("void"), 0);
}

#[test]
fn post_against_a_check_pos_win_changed_since_the_user_saw_it_is_refused_without_writing() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let seen = t.seen(check);
    t.exec(&post(check, "TEA", Decimal::ONE, None, None));
    refused(
        &t.exec(&post(check, "CF01", Decimal::ONE, None, Some(&seen))),
        reasons::CHECK_CHANGED,
    );
    assert_eq!(t.store.state().orders.lines.len(), 1);
}

#[test]
fn open_item_name_is_cut_to_30_characters_like_pos_win() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let name = "M".repeat(45);
    let r = result(&t.exec(&post(check, "CF01", Decimal::ONE, Some(&name), None)));
    assert_eq!(r.lines[0].item_name.as_deref().map(str::len), Some(30));
}

#[test]
fn invalid_order_payload_is_rejected_without_a_store_write() {
    let t = T::new();
    invalid(&t.exec(&post(1, "CF01", Decimal::ZERO, None, None)));
    assert!(t.store.calls().is_empty());
    assert_eq!(t.nudge.count(), 0);
}

#[test]
fn covers_apply_once_and_answer_with_the_new_guest_count_and_fingerprint() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let before = t.seen(check);
    let cmd = covers(check, 5, 1, None);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied);
    let r = result(&first);
    assert_eq!(r.guests, 5);
    assert_ne!(r.fingerprint_after, before);
    assert_eq!(r.fingerprint_after, t.seen(check));
    assert_eq!(
        t.store.state().orders.checks[&(RVC, check)],
        Check {
            table_no: "T4".into(),
            guests: 5,
            status: 1,
            children: 1
        }
    );
    assert_eq!(replay, first);
    assert_eq!(
        t.store.calls(),
        [
            "open 3 T4 2 clerk-7".to_owned(),
            format!("covers {check} 5/1 clerk-7")
        ]
    );
    assert_eq!(t.nudge.count(), 3);
}

#[test]
fn covers_on_a_missing_or_closed_check_are_refused_before_the_batch_runs() {
    let t = T::new();
    refused(
        &t.exec(&covers(424_242, 3, 0, None)),
        order::CHECK_NOT_FOUND,
    );
    let check = t.open("T4", &[]);
    t.close(check);
    refused(&t.exec(&covers(check, 3, 0, None)), order::BILL_NOT_OPEN);
    assert_eq!(t.store.state().orders.checks[&(RVC, check)].guests, 2);
    assert_eq!(t.calls_starting("covers"), 0);
    assert_eq!(t.nudge.count(), 1);
}

#[test]
fn covers_the_batch_refuses_because_pos_win_closed_the_check_meanwhile_are_a_logged_rejection() {
    let t = T::new();
    let check = t.open("T4", &[]);
    t.store.knobs().close_before_covers = true;
    let cmd = covers(check, 6, 0, None);
    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);
    refused(&first, order::BILL_NOT_OPEN);
    assert_eq!(replay, first);
    assert_eq!(t.store.state().orders.checks[&(RVC, check)].guests, 2);
    assert_eq!(t.nudge.count(), 1);
}

#[test]
fn covers_with_an_invalid_count_are_rejected_without_a_store_write() {
    for (guests, children) in [(-1, 0), (2, -1), (1000, 0)] {
        let t = T::new();
        invalid(&t.exec(&covers(7001, guests, children, None)));
        assert_eq!(t.store.work_runs(), 0);
    }
}

// ---- SiteCommandComboTests

#[test]
fn a_combo_posts_each_component_once_and_answers_with_the_first_line_and_the_post_state() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let cmd = combo(check, "10", dec(2), None, None);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    let r = result(&first);
    assert_eq!(r.trn_seq, Some(1));
    let lines: Vec<(Option<&str>, Decimal, Decimal)> = r
        .lines
        .iter()
        .map(|l| (l.item_code.as_deref(), l.qty, l.price))
        .collect();
    assert_eq!(
        lines,
        [
            (Some("CF01"), dec(2), dec(20_000)),
            (Some("TEA"), dec(4), dec(15_000))
        ]
    );
    assert_eq!(r.sub_total, dec(100_000));
    assert_eq!(r.fingerprint_after, t.seen(check));
    assert_eq!(replay, first);
    assert_eq!(
        t.store.calls(),
        [
            "open 3 T4 2 clerk-7".to_owned(),
            format!("combo {check} 10 x2")
        ]
    );
    assert_eq!(t.nudge.count(), 3);
}

#[test]
fn a_selection_posts_only_the_chosen_components() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let r = result(&t.exec(&combo(check, "10", Decimal::ONE, Some(vec![" TEA "]), None)));
    assert_eq!(r.lines.len(), 1);
    assert_eq!(r.lines[0].item_code.as_deref(), Some("TEA"));
}

#[test]
fn a_combo_with_an_unknown_code_is_a_logged_rejection_that_writes_nothing() {
    for (code, selected) in [("10", Some("BEER")), ("NOPE", None)] {
        let t = T::new();
        let check = t.open("T4", &[]);
        let cmd = combo(
            check,
            code,
            Decimal::ONE,
            selected.map(|s| vec!["CF01", s]),
            None,
        );
        let first = t.exec(&cmd);
        let replay = t.exec(&cmd);
        refused(&first, order::ITEM_NOT_FOUND);
        assert_eq!(replay, first);
        assert!(t.store.state().orders.lines.is_empty());
        assert_eq!(t.nudge.count(), 1);
    }
}

#[test]
fn a_combo_item_without_components_posts_itself() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let r = result(&t.exec(&combo(check, "CF01", Decimal::ONE, None, None)));
    assert_eq!(r.lines.len(), 1);
    assert_eq!(r.lines[0].item_code.as_deref(), Some("CF01"));
}

#[test]
fn a_combo_on_a_check_missing_from_tmpinvoice_is_refused_before_the_batch_runs() {
    let t = T::new();
    refused(
        &t.exec(&combo(424_242, "10", Decimal::ONE, None, None)),
        order::CHECK_NOT_FOUND,
    );
    assert!(t.store.calls().is_empty());
}

#[test]
fn a_combo_against_a_changed_check_is_refused_without_writing() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let seen = t.seen(check);
    t.exec(&combo(check, "CF01", Decimal::ONE, None, None));
    refused(
        &t.exec(&combo(check, "10", Decimal::ONE, None, Some(&seen))),
        reasons::CHECK_CHANGED,
    );
    assert_eq!(t.store.state().orders.lines.len(), 1);
}

#[test]
fn an_invalid_combo_payload_is_rejected_without_a_store_write() {
    for (code, qty, selected) in [("", 1, None), ("10", 0, None), ("10", 1, Some("A|B"))] {
        let t = T::new();
        invalid(&t.exec(&combo(1, code, dec(qty), selected.map(|s| vec![s]), None)));
        assert!(t.store.calls().is_empty());
        assert_eq!(t.nudge.count(), 0);
    }
}

// ---- SiteCommandTableTests

#[test]
fn table_change_applies_once_and_answers_with_the_new_table_guests_and_fingerprint() {
    let t = T::new();
    let check = t.open("T4", &[]);
    let cmd = table_change(check, "T12", 5, Some(&t.seen(check)));

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    let r = result(&first);
    assert_eq!((r.table_no.as_deref(), r.guests), (Some("T12"), 5));
    assert_eq!(r.fingerprint_after, t.seen(check));
    assert_eq!(replay, first);
    assert_eq!(
        t.store.calls(),
        [
            "open 3 T4 2 clerk-7".to_owned(),
            format!("table {check} T12 5")
        ]
    );
    assert_eq!(
        t.store
            .schema_probes
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(t.nudge.count(), 3);
}

#[test]
fn table_change_on_a_missing_closed_or_changed_check_is_refused_without_writing() {
    let t = T::new();
    refused(
        &t.exec(&table_change(424_242, "T1", 2, None)),
        order::CHECK_NOT_FOUND,
    );
    let closed = t.open("T4", &[]);
    t.close(closed);
    refused(
        &t.exec(&table_change(closed, "T1", 2, None)),
        order::BILL_NOT_OPEN,
    );
    let changed = t.open("T5", &[]);
    let seen = t.seen(changed);
    t.open("T6", &["CF01"]);
    t.store
        .state()
        .fingerprints
        .insert(changed, "cs1:ffff".into());
    refused(
        &t.exec(&table_change(changed, "T1", 2, Some(&seen))),
        reasons::CHECK_CHANGED,
    );
    assert_eq!(t.calls_starting("table"), 0);
    assert_eq!(t.store.state().orders.checks[&(RVC, closed)].table_no, "T4");
}

#[test]
fn transfer_writes_the_trimmed_table_and_refuses_the_table_the_check_is_on() {
    let t = T::new();
    let check = t.open("T4", &["CF01"]);
    let moved = t.exec(&transfer(check, "  T7 ", Some(&t.seen(check))));
    let same = t.exec(&transfer(check, "t7", None));
    assert_eq!(moved.status, CommandStatus::Applied, "{:?}", moved.reason);
    assert_eq!(result(&moved).table_no.as_deref(), Some("T7"));
    assert_eq!(result(&moved).fingerprint_after, t.seen(check));
    refused(&same, table::SAME_TABLE);
    let transfers: Vec<String> = t
        .store
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("transfer"))
        .collect();
    assert_eq!(transfers, [format!("transfer {check} T7")]);
}

#[test]
fn table_change_with_an_invalid_payload_is_rejected_without_a_store_write() {
    for (check, table_no, guests) in [(0, "T1", 2), (7001, " ", 2), (7001, "T1", 1000)] {
        let t = T::new();
        invalid(&t.exec(&table_change(check, table_no, guests, None)));
        assert_eq!(t.store.work_runs(), 0);
    }
}

#[test]
fn a_store_without_a_touched_column_refuses_the_verb_reports_once_and_is_probed_again() {
    let t = T::new();
    let check = t.open("T4", &[]);
    t.store
        .knobs()
        .missing_columns
        .insert("tablelist.TableStatus".into());

    let first = t.exec(&table_change(check, "T9", 3, None));
    let second = t.exec(&table_change(check, "T9", 3, None));

    refused(&first, table::SCHEMA_UNSUPPORTED);
    refused(&second, table::SCHEMA_UNSUPPORTED);
    {
        let events = lock(&t.events.0);
        assert_eq!(events.len(), 1, "one event per verb per process");
        let (level, code, detail) = &events[0];
        assert_eq!(
            (level.as_str(), code.as_str()),
            (site_event_levels::WARN, codes::SCHEMA_UNSUPPORTED)
        );
        let detail = detail.as_ref().unwrap();
        assert_eq!(
            detail.get("verb").and_then(Json::as_str),
            Some(t::TABLE_CHANGE)
        );
        assert_eq!(
            detail.get("missing"),
            Some(&Json::Array(vec![Json::String(
                "tablelist.TableStatus".into()
            )]))
        );
    }
    assert_eq!(t.calls_starting("table"), 0);
    assert_eq!(t.nudge.count(), 1);

    t.store.knobs().missing_columns.clear();
    assert_eq!(
        t.exec(&table_change(check, "T9", 3, None)).status,
        CommandStatus::Applied
    );
    assert_eq!(
        t.exec(&table_change(check, "T8", 3, None)).status,
        CommandStatus::Applied
    );
    assert_eq!(
        t.store
            .schema_probes
            .load(std::sync::atomic::Ordering::SeqCst),
        3
    );
}

#[test]
fn every_verb_declares_the_columns_it_touches() {
    for verb in t::ALL {
        assert!(!schema_columns(verb).unwrap().is_empty());
    }
}

#[test]
fn merge_locks_both_checks_in_ascending_order_moves_every_line_and_closes_the_source() {
    let t = T::new();
    let source = t.open("T4", &["CF01", "TEA"]);
    let target = t.open("T5", &["CF01"]);
    let seeded = t.store.calls().len();
    let cmd = t.merge(target, source, None, None);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    let r = multi(&first);
    assert_eq!(r.removed, [source]);
    assert_eq!(r.checks.len(), 1);
    assert_eq!(r.checks[0].check_no, target);
    assert_eq!(r.checks[0].lines.len(), 3);
    assert_eq!(r.checks[0].fingerprint_after, t.seen(target));
    assert!(!t.store.state().orders.checks.contains_key(&(RVC, source)));
    assert_eq!(
        t.store.calls()[seeded..],
        [
            format!("lock {source}"),
            format!("lock {target}"),
            format!("merge {source}->{target}")
        ]
    );
    assert_eq!(replay, first);
}

#[test]
fn merge_into_the_lower_check_still_locks_in_ascending_order() {
    let t = T::new();
    let target = t.open("T4", &["CF01"]);
    let source = t.open("T5", &["TEA"]);
    let seeded = t.store.calls().len();
    assert_eq!(
        t.exec(&t.merge(target, source, None, None)).status,
        CommandStatus::Applied
    );
    assert_eq!(
        t.store.calls()[seeded..],
        [
            format!("lock {target}"),
            format!("lock {source}"),
            format!("merge {source}->{target}")
        ]
    );
}

#[test]
fn merge_of_a_check_with_a_payment_is_refused_without_writing() {
    let t = T::new();
    let target = t.open("T4", &["CF01"]);
    let source = t.open("T5", &["TEA"]);
    t.store.knobs().paid_checks.insert(source);
    refused(
        &t.exec(&t.merge(target, source, None, None)),
        table::BILL_HAS_PAYMENT,
    );
    assert_eq!(t.calls_starting("merge"), 0);
    assert!(t.store.state().orders.checks.contains_key(&(RVC, source)));
    assert_eq!(t.nudge.count(), 4);
}

#[test]
fn merge_against_a_check_pos_win_changed_is_refused_without_writing() {
    let t = T::new();
    let target = t.open("T4", &["CF01"]);
    let source = t.open("T5", &["TEA"]);
    let seen_source = t.seen(source);
    t.exec(&post(source, "CF01", Decimal::ONE, None, None));
    refused(
        &t.exec(&t.merge(target, source, None, Some(&seen_source))),
        reasons::CHECK_CHANGED,
    );
    assert_eq!(t.calls_starting("merge"), 0);
}

#[test]
fn merge_of_a_missing_or_closed_check_is_refused_without_writing() {
    let t = T::new();
    let target = t.open("T4", &["CF01"]);
    refused(
        &t.exec(&t.merge(target, 424_242, None, None)),
        order::CHECK_NOT_FOUND,
    );
    let source = t.open("T5", &["TEA"]);
    t.close(target);
    refused(
        &t.exec(&t.merge(target, source, None, None)),
        order::BILL_NOT_OPEN,
    );
    assert_eq!(t.calls_starting("merge"), 0);
}

#[test]
fn merge_with_an_expected_hash_on_the_command_or_a_wrong_checks_list_is_invalid() {
    let t = T::new();
    let mut with_hash = t.merge(7001, 7002, Some("cs1:aa"), Some("cs1:bb"));
    with_hash.expected_hash = Some("cs1:aa".into());
    let payload = |a: i64, b: i64, list: Vec<OrderCheckExpectation>| OrderCheckMergePayload {
        rvc_no: RVC,
        target_check_no: a,
        source_check_no: b,
        cashier: "clerk-7".into(),
        checks: list,
    };
    let wrong_list = command(
        t::CHECK_MERGE,
        payload(
            7001,
            7002,
            vec![expect(7001, "cs1:aa"), expect(7003, "cs1:bb")],
        )
        .to_element(),
        &key(7001),
        None,
    );
    let same = command(
        t::CHECK_MERGE,
        payload(
            7001,
            7001,
            vec![expect(7001, "cs1:aa"), expect(7001, "cs1:bb")],
        )
        .to_element(),
        &key(7001),
        None,
    );
    for cmd in [with_hash, wrong_list, same] {
        invalid(&t.exec(&cmd));
    }
    assert_eq!(t.store.work_runs(), 0);
}

#[test]
fn move_moves_the_chosen_lines_and_answers_with_both_checks() {
    let t = T::new();
    let from = t.open("T4", &["CF01", "TEA"]);
    let to = t.open("T5", &["CF01"]);
    let seeded = t.store.calls().len();
    let cmd = t.move_cmd(from, to, &[2], None, None);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    let r = multi(&first);
    assert!(r.removed.is_empty());
    let seqs = |n: i64| {
        r.check(n)
            .unwrap()
            .lines
            .iter()
            .map(|l| l.line_seq)
            .collect::<Vec<_>>()
    };
    assert_eq!(seqs(from), [1]);
    assert_eq!(seqs(to), [2, 3]);
    assert_eq!(r.check(from).unwrap().fingerprint_after, t.seen(from));
    assert_eq!(r.check(to).unwrap().fingerprint_after, t.seen(to));
    assert_eq!(
        t.store.calls()[seeded..],
        [
            format!("lock {from}"),
            format!("lock {to}"),
            format!("lock-lines {from}"),
            format!("move {from}->{to} 2")
        ]
    );
    assert_eq!(replay, first);
    assert_eq!(t.nudge.count(), 7);
}

#[test]
fn move_to_a_lower_check_locks_in_ascending_order_and_a_refused_audit_still_moves() {
    let t = T::new();
    let to = t.open("T4", &["CF01"]);
    let from = t.open("T5", &["TEA", "CF01"]);
    let seeded = t.store.calls().len();
    t.store.knobs().move_audit_error = Some(208);

    let r = t.exec(&t.move_cmd(from, to, &[2, 3, 2], None, None));

    assert_eq!(
        r.status,
        CommandStatus::Applied,
        "the legacy MOVE audit is best-effort"
    );
    assert_eq!(
        t.store.calls()[seeded..],
        [
            format!("lock {to}"),
            format!("lock {from}"),
            format!("lock-lines {from}"),
            format!("move {from}->{to} 2,3")
        ]
    );
    let on_to: Vec<i64> = t
        .store
        .state()
        .orders
        .lines
        .keys()
        .filter(|k| k.0 == to)
        .map(|k| k.1)
        .collect();
    assert_eq!(on_to, [1, 2, 3]);
}

#[test]
fn move_onto_the_source_itself_is_refused_before_the_store_is_touched() {
    let t = T::new();
    let check = t.open("T4", &["CF01"]);
    let runs = t.store.work_runs();
    let seen = t.seen(check);
    refused(
        &t.exec(&t.move_cmd(check, check, &[1], Some(&seen), Some(&seen))),
        table::INVALID_MOVE_TARGET,
    );
    assert_eq!(t.store.work_runs(), runs);
}

#[test]
fn move_to_a_missing_or_closed_target_is_an_invalid_target() {
    let t = T::new();
    let from = t.open("T4", &["CF01"]);
    refused(
        &t.exec(&t.move_cmd(from, 424_242, &[1], None, None)),
        table::INVALID_MOVE_TARGET,
    );
    let to = t.open("T5", &[]);
    t.close(to);
    refused(
        &t.exec(&t.move_cmd(from, to, &[1], None, None)),
        table::INVALID_MOVE_TARGET,
    );
    assert_eq!(t.calls_starting("move"), 0);
}

#[test]
fn move_from_a_missing_closed_or_empty_source_is_refused() {
    let t = T::new();
    let to = t.open("T5", &["CF01"]);
    refused(
        &t.exec(&t.move_cmd(424_242, to, &[1], None, None)),
        order::CHECK_NOT_FOUND,
    );
    let closed = t.open("T4", &["TEA"]);
    t.close(closed);
    refused(
        &t.exec(&t.move_cmd(closed, to, &[2], None, None)),
        order::BILL_NOT_OPEN,
    );
    let empty = t.open("T6", &[]);
    refused(
        &t.exec(&t.move_cmd(empty, to, &[1], None, None)),
        order::CHECK_NOT_FOUND,
    );
    assert_eq!(t.calls_starting("move"), 0);
}

#[test]
fn move_of_a_line_not_on_the_source_a_paid_check_or_a_discounted_source_is_refused() {
    let t = T::new();
    let from = t.open("T4", &["CF01"]);
    let to = t.open("T5", &["TEA"]);
    refused(
        &t.exec(&t.move_cmd(from, to, &[1, 2], None, None)),
        order::ITEM_NOT_FOUND,
    );
    t.store.knobs().discounted_checks.insert(from);
    refused(
        &t.exec(&t.move_cmd(from, to, &[1], None, None)),
        table::DISCOUNT_PRESENT,
    );
    t.store.knobs().paid_checks.insert(to);
    refused(
        &t.exec(&t.move_cmd(from, to, &[1], None, None)),
        table::BILL_HAS_PAYMENT,
    );
    assert_eq!(t.calls_starting("move"), 0);
    assert!(t.store.state().orders.lines.contains_key(&(from, 1)));
}

#[test]
fn move_against_a_target_pos_win_changed_is_refused_without_writing() {
    let t = T::new();
    let from = t.open("T4", &["CF01"]);
    let to = t.open("T5", &["TEA"]);
    refused(
        &t.exec(&t.move_cmd(from, to, &[1], None, Some(NEVER_SEEN))),
        reasons::CHECK_CHANGED,
    );
    assert_eq!(t.calls_starting("lock-lines"), 0);
}

#[test]
fn a_multi_check_command_that_fails_after_writing_commits_nothing() {
    let t = T::new();
    let target = t.open("T4", &["CF01"]);
    let source = t.open("T5", &["TEA"]);
    let cmd = t.merge(target, source, None, None);
    t.store.knobs().throw_in_work = Some(Throw::Invalid(
        "The store refused to recalculate check; nothing is committed.".into(),
    ));
    assert!(t.executor().execute(&cmd, &Ctx::background()).is_err());
    assert!(t.store.state().orders.checks.contains_key(&(RVC, source)));
    assert!(!t.store.logged(cmd.command_id));
}

#[test]
fn a_table_verb_denied_by_sql_is_rejected_terminally_as_missing_grants() {
    let t = T::new();
    let from = t.open("T4", &["CF01"]);
    let to = t.open("T5", &["TEA"]);
    let cmd = t.move_cmd(from, to, &[1], None, None);
    t.store.knobs().throw_in_work = Some(Throw::Sql(
        230,
        "The UPDATE permission was denied on the column 'CheckNo' of the object 'tmpTrn'".into(),
    ));
    refused(&t.exec(&cmd), order::SQL_GRANTS_MISSING);
    assert!(!t.store.logged(cmd.command_id));
}

// ---- order.check_cancel_empty (ADR 0144, RT-0775) ----

const CUTOFF: &str = "2026-11-23T04:00:00";
const VIRTUAL_TABLE: &str = "A1234";

fn cancel_empty_cmd(check: i64, table: &str, cutoff: &str, expected: Option<&str>) -> SyncCommand {
    let p = OrderCheckCancelEmptyPayload {
        rvc_no: RVC,
        check_no: check,
        table_no: table.to_owned(),
        cashier: "SML".to_owned(),
        opened_before: cutoff.to_owned(),
    };
    command(t::CHECK_CANCEL_EMPTY, p.to_element(), &key(check), expected)
}

fn cancelled(t: &T) -> bool {
    t.calls_starting("cancel-empty") > 0
}

#[test]
fn a_stale_empty_bill_on_the_named_table_is_cancelled_and_reported_removed() {
    let t = T::new();
    let check = t.open(VIRTUAL_TABLE, &[]);
    let cmd = cancel_empty_cmd(check, VIRTUAL_TABLE, CUTOFF, None);

    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    assert_eq!(multi(&first).removed, vec![check]);
    assert!(!t.store.state().orders.checks.contains_key(&(RVC, check)));
    assert_eq!(replay, first, "a redelivery is answered from the log");
    assert_eq!(t.calls_starting("cancel-empty"), 1);
    assert!(
        t.store
            .calls()
            .contains(&format!("cancel-empty {check} by SML"))
    );
    assert!(
        t.store
            .calls()
            .contains(&format!("facts {check} before {CUTOFF}"))
    );
}

#[test]
fn a_second_command_for_an_already_cancelled_bill_is_rejected_check_not_found() {
    let t = T::new();
    let check = t.open(VIRTUAL_TABLE, &[]);
    assert_eq!(
        t.exec(&cancel_empty_cmd(check, VIRTUAL_TABLE, CUTOFF, None))
            .status,
        CommandStatus::Applied
    );
    refused(
        &t.exec(&cancel_empty_cmd(check, VIRTUAL_TABLE, CUTOFF, None)),
        order::CHECK_NOT_FOUND,
    );
}

#[test]
fn a_bill_that_gained_a_line_is_rejected_bill_not_empty_and_nothing_is_written() {
    let t = T::new();
    let check = t.open(VIRTUAL_TABLE, &["CF01"]);
    refused(
        &t.exec(&cancel_empty_cmd(check, VIRTUAL_TABLE, CUTOFF, None)),
        table::BILL_NOT_EMPTY,
    );
    assert!(!cancelled(&t));
    assert!(t.store.state().orders.checks.contains_key(&(RVC, check)));
}

#[test]
fn a_bill_of_the_current_business_day_is_rejected_bill_not_stale() {
    let t = T::new();
    let check = t.open(VIRTUAL_TABLE, &[]);
    t.store.knobs().fresh_checks.insert(check);
    refused(
        &t.exec(&cancel_empty_cmd(check, VIRTUAL_TABLE, CUTOFF, None)),
        table::BILL_NOT_STALE,
    );
    assert!(!cancelled(&t));
}

#[test]
fn a_bill_on_another_table_is_rejected_table_mismatch() {
    for actual in ["A9999", "T4"] {
        let t = T::new();
        let check = t.open(actual, &[]);
        refused(
            &t.exec(&cancel_empty_cmd(check, VIRTUAL_TABLE, CUTOFF, None)),
            table::TABLE_MISMATCH,
        );
        assert!(!cancelled(&t), "{actual}");
    }
}

#[test]
fn the_table_is_compared_trimmed_and_ignoring_case() {
    let t = T::new();
    let check = t.open(" a1234 ", &[]);
    assert_eq!(
        t.exec(&cancel_empty_cmd(check, "A1234 ", CUTOFF, None))
            .status,
        CommandStatus::Applied
    );
}

#[test]
fn a_closed_unknown_or_paid_check_is_refused_without_a_write() {
    let t = T::new();
    let closed = t.open(VIRTUAL_TABLE, &[]);
    t.close(closed);
    refused(
        &t.exec(&cancel_empty_cmd(closed, VIRTUAL_TABLE, CUTOFF, None)),
        order::BILL_NOT_OPEN,
    );
    refused(
        &t.exec(&cancel_empty_cmd(999_999, VIRTUAL_TABLE, CUTOFF, None)),
        order::CHECK_NOT_FOUND,
    );
    let paid = t.open(VIRTUAL_TABLE, &[]);
    t.store.knobs().paid_checks.insert(paid);
    refused(
        &t.exec(&cancel_empty_cmd(paid, VIRTUAL_TABLE, CUTOFF, None)),
        table::BILL_HAS_PAYMENT,
    );
    assert!(!cancelled(&t));
}

#[test]
fn a_malformed_cutoff_or_an_expected_hash_is_an_invalid_payload() {
    let t = T::new();
    let check = t.open(VIRTUAL_TABLE, &[]);
    invalid(&t.exec(&cancel_empty_cmd(check, VIRTUAL_TABLE, "2026-11-23", None)));
    invalid(&t.exec(&cancel_empty_cmd(
        check,
        VIRTUAL_TABLE,
        "2026-02-30T04:00:00",
        None,
    )));
    invalid(&t.exec(&cancel_empty_cmd(
        check,
        VIRTUAL_TABLE,
        CUTOFF,
        Some("cs1:00"),
    )));
    assert!(!cancelled(&t));
}

#[test]
fn a_cashier_longer_than_the_store_column_is_an_invalid_payload() {
    let t = T::new();
    let check = t.open(VIRTUAL_TABLE, &[]);
    let p = OrderCheckCancelEmptyPayload {
        rvc_no: RVC,
        check_no: check,
        table_no: VIRTUAL_TABLE.to_owned(),
        cashier: "apikey:0123456789abcdef".to_owned(),
        opened_before: CUTOFF.to_owned(),
    };
    invalid(&t.exec(&command(
        t::CHECK_CANCEL_EMPTY,
        p.to_element(),
        &key(check),
        None,
    )));
    assert!(!cancelled(&t));
}
