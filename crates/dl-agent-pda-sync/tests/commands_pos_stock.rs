//! `pos.stock_balance` on a hand-written stock source (RT-1024, ADR 0165; port of
//! `PosStockBalanceCommandTests`): the balances come back as the Applied result, a refusal is
//! the same rejection reason `txn.stock_deplete` uses, an invalid payload never reaches the
//! source, a source error propagates and the command writes nothing to the store. The two
//! SELECT texts are pinned in the unit tests of `commands::pos_stock`.

mod commands_support;

use std::sync::{Arc, Mutex};

use commands_support::stock_cmd::{command, d, invalid, refused, result_of};
use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::{
    CommandError, PosStockRead, PosStockSource, Result, SiteCommandExecutor,
};
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, PosStockBalance, PosStockBalanceResult, pos_stock_types,
    txn_stock_reasons as reasons,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;

#[derive(Default)]
struct FakeStock {
    read: Mutex<Option<PosStockRead>>,
    fail: bool,
    asked: Mutex<Vec<(i32, Vec<String>)>>,
}

impl PosStockSource for FakeStock {
    fn read(&self, store_code: i32, item_codes: &[String], _: &Ctx) -> Result<PosStockRead> {
        self.asked
            .lock()
            .unwrap()
            .push((store_code, item_codes.to_vec()));
        if self.fail {
            return Err(CommandError::invalid("cancelled"));
        }
        Ok(self.read.lock().unwrap().clone().unwrap_or_default())
    }
}

fn found() -> PosStockRead {
    PosStockRead {
        refusal: None,
        database: "SMILE_BO".into(),
        store_rvc: 10,
        items: vec![
            PosStockBalance {
                item_code: "A".into(),
                qty: d("12.5"),
            },
            PosStockBalance {
                item_code: "B".into(),
                qty: d("-1"),
            },
        ],
    }
}

fn executor(store: &Arc<FakeStore>, source: Option<Arc<FakeStock>>) -> SiteCommandExecutor {
    let e = SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none());
    match source {
        Some(s) => e.with_stock_balance(s),
        None => e,
    }
}

fn balance(payload: &str) -> dl_agent_pda_sync::contracts::wire::SyncCommand {
    command(pos_stock_types::STOCK_BALANCE, "pos-stock:1", payload)
}

#[test]
fn the_balances_come_back_as_the_applied_result_and_nothing_is_written() {
    let store = FakeStore::new();
    let source = Arc::new(FakeStock::default());
    *source.read.lock().unwrap() = Some(found());
    let e = executor(&store, Some(source.clone()));
    let c = balance(r#"{"store_code":1,"item_codes":[" A ","B","C"],"reason":"stocktake"}"#);
    let r = e.execute(&c, &Ctx::background()).unwrap();
    let res: PosStockBalanceResult = result_of(&r);
    assert_eq!(
        (res.store_code, res.database.as_str(), res.store_rvc),
        (1, "SMILE_BO", 10)
    );
    let items: Vec<_> = res
        .items
        .iter()
        .map(|i| (i.item_code.as_str(), i.qty))
        .collect();
    assert_eq!(items, [("A", d("12.5")), ("B", d("-1"))]);
    // The codes are trimmed before the read.
    assert_eq!(
        source.asked.lock().unwrap().clone(),
        [(1, vec!["A".to_owned(), "B".to_owned(), "C".to_owned()])]
    );
    // A read: no transaction, no log row, no state.
    assert_eq!(store.work_runs(), 0);
    assert!(!store.logged(c.command_id));
}

#[test]
fn a_redelivered_read_reads_again() {
    let store = FakeStore::new();
    let source = Arc::new(FakeStock::default());
    *source.read.lock().unwrap() = Some(found());
    let e = executor(&store, Some(source.clone()));
    let c = balance(r#"{"store_code":1,"item_codes":["A"]}"#);
    e.execute(&c, &Ctx::background()).unwrap();
    e.execute(&c, &Ctx::background()).unwrap();
    assert_eq!(source.asked.lock().unwrap().len(), 2);
}

#[test]
fn a_refused_read_is_the_rejection_with_the_same_reason_as_the_depletion() {
    for reason in [
        reasons::STORE_UNMAPPED,
        reasons::DATABASE_INVALID,
        reasons::LEDGER_MISSING,
    ] {
        let store = FakeStore::new();
        let source = Arc::new(FakeStock::default());
        *source.read.lock().unwrap() = Some(PosStockRead::refused(reason));
        let e = executor(&store, Some(source));
        let r = e
            .execute(
                &balance(r#"{"store_code":1,"item_codes":["A"]}"#),
                &Ctx::background(),
            )
            .unwrap();
        refused(&r, reason);
    }
}

#[test]
fn an_invalid_payload_never_reaches_the_source() {
    let store = FakeStore::new();
    let source = Arc::new(FakeStock::default());
    let e = executor(&store, Some(source.clone()));
    let ctx = Ctx::background();
    invalid(
        &e.execute(&balance(r#"{"store_code":0,"item_codes":["A"]}"#), &ctx)
            .unwrap(),
        "store_code must be 1..255",
    );
    invalid(
        &e.execute(&balance(r#"{"store_code":1,"item_codes":[]}"#), &ctx)
            .unwrap(),
        "item_codes must hold 1..1000 codes",
    );
    invalid(
        &e.execute(&balance(r#"{"store_code":1,"item_codes":["A","a"]}"#), &ctx)
            .unwrap(),
        "an item code is repeated",
    );
    invalid(
        &e.execute(&balance("[]"), &ctx).unwrap(),
        "payload is not a pos.stock_balance object",
    );
    assert!(source.asked.lock().unwrap().is_empty());
}

#[test]
fn a_source_error_propagates() {
    let store = FakeStore::new();
    let source = Arc::new(FakeStock {
        fail: true,
        ..FakeStock::default()
    });
    let e = executor(&store, Some(source));
    let r = e.execute(
        &balance(r#"{"store_code":1,"item_codes":["A"]}"#),
        &Ctx::background(),
    );
    assert!(r.is_err());
}

#[test]
fn without_a_source_the_type_is_unknown() {
    let store = FakeStore::new();
    let e = executor(&store, None);
    let r = e
        .execute(
            &balance(r#"{"store_code":1,"item_codes":["A"]}"#),
            &Ctx::background(),
        )
        .unwrap();
    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some("unknown_command_type"));
}
