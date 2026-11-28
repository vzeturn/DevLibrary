//! `txn.stock_deplete` and `txn.stock_restore` on the in-memory SMILE_BO stock (ADR 0164,
//! RT-1022; port of `SiteCommandStockTests`): the rules. The vendor posting itself is
//! SQL Server's and is exercised by the .NET agent's SQL suite.

mod commands_support;

use commands_support::stock_cmd::{
    command, command_with, d, executor, invalid, refused, result_of, run, run_on, stock_store,
};
use commands_support::{FakeStore, Throw};
use dl_agent_pda_sync::commands::{StockHeldLine, StockVoucher, txn_aggregate_hash};
use dl_agent_pda_sync::config::time::local_offset_minutes;
use dl_agent_pda_sync::contracts::wire::{
    TxnStockResult, order_command_reasons, stock_hash_line, txn_outcomes,
    txn_stock_reasons as reasons, txn_stock_types,
};
use dl_uuid::Uuid;
use std::sync::Arc;

const CHECK: i64 = 9001;

fn key() -> String {
    format!("txn:stock:{CHECK}")
}

fn deplete_payload(version: i64, date: &str, lines: &str) -> String {
    format!(
        r#"{{"rvc":1,"check_no":{CHECK},"version":{version},"business_date":"{date}","lines":{lines},"actor":"cashier-1"}}"#
    )
}

fn deplete(
    version: i64,
    date: &str,
    lines: &str,
) -> dl_agent_pda_sync::contracts::wire::SyncCommand {
    command(
        txn_stock_types::DEPLETE,
        &key(),
        &deplete_payload(version, date, lines),
    )
}

fn restore(version: i64) -> dl_agent_pda_sync::contracts::wire::SyncCommand {
    command(
        txn_stock_types::RESTORE,
        &key(),
        &format!(
            r#"{{"rvc":1,"check_no":{CHECK},"version":{version},"reason":"bill cancelled","actor":"cashier-1"}}"#
        ),
    )
}

fn line(item: &str, store: i32, qty: &str) -> String {
    format!(r#"{{"item_code":"{item}","store_code":{store},"qty":{qty}}}"#)
}

fn lines(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

fn vouchers(store: &FakeStore) -> Vec<StockVoucher> {
    store.state().stock.vouchers.clone()
}

fn held(store: &FakeStore) -> Vec<StockHeldLine> {
    store
        .state()
        .stock
        .held
        .get(&format!("stock:{CHECK}"))
        .cloned()
        .unwrap_or_default()
}

fn untouched(store: &FakeStore) {
    let s = store.state();
    assert!(s.stock.applied.is_empty());
    assert!(s.stock.vouchers.is_empty());
    assert!(s.stock.held.is_empty());
}

#[test]
fn a_new_bill_posts_one_voucher_and_holds_the_lines() {
    let store = stock_store();
    let c = deplete(
        1,
        "2026-11-20",
        &lines(&[line("A", 1, "2"), line("B", 1, "0.5")]),
    );
    let r = run(&store, &c);
    let res: TxnStockResult = result_of(&r);
    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(res.aggregate, "stock:9001");
    assert_eq!((res.version, res.applied_version, res.lines), (1, 1, 2));
    assert_eq!(res.net_qty, d("2.5"));
    assert_eq!(
        res.hash,
        txn_aggregate_hash(vec![
            stock_hash_line("A", 1, d("2")),
            stock_hash_line("B", 1, d("0.5")),
        ])
    );

    let v = vouchers(&store);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].reference, "Cloud bill 9001");
    assert_eq!((v[0].store_rvc, v[0].with_rvc), (10, 20));
    assert_eq!(v[0].trn_sub, StockVoucher::DEPLETION_SUB);
    assert_eq!(v[0].user, "cashier-1");
    let moves: Vec<_> = v[0]
        .lines
        .iter()
        .map(|l| (l.item_code.as_str(), l.trn_qty))
        .collect();
    assert_eq!(moves, [("A", d("-2")), ("B", d("-0.5"))]);
    assert_eq!(held(&store).len(), 2);
    let s = store.state();
    let (version, hash, id, key) = s.stock.applied["stock:9001"].clone();
    assert_eq!((version, hash), (1, res.hash));
    assert_eq!(id, c.command_id);
    assert_eq!(key, Some(501));
}

#[test]
fn a_newer_version_posts_only_the_difference_and_releases_dropped_lines() {
    let store = stock_store();
    run(
        &store,
        &deplete(
            1,
            "2026-11-20",
            &lines(&[line("A", 1, "2"), line("B", 1, "1")]),
        ),
    );
    // A grows by 3, B is dropped, nothing else changes.
    let r = run(
        &store,
        &deplete(2, "2026-11-21", &lines(&[line("A", 1, "5")])),
    );
    let res: TxnStockResult = result_of(&r);
    assert_eq!(
        (res.outcome.as_str(), res.applied_version),
        (txn_outcomes::APPLIED, 2)
    );
    assert_eq!((res.lines, res.net_qty), (1, d("5")));

    let v = vouchers(&store);
    assert_eq!(v.len(), 2);
    let moves: Vec<_> = v[1]
        .lines
        .iter()
        .map(|l| (l.item_code.as_str(), l.trn_qty))
        .collect();
    // A leaves 3 more (negative); the dropped B comes back (positive), on the new day.
    assert_eq!(moves, [("A", d("-3")), ("B", d("1"))]);
    assert_eq!(held(&store).len(), 1);
}

#[test]
fn an_unchanged_recipe_at_a_newer_version_writes_no_voucher() {
    let store = stock_store();
    run(
        &store,
        &deplete(1, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    let r = run(
        &store,
        &deplete(2, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    let res: TxnStockResult = result_of(&r);
    assert_eq!(
        (res.outcome.as_str(), res.applied_version),
        (txn_outcomes::APPLIED, 2)
    );
    assert_eq!(vouchers(&store).len(), 1);
    assert_eq!(store.state().stock.applied["stock:9001"].0, 2);
}

#[test]
fn a_version_at_or_below_the_applied_one_is_stale_and_writes_nothing() {
    let store = stock_store();
    run(
        &store,
        &deplete(3, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    for version in [3, 2] {
        let r = run(
            &store,
            &deplete(version, "2026-11-20", &lines(&[line("A", 1, "9")])),
        );
        let res: TxnStockResult = result_of(&r);
        assert_eq!(res.outcome, txn_outcomes::STALE);
        assert_eq!((res.version, res.applied_version), (version, 3));
        // The answer is the held recipe, not the stale payload.
        assert_eq!(res.net_qty, d("2"));
    }
    assert_eq!(vouchers(&store).len(), 1);
    assert_eq!(held(&store)[0].qty, d("2"));
}

#[test]
fn a_redelivered_command_answers_from_the_log_without_running_again() {
    let store = stock_store();
    let id = Uuid::new_v4();
    let c = command_with(
        id,
        txn_stock_types::DEPLETE,
        &key(),
        &deplete_payload(1, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    let e = executor(&store);
    let first = run_on(&e, &c);
    let again = run_on(&e, &c);
    assert_eq!(first.reason, again.reason);
    assert_eq!(store.work_runs(), 1);
    assert_eq!(vouchers(&store).len(), 1);
}

#[test]
fn two_cloud_stores_booked_into_one_bo_store_share_a_voucher() {
    let store = stock_store();
    run(
        &store,
        &deplete(
            1,
            "2026-11-20",
            &lines(&[line("A", 1, "2"), line("A", 3, "1"), line("A", 2, "4")]),
        ),
    );
    let v = vouchers(&store);
    // Ordered by BO store: 10 (cloud stores 1 and 3, A summed) then 11.
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].store_rvc, 10);
    assert_eq!(v[0].lines.len(), 1);
    assert_eq!(v[0].lines[0].trn_qty, d("-3"));
    assert_eq!((v[1].store_rvc, v[1].lines[0].trn_qty), (11, d("-4")));
    assert_eq!(held(&store).len(), 3);
}

#[test]
fn a_restore_brings_the_held_lines_back_on_the_store_day_and_empties_the_hold() {
    let store = stock_store();
    run(
        &store,
        &deplete(
            1,
            "2026-11-10",
            &lines(&[line("A", 1, "2"), line("B", 1, "1")]),
        ),
    );
    let r = run(&store, &restore(2));
    let res: TxnStockResult = result_of(&r);
    assert_eq!(
        (res.outcome.as_str(), res.lines, res.net_qty),
        (txn_outcomes::APPLIED, 0, d("0"))
    );
    assert_eq!(res.hash, txn_aggregate_hash(Vec::new()));

    let v = vouchers(&store);
    assert_eq!(v.len(), 2);
    let moves: Vec<_> = v[1]
        .lines
        .iter()
        .map(|l| (l.item_code.as_str(), l.trn_qty))
        .collect();
    assert_eq!(moves, [("A", d("2")), ("B", d("1"))]);
    // The store's current day, not the depletion's: the clock's local day.
    let (y, m, day, ..) = commands_support::clock(1_794_729_600)
        .now()
        .to_offset(local_offset_minutes())
        .parts();
    assert_eq!(
        (
            v[1].day.year(),
            u32::from(v[1].day.month().number()),
            u32::from(v[1].day.day())
        ),
        (i32::try_from(y).unwrap(), m, day)
    );
    assert!(held(&store).is_empty());
}

#[test]
fn restoring_a_bill_that_holds_nothing_posts_nothing_but_records_the_version() {
    let store = stock_store();
    let r = run(&store, &restore(1));
    let res: TxnStockResult = result_of(&r);
    assert_eq!(
        (res.outcome.as_str(), res.lines),
        (txn_outcomes::APPLIED, 0)
    );
    assert!(vouchers(&store).is_empty());
    assert_eq!(store.state().stock.applied["stock:9001"].0, 1);
    // Applied at version 1: a replay of version 1 is stale.
    let again: TxnStockResult = result_of(&run(&store, &restore(1)));
    assert_eq!(again.outcome, txn_outcomes::STALE);
}

#[test]
fn the_site_is_checked_once_per_run_not_once_per_command() {
    let store = stock_store();
    let e = executor(&store);
    run_on(&e, &deplete(1, "2026-11-20", &lines(&[line("A", 1, "1")])));
    run_on(&e, &restore(2));
    let probes = store
        .calls()
        .iter()
        .filter(|c| *c == "stock-profile")
        .count();
    assert_eq!(probes, 1);
}

#[test]
fn refusals_roll_back_without_a_log_row_so_a_redrive_runs_again() {
    type Setup = fn(&Arc<FakeStore>);
    let cases: [(&str, &str, Setup); 7] = [
        ("unmapped", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.maps.remove(&1);
        }),
        ("database", reasons::DATABASE_INVALID, |s| {
            s.state().stock.maps.get_mut(&1).unwrap().database = Some("Bad".into());
            s.state().stock.bad_databases.insert("Bad".into());
        }),
        ("ledger", reasons::LEDGER_MISSING, |s| {
            s.state().stock.ledger_ready = false;
        }),
        ("bo store", reasons::STORE_UNMAPPED, |s| {
            s.state().stock.stores.remove(&20);
        }),
        ("cost method", reasons::COST_METHOD_UNSUPPORTED, |s| {
            s.state().stock.settings.cost_method = Some(1);
        }),
        ("period", reasons::PERIOD_CLOSED, |s| {
            s.state().stock.settings.inv_period = Some(202_612);
        }),
        ("item", reasons::ITEM_UNKNOWN, |s| {
            s.state().stock.items.remove("A");
        }),
    ];
    for (name, reason, setup) in cases {
        let store = stock_store();
        setup(&store);
        let c = deplete(1, "2026-11-20", &lines(&[line("A", 1, "2")]));
        let r = run(&store, &c);
        refused(&r, reason);
        assert!(!store.logged(c.command_id), "{name}: logged");
        untouched(&store);
    }
}

#[test]
fn a_redrive_after_the_store_is_fixed_applies() {
    let store = stock_store();
    store.state().stock.items.remove("A");
    let c = deplete(1, "2026-11-20", &lines(&[line("A", 1, "2")]));
    refused(&run(&store, &c), reasons::ITEM_UNKNOWN);
    store.state().stock.items.insert("A".into());
    let r = run(&store, &c);
    assert_eq!(
        result_of::<TxnStockResult>(&r).outcome,
        txn_outcomes::APPLIED
    );
}

#[test]
fn a_period_before_the_gl_period_is_closed_too() {
    let store = stock_store();
    store.state().stock.settings.gl_period = Some(202_612);
    refused(
        &run(
            &store,
            &deplete(1, "2026-11-20", &lines(&[line("A", 1, "2")])),
        ),
        reasons::PERIOD_CLOSED,
    );
}

#[test]
fn a_payload_problem_is_an_invalid_payload_rejection_that_never_reaches_the_store() {
    let store = stock_store();
    let r = run(
        &store,
        &deplete(0, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    invalid(&r, "Version must be positive.");
    let r = run(&store, &deplete(1, "2026-11-20", "[]"));
    assert_eq!(
        r.reason
            .as_deref()
            .map(|s| s.starts_with("invalid_payload: ")),
        Some(true)
    );
    let r = run(&store, &command(txn_stock_types::DEPLETE, &key(), "[]"));
    invalid(&r, "payload is not a txn.stock_deplete object");
    let r = run(&store, &command(txn_stock_types::RESTORE, &key(), "7"));
    invalid(&r, "payload is not a txn.stock_restore object");
    assert_eq!(store.work_runs(), 0);
}

#[test]
fn an_aggregate_key_of_another_bill_is_refused() {
    let store = stock_store();
    let c = command(
        txn_stock_types::DEPLETE,
        "txn:stock:1",
        &deplete_payload(1, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    invalid(&run(&store, &c), "AggregateKey does not match the bill");
    let c = command(
        txn_stock_types::RESTORE,
        "txn:stock:1",
        &format!(r#"{{"rvc":1,"check_no":{CHECK},"version":1,"reason":"x","actor":"a"}}"#),
    );
    invalid(&run(&store, &c), "AggregateKey does not match the bill");
    // An empty key skips the check.
    let c = command(
        txn_stock_types::DEPLETE,
        "",
        &deplete_payload(1, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    assert_eq!(
        result_of::<TxnStockResult>(&run(&store, &c)).outcome,
        txn_outcomes::APPLIED
    );
}

#[test]
fn a_permission_denied_error_is_the_terminal_grants_reject() {
    let store = stock_store();
    store.knobs().throw_in_work = Some(Throw::Sql(229, "permission denied".into()));
    let r = run(
        &store,
        &deplete(1, "2026-11-20", &lines(&[line("A", 1, "2")])),
    );
    refused(&r, order_command_reasons::SQL_GRANTS_MISSING);
    untouched(&store);
}

#[test]
fn the_hash_line_writes_minus_the_held_quantity_to_four_decimals() {
    assert_eq!(stock_hash_line("A", 1, d("2.5")), "A|1|-2.5\n");
    assert_eq!(stock_hash_line("A", 1, d("2.00000")), "A|1|-2\n");
    assert_eq!(stock_hash_line("B", 7, d("0.12345")), "B|7|-0.1235\n");
}
