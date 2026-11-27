//! Exactly-once redelivery of the order verbs on the in-memory fake (B9): the cloud may
//! deliver the same command id again after a lost answer or a lease expiry, and each
//! order verb must then answer from the journal without running its store write again.
//! The open, combo, covers and table verbs replay in `commands_executor.rs`; this file
//! holds the line verbs (`order.item_post`, `order.send`, both `order.item_void`s) and
//! `sync.resnapshot`, which (as in C#) is not journalled but idempotent: a redelivery
//! finds its streams already pending and schedules nothing new.

mod commands_support;

use std::sync::Arc;

use commands_support::{CountingNudge, FakeStore, clock};
use dl_agent_pda_sync::commands::{ResnapshotCommandHandler, SiteCommandExecutor};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, OrderCheckOpenPayload, OrderCheckResult, OrderItemPostPayload,
    OrderItemVoidPayload, OrderSendPayload, SyncCommand, SyncCommandResult, SyncResnapshotPayload,
    SyncResnapshotResult, check_aggregate, order_aggregate, order_command_types as o,
    site_command_types as s,
};
use dl_agent_pda_sync::outbox::SqliteOutbox;
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_json::Value as Json;
use dl_uuid::Uuid;

const RVC: i32 = 3;

fn command(type_: &str, payload: Json, aggregate: &str) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: type_.to_owned(),
        aggregate_key: aggregate.to_owned(),
        expected_hash: None,
        payload,
        created_by: "clerk-7".to_owned(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

struct T {
    store: Arc<FakeStore>,
    nudge: Arc<CountingNudge>,
}

impl T {
    fn exec(&self, c: &SyncCommand) -> SyncCommandResult {
        SiteCommandExecutor::new(self.store.clone(), clock(1_794_729_600), &Logger::none())
            .with_nudge(self.nudge.clone())
            .execute(c, &Ctx::background())
            .expect("the command runs")
    }

    /// Delivers `c` twice: the same answer, one store write, one journal entry.
    fn twice(&self, c: &SyncCommand) -> OrderCheckResult {
        let (runs, calls) = (self.store.work_runs(), self.store.calls().len());
        let first = self.exec(c);
        let replay = self.exec(c);
        assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
        assert_eq!(replay, first, "{}: answered from the journal", c.type_);
        assert_eq!(self.store.work_runs(), runs + 1, "{}: ran once", c.type_);
        assert_eq!(
            self.store.calls().len(),
            calls + 1,
            "{}: one store write",
            c.type_
        );
        assert!(self.store.logged(c.command_id));
        OrderCheckResult::result_from(first.result.as_ref()).unwrap()
    }
}

fn key(check: i64) -> String {
    check_aggregate::key(RVC, check)
}

fn post(check: i64, item: &str) -> SyncCommand {
    let p = OrderItemPostPayload {
        rvc_no: RVC,
        check_no: check,
        cashier: "clerk-7".to_owned(),
        item_code: item.to_owned(),
        qty: Decimal::ONE,
        seat_no: 0,
        price: Decimal::from(50_000),
        note: None,
        open_name: None,
        allow_zero_open_price: false,
    };
    command(o::ITEM_POST, p.to_element(), &key(check))
}

fn void(check: i64, seq: i64, sent: bool) -> SyncCommand {
    let p = OrderItemVoidPayload {
        rvc_no: RVC,
        check_no: check,
        trn_seq: seq,
        cashier: "clerk-7".to_owned(),
        sent,
        reason: sent.then(|| "khách đổi món".to_owned()),
        author_cashier: None,
    };
    command(o::ITEM_VOID, p.to_element(), &key(check))
}

#[test]
fn a_redelivered_post_send_or_void_answers_from_the_journal_without_writing_again() {
    let t = T {
        store: FakeStore::new(),
        nudge: Arc::new(CountingNudge::default()),
    };
    let open = OrderCheckOpenPayload {
        rvc_no: RVC,
        table_no: "T4".to_owned(),
        num_guest: 2,
        cashier: "clerk-7".to_owned(),
    };
    let check = t
        .twice(&command(
            o::CHECK_OPEN,
            open.to_element(),
            &order_aggregate::open(RVC, Uuid::new_v4()),
        ))
        .check_no;

    let coffee = t.twice(&post(check, "CF01"));
    let tea = t.twice(&post(check, "TEA"));
    assert_eq!(
        tea.lines.len(),
        2,
        "the redelivered coffee posted no second line"
    );
    let send = OrderSendPayload {
        rvc_no: RVC,
        check_no: check,
        table_no: None,
        cashier: "clerk-7".to_owned(),
        pda_handled: None,
    };
    let sent = t.twice(&command(o::SEND, send.to_element(), &key(check)));
    assert_eq!(sent.sent_seqs, Some(vec![1, 2]));
    let voided = t.twice(&void(check, coffee.trn_seq.unwrap(), true));
    assert_eq!(voided.lines.len(), 1);
    let third = t.twice(&post(check, "CF01"));
    let removed = t.twice(&void(check, third.trn_seq.unwrap(), false));
    assert_eq!(removed.lines, voided.lines);

    assert_eq!(
        t.store.calls(),
        [
            "open 3 T4 2 clerk-7".to_owned(),
            format!("post {check} CF01 x1"),
            format!("post {check} TEA x1"),
            format!("send {check}"),
            format!("void-sent {check} 1 khách đổi món"),
            format!("post {check} CF01 x1"),
            format!("void-unsent {check} 3"),
        ]
    );
}

#[test]
fn an_applied_line_verb_asks_capture_to_re_read_its_own_check() {
    // RT-1309: the check the command wrote is re-read whatever its signature says.
    let t = T {
        store: FakeStore::new(),
        nudge: Arc::new(CountingNudge::default()),
    };
    let open = OrderCheckOpenPayload {
        rvc_no: RVC,
        table_no: "T6".to_owned(),
        num_guest: 1,
        cashier: "clerk-7".to_owned(),
    };
    let opened = t.exec(&command(
        o::CHECK_OPEN,
        open.to_element(),
        &order_aggregate::open(RVC, Uuid::new_v4()),
    ));
    let check = OrderCheckResult::result_from(opened.result.as_ref())
        .unwrap()
        .check_no;
    assert!(t.nudge.checks().is_empty(), "an open has no check key yet");

    let posted = t.exec(&post(check, "CF01"));

    assert_eq!(posted.status, CommandStatus::Applied, "{:?}", posted.reason);
    assert_eq!(t.nudge.checks(), [check.to_string()]);
    assert_eq!(t.nudge.count(), 2, "both verbs still make the open job due");
}

#[test]
fn a_redelivered_resnapshot_finds_its_streams_pending_and_schedules_nothing_new() {
    let dir = std::env::temp_dir()
        .join("pda-sync-agent-tests")
        .join(Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let outbox =
        Arc::new(SqliteOutbox::open(dir.join("outbox.db"), i64::MAX, &Logger::none()).unwrap());
    let handler = ResnapshotCommandHandler::new(
        outbox.clone(),
        clock(1_794_729_600),
        &Logger::none(),
        SyncCatalog::load().unwrap(),
    );
    let store = FakeStore::new();
    let executor = SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none())
        .with_resnapshot(Arc::new(handler));
    let p = SyncResnapshotPayload {
        streams: vec!["cashierdef".to_owned()],
        reason: "verify mismatch".to_owned(),
    };
    let cmd = command(s::SYNC_RESNAPSHOT, p.to_element(), "sync");
    let result = |r: &SyncCommandResult| {
        assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
        SyncResnapshotResult::from_element(r.result.as_ref().unwrap()).unwrap()
    };

    let first = result(&executor.execute(&cmd, &Ctx::background()).unwrap());
    let again = result(&executor.execute(&cmd, &Ctx::background()).unwrap());

    assert_eq!(first.streams, ["CashierDef"]);
    assert!(!first.full && !first.already_pending);
    assert_eq!(again.streams, first.streams);
    assert!(
        again.already_pending,
        "the redelivery scheduled nothing new"
    );
    assert_eq!(store.work_runs(), 0, "no SQL transaction, no journal row");
    drop(executor);
    drop(outbox);
    std::fs::remove_dir_all(&dir).unwrap();
}
