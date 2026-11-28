//! Shared by the stock verb suites (`commands_stock*.rs`): a store with two mapped cloud
//! stores, the executor, the command builder and the result readers.

use std::sync::Arc;

use dl_agent_pda_sync::commands::{SiteCommandExecutor, StockStoreMap};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{CommandStatus, SyncCommand, SyncCommandResult};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::{FakeStore, clock};

/// The decimal of a literal.
pub fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// A SMILE_BO site: cloud store 1 -> BO store 10 (consumption 20, department D), cloud store 2
/// -> BO store 11 (consumption 21), cloud store 3 -> BO store 10 again; items A and B.
pub fn stock_store() -> Arc<FakeStore> {
    let store = FakeStore::new();
    {
        let mut s = store.state();
        for (code, rvc, with) in [(1, 10, 20), (2, 11, 21), (3, 10, 20)] {
            s.stock.maps.insert(
                code,
                StockStoreMap {
                    store_code: code,
                    database: None,
                    store_rvc: rvc,
                    with_rvc: with,
                    dept_code: Some("D".into()),
                    j_type: Some("SX".into()),
                },
            );
        }
        for (rvc, kind) in [(10, 0), (11, 0), (20, 1), (21, 1)] {
            s.stock.stores.insert(rvc, kind);
        }
        s.stock.items.insert("A".into());
        s.stock.items.insert("B".into());
    }
    store
}

/// A fresh executor over `store` (its ledger caches start empty).
pub fn executor(store: &Arc<FakeStore>) -> SiteCommandExecutor {
    SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none())
}

/// A command of `type_` with a JSON payload; `aggregate_key` empty skips the key check.
pub fn command(type_: &str, aggregate_key: &str, payload: &str) -> SyncCommand {
    command_with(Uuid::new_v4(), type_, aggregate_key, payload)
}

/// [`command`] with a chosen command id.
pub fn command_with(id: Uuid, type_: &str, aggregate_key: &str, payload: &str) -> SyncCommand {
    SyncCommand {
        command_id: id,
        type_: type_.into(),
        aggregate_key: aggregate_key.into(),
        expected_hash: None,
        payload: stj::parse(payload).unwrap(),
        created_by: "cloud".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

/// Runs `c` on a fresh executor.
pub fn run(store: &Arc<FakeStore>, c: &SyncCommand) -> SyncCommandResult {
    run_on(&executor(store), c)
}

/// Runs `c` on `e`.
pub fn run_on(e: &SiteCommandExecutor, c: &SyncCommand) -> SyncCommandResult {
    e.execute(c, &Ctx::background()).unwrap()
}

/// The decoded result of an applied command.
pub fn result_of<T: WireJson>(r: &SyncCommandResult) -> T {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    T::from_element(r.result.as_ref().unwrap()).unwrap()
}

/// The command was rejected with exactly `reason`.
pub fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

/// The command was rejected as an invalid payload with `problem`.
pub fn invalid(r: &SyncCommandResult, problem: &str) {
    refused(r, &format!("invalid_payload: {problem}"));
}
