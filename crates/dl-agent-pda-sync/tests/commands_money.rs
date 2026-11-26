//! The money verbs (ADR 0174, RT-1297) through the executor on the hand-written fake (port
//! of the C# `SiteCommandMoneyTests`): a tender answers with the check, a close answers
//! with the closed bill exactly once per command id, an unbalanced close or a bad payload
//! writes nothing.

mod commands_support;

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::bootstrap::{STEPS, StepKind, read_script};
use dl_agent_pda_sync::commands::{SiteCommandExecutor, reasons};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, OrderCheckClosePayload, OrderCheckCloseResult, OrderCheckOpenPayload,
    OrderCheckResult, OrderItemPostPayload, OrderTenderAddPayload, OrderTenderRemovePayload,
    SyncCommand, SyncCommandResult, money_command_reasons as money, money_command_types as m,
    order_aggregate, order_command_types as o,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_decimal::Decimal;
use dl_json::Value as Json;
use dl_uuid::Uuid;
use std::sync::Arc;

const RVC: i32 = 3;

struct T {
    store: Arc<FakeStore>,
}

impl T {
    fn new() -> T {
        T {
            store: FakeStore::new(),
        }
    }

    fn exec(&self, c: &SyncCommand) -> SyncCommandResult {
        SiteCommandExecutor::new(self.store.clone(), clock(1_795_680_000), &Logger::none())
            .execute(c, &Ctx::background())
            .expect("the command runs")
    }

    fn check_with_item(&self, price: i64) -> i64 {
        let open = OrderCheckOpenPayload {
            rvc_no: RVC,
            table_no: "SPA1".into(),
            num_guest: 1,
            cashier: "clerk-7".into(),
        };
        let opened = self.exec(&command(
            o::CHECK_OPEN,
            open.to_element(),
            &order_aggregate::open(RVC, Uuid::new_v4()),
        ));
        let check = OrderCheckResult::result_from(opened.result.as_ref())
            .expect("an opened check")
            .check_no;
        let post = OrderItemPostPayload {
            rvc_no: RVC,
            check_no: check,
            cashier: "clerk-7".into(),
            item_code: "CF01".into(),
            qty: Decimal::ONE,
            seat_no: 0,
            price: Decimal::from(price),
            note: None,
            open_name: None,
            allow_zero_open_price: false,
        };
        let posted = self.exec(&command(o::ITEM_POST, post.to_element(), &key(check)));
        assert_eq!(posted.status, CommandStatus::Applied, "{:?}", posted.reason);
        check
    }

    fn calls_starting(&self, prefix: &str) -> usize {
        self.store
            .calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

fn key(check: i64) -> String {
    format!("check:{RVC}:{check}")
}

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

fn tender(check: i64, amount: i64, payment_type: &str) -> SyncCommand {
    let p = OrderTenderAddPayload {
        rvc_no: RVC,
        check_no: check,
        cashier: "clerk-7".into(),
        payment_type: payment_type.into(),
        amount: Decimal::from(amount),
        tendered: Decimal::from(amount),
        reference: None,
        card_type: None,
        bill_no: None,
    };
    command(m::TENDER_ADD, p.to_element(), &key(check))
}

fn untender(check: i64, seq: i64) -> SyncCommand {
    let p = OrderTenderRemovePayload {
        rvc_no: RVC,
        check_no: check,
        trn_seq: seq,
        cashier: "clerk-7".into(),
    };
    command(m::TENDER_REMOVE, p.to_element(), &key(check))
}

fn close(check: i64) -> SyncCommand {
    let p = OrderCheckClosePayload {
        rvc_no: RVC,
        check_no: check,
        cashier: "clerk-7".into(),
    };
    command(m::CHECK_CLOSE, p.to_element(), &key(check))
}

#[test]
fn a_paid_check_closes_once_and_a_replay_answers_from_the_log() {
    let t = T::new();
    let check = t.check_with_item(250_000);
    let paid = t.exec(&tender(check, 250_000, "CASH"));
    assert_eq!(paid.status, CommandStatus::Applied, "{:?}", paid.reason);
    let seq = OrderCheckResult::result_from(paid.result.as_ref())
        .and_then(|r| r.trn_seq)
        .unwrap();
    assert!(seq > 0);

    let cmd = close(check);
    let first = t.exec(&cmd);
    let replay = t.exec(&cmd);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    let bill = OrderCheckCloseResult::close_result_from(first.result.as_ref()).unwrap();
    assert_eq!(
        (bill.sub_total, bill.total_paid, bill.already_closed),
        (Decimal::from(250_000), Decimal::from(250_000), false)
    );
    assert_eq!(replay, first);
    assert_eq!(t.calls_starting("close"), 1);
}

#[test]
fn an_underpaid_close_is_refused_and_leaves_the_check_open() {
    let t = T::new();
    let check = t.check_with_item(250_000);
    assert_eq!(
        t.exec(&tender(check, 100_000, "CASH")).status,
        CommandStatus::Applied
    );

    let result = t.exec(&close(check));

    assert_eq!(result.status, CommandStatus::Rejected);
    assert_eq!(result.reason.as_deref(), Some(money::UNDERPAID));
    assert_eq!(t.store.state().orders.checks[&(RVC, check)].status, 1);
}

#[test]
fn a_removed_tender_reopens_the_balance() {
    let t = T::new();
    let check = t.check_with_item(80_000);
    let paid = t.exec(&tender(check, 80_000, "CASH"));
    let seq = OrderCheckResult::result_from(paid.result.as_ref())
        .and_then(|r| r.trn_seq)
        .unwrap();

    let removed = t.exec(&untender(check, seq));
    let again = t.exec(&untender(check, seq));

    assert_eq!(
        removed.status,
        CommandStatus::Applied,
        "{:?}",
        removed.reason
    );
    assert_eq!(again.reason.as_deref(), Some(money::TENDER_NOT_FOUND));
    assert_eq!(
        t.exec(&close(check)).reason.as_deref(),
        Some(money::UNDERPAID)
    );
}

#[test]
fn the_money_grant_script_runs_last_and_only_grants_table_writes_to_pda_sync() {
    let step = STEPS.last().unwrap();
    assert_eq!(
        (step.id, step.kind, step.resource_name),
        (
            "money_grants",
            StepKind::MoneyGrants,
            "Sql.180_money_grants.sql"
        )
    );
    // Regex.Replace(sql, "--[^\n]*", "")
    let sql = read_script(step.resource_name)
        .unwrap()
        .split('\n')
        .map(|l| l.find("--").map_or(l, |at| &l[..at]))
        .collect::<Vec<_>>()
        .join("\n");
    let words: Vec<String> = sql
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    for forbidden in ["DROP", "TRUNCATE", "ALTER", "CREATE"] {
        assert!(!words.iter().any(|w| w == forbidden), "{forbidden}");
    }
    assert!(
        !words.windows(2).any(|w| w[0] == "EXECUTE" && w[1] == "AS"),
        "EXECUTE AS"
    );
    assert!(sql.contains("TO [pda_sync]"));
    // \(N'(\w+)',\s+N'(\w+)': the permission of each (table, permission) pair.
    let mut permissions: Vec<&str> = Vec::new();
    for (at, _) in sql.match_indices("(N'") {
        let rest = &sql[at + 3..];
        let Some(end) = rest.find("',") else { continue };
        if !rest[..end].chars().all(|c| c.is_alphanumeric() || c == '_') || end == 0 {
            continue;
        }
        let after = &rest[end + 2..];
        let trimmed = after.trim_start();
        if trimmed.len() == after.len() || !trimmed.starts_with("N'") {
            continue;
        }
        let perm = &trimmed[2..];
        let len = perm
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(perm.len());
        if len > 0 && perm[len..].starts_with('\'') {
            permissions.push(&perm[..len]);
        }
    }
    permissions.sort_unstable();
    permissions.dedup();
    assert_eq!(permissions, ["DELETE", "INSERT", "UPDATE"]);
}

#[test]
fn an_invalid_tender_is_rejected_without_touching_the_store() {
    for (payment_type, amount) in [("TIP", 10_000), ("CASH", 0)] {
        let t = T::new();
        let check = t.check_with_item(10_000);
        let calls = t.store.calls().len();

        let result = t.exec(&tender(check, amount, payment_type));

        assert_eq!(result.status, CommandStatus::Rejected, "{payment_type}");
        assert!(
            result
                .reason
                .as_deref()
                .unwrap_or("")
                .starts_with(reasons::INVALID_PAYLOAD),
            "{:?}",
            result.reason
        );
        assert_eq!(t.store.calls().len(), calls, "{payment_type}");
    }
}
