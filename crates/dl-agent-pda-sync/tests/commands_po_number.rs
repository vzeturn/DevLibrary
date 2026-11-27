//! `order.po_number` on the in-memory fake (RT-1413, port of `SiteCommandPoNumberTests`): the
//! guest of an open check goes to `TmpInvoice.PONumber` once, nothing else of the check is
//! written, an empty value clears it, and a missing column, a closed or unknown check and a bad
//! payload are refused.

mod commands_support;

use dl_json::Value as Json;
use dl_uuid::Uuid;
use dl_agent_pda_sync::commands::{SiteCommandExecutor, reasons};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, OrderPoNumberPayload, SyncCommand, SyncCommandResult,
    order_command_reasons as order, table_command_reasons as table, table_command_types as t,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;

use commands_support::{Check, FakeStore, clock};
use std::sync::Arc;

const RVC: i32 = 3;
const CHECK: i64 = 7001;

fn store_with_open_check(status: i32) -> Arc<FakeStore> {
    let store = FakeStore::new();
    store.state().orders.checks.insert(
        (RVC, CHECK),
        Check {
            table_no: "T4".into(),
            guests: 2,
            status,
            children: 0,
        },
    );
    store
}

fn exec(store: &Arc<FakeStore>, c: &SyncCommand) -> SyncCommandResult {
    SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none())
        .execute(c, &Ctx::background())
        .expect("the command runs")
}

fn command(payload: Json) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: t::PO_NUMBER.to_owned(),
        aggregate_key: OrderPoNumberPayload::aggregate_key(RVC, CHECK),
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

fn po(text: &str) -> SyncCommand {
    command(
        OrderPoNumberPayload {
            rvc_no: RVC,
            check_no: CHECK,
            po_number: Some(text.to_owned()),
        }
        .to_element(),
    )
}

/// RT-1425: the heartbeat advertises the verb (the cloud only queues it for an agent that
/// lists it), and every advertised verb reaches a handler rather than `unknown_command_type`.
/// Unlike the PDA Rust agent, this one runs the money verbs too, so they stay advertised.
#[test]
fn the_agent_advertises_po_number_and_runs_every_verb_it_advertises() {
    let advertised = dl_agent_pda_sync::commands::runnable_order_types();
    assert!(advertised.iter().any(|v| v == t::PO_NUMBER));
    assert!(advertised.iter().any(|v| v == t::TABLE_CHANGE));
    let store = store_with_open_check(0);
    for verb in &advertised {
        let c = SyncCommand {
            type_: verb.clone(),
            ..command(stj::parse("{}").unwrap())
        };
        let r = exec(&store, &c);
        assert_ne!(r.reason.as_deref(), Some(reasons::UNKNOWN_TYPE), "{verb}");
    }
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn po_calls(store: &FakeStore) -> Vec<String> {
    store
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("po "))
        .collect()
}

#[test]
fn the_guest_is_written_once_and_nothing_else_of_the_check() {
    let store = store_with_open_check(1);
    let c = po("101-Nguyễn Văn An");

    let first = exec(&store, &c);
    let replay = exec(&store, &c);

    assert_eq!(first.status, CommandStatus::Applied, "{:?}", first.reason);
    assert_eq!(first.result, None);
    assert_eq!(replay, first);
    assert_eq!(store.work_runs(), 1);
    assert_eq!(po_calls(&store), ["po 7001 101-Nguyễn Văn An"]);
    let check = store.state().orders.checks[&(RVC, CHECK)].clone();
    assert_eq!((check.table_no.as_str(), check.guests), ("T4", 2));
}

#[test]
fn an_empty_value_clears_the_column() {
    let store = store_with_open_check(1);
    assert_eq!(exec(&store, &po("")).status, CommandStatus::Applied);
    assert_eq!(po_calls(&store), ["po 7001 "]);
}

#[test]
fn an_unknown_or_closed_check_is_refused_without_a_write() {
    let missing = FakeStore::new();
    refused(&exec(&missing, &po("101-An")), order::CHECK_NOT_FOUND);
    assert!(po_calls(&missing).is_empty());

    let closed = store_with_open_check(2);
    refused(&exec(&closed, &po("101-An")), order::BILL_NOT_OPEN);
    assert!(po_calls(&closed).is_empty());
}

#[test]
fn a_store_without_the_column_refuses_the_verb() {
    let store = store_with_open_check(1);
    store
        .knobs()
        .missing_columns
        .insert("tmpInvoice.PONumber".into());
    refused(&exec(&store, &po("101-An")), table::SCHEMA_UNSUPPORTED);
    assert!(po_calls(&store).is_empty());
}

#[test]
fn a_bad_payload_is_invalid_and_never_reaches_the_store() {
    let long = "x".repeat(51);
    let payloads = [
        r#"{"rvc_no":3,"check_no":7001}"#.to_owned(),
        r#"{"rvc_no":0,"check_no":7001,"po_number":"101-An"}"#.to_owned(),
        r#"{"rvc_no":3,"check_no":0,"po_number":"101-An"}"#.to_owned(),
        format!(r#"{{"rvc_no":3,"check_no":7001,"po_number":"{long}"}}"#),
        r#"{"rvc_no":3,"check_no":7001,"po_number":"101-An\u0007"}"#.to_owned(),
        r#"["not","an","object"]"#.to_owned(),
    ];
    for raw in payloads {
        let store = store_with_open_check(1);
        let r = exec(&store, &command(stj::parse(&raw).unwrap()));
        assert_eq!(r.status, CommandStatus::Rejected, "{raw}");
        assert!(
            r.reason
                .as_deref()
                .unwrap_or("")
                .starts_with(reasons::INVALID_PAYLOAD),
            "{raw}: {:?}",
            r.reason
        );
        assert_eq!(store.work_runs(), 0, "{raw}");
    }
}
