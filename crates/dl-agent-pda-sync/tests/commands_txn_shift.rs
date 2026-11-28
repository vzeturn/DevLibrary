//! `txn.shift_open` / `shift_close` / `cash_in` / `cash_out` (ADR 0160, RT-1427; port of
//! `SiteCommandTxnShiftTests`) on the in-memory fake: the rules.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::{
    SiteCommandExecutor, TxnLiveColumn, reasons, txn_aggregate_hash, txn_aggregate_line,
};
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue, VendorRow};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, TxnCashPayload, TxnShiftMoneyLine,
    TxnShiftPayload, TxnShiftResult, txn_command_types, txn_outcomes, txn_reasons, txn_scopes,
};
use dl_agent_pda_sync::contracts::{SyncCatalog, WireJson};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_decimal::Decimal;
use dl_uuid::Uuid;

const SHIFT: i64 = 5501;

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("UTC", 0)),
    ))
}

fn col(name: &str, sql_type: &str, length: i32, precision: u8, scale: u8) -> TxnLiveColumn {
    TxnLiveColumn {
        nullable: true,
        ..TxnLiveColumn::new(name, sql_type, length, precision, scale)
    }
}

fn identity(name: &str) -> TxnLiveColumn {
    TxnLiveColumn {
        nullable: false,
        generated: true,
        ..TxnLiveColumn::new(name, "bigint", 8, 19, 0)
    }
}

fn money(name: &str, scale: u8) -> TxnLiveColumn {
    col(name, "numeric", 9, 18, scale)
}

fn store() -> Arc<FakeStore> {
    let store = FakeStore::new();
    let int = |n: &str| col(n, "int", 4, 10, 0);
    let at = |n: &str| col(n, "datetime", 8, 23, 3);
    let mut s = store.state();
    s.txn.columns.insert(
        "CashierShift".into(),
        vec![
            identity("ShiftNo"),
            int("RVCNo"),
            col("Cashier", "varchar", 50, 0, 0),
            at("StartTime"),
            at("EndTime"),
            at("OpenDate"),
            at("CloseDate"),
            int("ShiftStatus"),
            money("CashOpen", 2),
            money("CashIn", 2),
            money("CashOut", 2),
            money("CashEnd", 2),
            col("ShiftNotes", "varchar", 200, 0, 0),
        ],
    );
    s.txn.columns.insert(
        "ShiftMoney".into(),
        vec![
            int("CrCode"),
            int("RVCNo"),
            at("POSDate"),
            col("ShiftNo", "bigint", 8, 19, 0),
            col("Cashier", "varchar", 50, 0, 0),
            col("NoteName", "varchar", 50, 0, 0),
            int("QTy"),
            money("Amount", 0),
            int("TrnCode"),
            at("TrnTime"),
            identity("SeqNo"),
        ],
    );
    s.txn.columns.insert(
        "DailyCashLog".into(),
        vec![
            identity("TrnSeq"),
            at("TrnDate"),
            col("TrnDesc", "varchar", 200, 0, 0),
            money("TrnAmount", 2),
            col("PostdBy", "varchar", 50, 0, 0),
            at("PostTime"),
        ],
    );
    drop(s);
    store
}

fn executor(store: &Arc<FakeStore>, with_mapper: bool) -> SiteCommandExecutor {
    let e = SiteCommandExecutor::new(store.clone(), clock(1_795_507_200), &Logger::none());
    if with_mapper {
        e.with_mapper(mapper())
    } else {
        e
    }
}

fn run(store: &Arc<FakeStore>, c: &SyncCommand) -> SyncCommandResult {
    executor(store, true)
        .execute(c, &Ctx::background())
        .unwrap()
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn line(amount: i64, trn_code: i32, at: &str) -> TxnShiftMoneyLine {
    TxnShiftMoneyLine {
        cr_code: 1,
        note_name: "500000".into(),
        qty: 4,
        amount,
        trn_code,
        trn_time: at.into(),
    }
}

fn open(version: i64, notes: Option<&str>) -> TxnShiftPayload {
    TxnShiftPayload {
        shift_key: SHIFT,
        version,
        rvc: 4,
        cashier: "cashier-1".into(),
        start_time: "2026-11-24T08:00:00".into(),
        open_date: "2026-11-24T00:00:00".into(),
        shift_status: 1,
        cash_open: d("2000000"),
        cash_in: Decimal::ZERO,
        cash_out: Decimal::ZERO,
        cash_end: None,
        end_time: None,
        close_date: None,
        notes: notes.map(str::to_owned),
        money: Some(vec![line(2_000_000, 1, "2026-11-24T08:00:00")]),
        actor: "cashier-1".into(),
    }
}

fn close(version: i64, cash_in: &str, cash_out: &str) -> TxnShiftPayload {
    let (cash_in, cash_out) = (d(cash_in), d(cash_out));
    TxnShiftPayload {
        shift_status: 2,
        cash_in,
        cash_out,
        cash_end: Some(d("2000000") + cash_in - cash_out),
        end_time: Some("2026-11-24T17:00:00".into()),
        close_date: Some("2026-11-24T00:00:00".into()),
        money: Some(vec![
            line(2_000_000, 1, "2026-11-24T08:00:00"),
            line(2_200_000, 2, "2026-11-24T17:00:00"),
        ]),
        ..open(version, Some("Ca sáng"))
    }
}

fn cash(
    version: i64,
    movement: i64,
    amount: &str,
    total_in: &str,
    total_out: &str,
    reason: &str,
) -> TxnCashPayload {
    TxnCashPayload {
        shift_key: SHIFT,
        version,
        movement_key: movement,
        rvc: 4,
        cashier: "cashier-1".into(),
        amount: d(amount),
        reason: reason.into(),
        business_date: "2026-11-24T00:00:00".into(),
        at: "2026-11-24T10:30:15".into(),
        shift_cash_in: d(total_in),
        shift_cash_out: d(total_out),
        actor: "cashier-1".into(),
    }
}

fn verb(type_: &str, json: &str, id: Option<Uuid>) -> SyncCommand {
    SyncCommand {
        command_id: id.unwrap_or_else(Uuid::new_v4),
        type_: type_.into(),
        aggregate_key: txn_scopes::aggregate_key(txn_scopes::SHIFT, SHIFT),
        expected_hash: None,
        payload: stj::parse(json).unwrap(),
        created_by: "cloud".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn open_cmd(p: &TxnShiftPayload) -> SyncCommand {
    verb(txn_command_types::SHIFT_OPEN, &p.to_json(), None)
}

fn close_cmd(p: &TxnShiftPayload) -> SyncCommand {
    verb(txn_command_types::SHIFT_CLOSE, &p.to_json(), None)
}

fn cash_in(p: &TxnCashPayload, id: Option<Uuid>) -> SyncCommand {
    verb(txn_command_types::CASH_IN, &p.to_json(), id)
}

fn cash_out(p: &TxnCashPayload, id: Option<Uuid>) -> SyncCommand {
    verb(txn_command_types::CASH_OUT, &p.to_json(), id)
}

fn body(r: &SyncCommandResult) -> TxnShiftResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    TxnShiftResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

fn rows(store: &FakeStore, table: &str) -> Vec<VendorRow> {
    store.state().txn.of(table)
}

fn count(res: &TxnShiftResult, table: &str) -> i32 {
    res.row_counts.iter().find(|(t, _)| t == table).unwrap().1
}

fn dec(row: &VendorRow, column: &str) -> Decimal {
    match row.get(column) {
        Some(RawValue::Decimal(v)) => *v,
        other => panic!("{column}: {other:?}"),
    }
}

fn int(row: &VendorRow, column: &str) -> i64 {
    match row.get(column) {
        Some(RawValue::I64(v)) => *v,
        Some(RawValue::I32(v)) => i64::from(*v),
        other => panic!("{column}: {other:?}"),
    }
}

fn is_null(row: &VendorRow, column: &str) -> bool {
    matches!(row.get(column), None | Some(RawValue::Null))
}

/// The aggregate hash the way the executor and capture compute it: every live column,
/// absent ones null.
fn expected_shift_hash(store: &FakeStore, shift_no: i64) -> String {
    let m = mapper();
    let s = store.state();
    let mut lines = Vec::new();
    for table in txn_scopes::SHIFT_TABLES {
        for row in s.txn.of(table) {
            if int(&row, "ShiftNo") != shift_no {
                continue;
            }
            let mut full = VendorRow::new();
            for c in &s.txn.columns[table] {
                full.insert(
                    c.name.clone(),
                    row.get(&c.name).cloned().unwrap_or(RawValue::Null),
                );
            }
            lines.push(txn_aggregate_line(
                table,
                &m.canonicalize(table, &full).unwrap().hash,
            ));
        }
    }
    txn_aggregate_hash(lines)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn opening_a_shift_inserts_the_header_reads_the_identity_back_and_writes_the_counted_lines_under_it()
 {
    let store = store();
    let res = body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));

    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(res.aggregate, "shift:5501");
    assert_eq!(res.shift_no, 1, "the store, not the cloud, chose ShiftNo");
    assert_eq!(count(&res, "CashierShift"), 1);
    assert_eq!(count(&res, "ShiftMoney"), 1);
    assert_eq!(store.state().txn.store_keys["shift:5501"], 1);
    let header = &rows(&store, "CashierShift")[0];
    assert_eq!(rows(&store, "CashierShift").len(), 1);
    assert_eq!(int(header, "ShiftNo"), 1);
    assert_eq!(int(header, "RVCNo"), 4);
    assert_eq!(int(header, "ShiftStatus"), 1);
    assert_eq!(dec(header, "CashOpen"), d("2000000"));
    assert!(is_null(header, "EndTime") && is_null(header, "CashEnd"));
    assert_eq!(
        header.get("ShiftNotes"),
        Some(&RawValue::String("Ca sáng".into())),
        "no TCVN3 column on CashierShift: the text is written as sent"
    );
    let money_rows = rows(&store, "ShiftMoney");
    assert_eq!(money_rows.len(), 1);
    assert_eq!(
        int(&money_rows[0], "ShiftNo"),
        1,
        "bound to the header's identity"
    );
    assert_eq!(dec(&money_rows[0], "Amount"), d("2000000"));
    assert_eq!(res.hash, expected_shift_hash(&store, 1));
    assert!(is_hex64(&res.hash));
    assert_eq!(store.state().txn.applied["shift:5501"].0, 1);
}

#[test]
fn cash_in_and_out_append_signed_cash_log_rows_set_the_running_totals_and_answer_the_identity_and_row_hash()
 {
    let store = store();
    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));

    let r_in = body(&run(
        &store,
        &cash_in(&cash(2, 71, "300000", "300000", "0", "Thu thêm"), None),
    ));
    let r_out = body(&run(
        &store,
        &cash_out(&cash(3, 72, "100000", "300000", "100000", "Mua nước"), None),
    ));

    let log = rows(&store, "DailyCashLog");
    assert_eq!(log.len(), 2);
    assert_eq!(dec(&log[0], "TrnAmount"), d("300000"));
    assert_eq!(
        dec(&log[1], "TrnAmount"),
        d("-100000"),
        "cash out is stored negative"
    );
    assert_eq!(
        log[0].get("TrnDesc"),
        Some(&RawValue::String("Thu thêm".into()))
    );
    assert!(!is_null(&log[0], "PostdBy"));
    let header = &rows(&store, "CashierShift")[0];
    assert_eq!(dec(header, "CashIn"), d("300000"));
    assert_eq!(dec(header, "CashOut"), d("100000"));
    assert_eq!(r_in.trn_seq, Some(1));
    assert_eq!(r_out.trn_seq, Some(2));
    let (h_in, h_out) = (r_in.cash_log_hash.unwrap(), r_out.cash_log_hash.unwrap());
    assert!(is_hex64(&h_out));
    assert_ne!(h_in, h_out);
    assert_eq!(r_out.hash, expected_shift_hash(&store, 1));
    assert_eq!(store.state().txn.applied["shift:5501"].0, 3);
}

#[test]
fn closing_updates_the_same_header_row_and_replaces_the_counted_lines() {
    let store = store();
    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));
    body(&run(
        &store,
        &cash_in(&cash(2, 71, "300000", "300000", "0", "Mua nước"), None),
    ));

    let res = body(&run(&store, &close_cmd(&close(3, "300000", "0"))));

    let headers = rows(&store, "CashierShift");
    assert_eq!(headers.len(), 1, "close updates the row open inserted");
    let header = &headers[0];
    assert_eq!(int(header, "ShiftStatus"), 2);
    assert_eq!(dec(header, "CashEnd"), d("2300000"));
    assert!(!is_null(header, "EndTime") && !is_null(header, "CloseDate"));
    assert_ne!(header.get("EndTime"), header.get("StartTime"));
    assert_eq!(dec(header, "CashOpen"), d("2000000"));
    assert_eq!(int(header, "ShiftNo"), 1, "the identity never changes");
    let lines = rows(&store, "ShiftMoney");
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|r| int(r, "ShiftNo") == 1));
    assert_eq!(count(&res, "ShiftMoney"), 2);
    assert_eq!(res.hash, expected_shift_hash(&store, 1));
    assert_eq!(
        rows(&store, "DailyCashLog").len(),
        1,
        "close does not rewrite the cash row"
    );
}

#[test]
fn close_without_a_prior_open_inserts_the_whole_closed_shift_because_it_carries_the_full_state() {
    let store = store();
    body(&run(&store, &close_cmd(&close(2, "300000", "100000"))));

    let header = &rows(&store, "CashierShift")[0];
    assert!(!is_null(header, "Cashier"));
    assert!(!is_null(header, "StartTime"));
    assert_eq!(int(header, "ShiftStatus"), 2);
}

#[test]
fn a_version_at_or_below_the_applied_one_is_stale_writes_nothing_and_returns_the_current_hash() {
    let store = store();
    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));
    let applied = body(&run(
        &store,
        &cash_in(&cash(2, 71, "300000", "300000", "0", "Mua nước"), None),
    ));
    let before = store.calls().len();

    let again = body(&run(
        &store,
        &cash_in(&cash(2, 71, "300000", "300000", "0", "Mua nước"), None),
    ));
    let older = body(&run(&store, &open_cmd(&open(1, Some("older")))));

    for stale in [again, older] {
        assert_eq!(stale.outcome, txn_outcomes::STALE);
        assert_eq!(stale.applied_version, 2);
        assert_eq!(stale.shift_no, 1);
        assert_eq!(stale.hash, applied.hash);
        assert_eq!(stale.trn_seq, None, "a stale answer wrote no cash row");
    }
    let writes: Vec<String> = store.calls()[before..]
        .iter()
        .filter(|c| {
            ["insert:", "update:", "replace:", "save:"]
                .iter()
                .any(|p| c.starts_with(p))
        })
        .cloned()
        .collect();
    assert!(writes.is_empty(), "{writes:?}");
    assert_eq!(
        rows(&store, "DailyCashLog").len(),
        1,
        "a replayed cash_in never duplicates"
    );
}

#[test]
fn a_replayed_command_id_answers_the_logged_result_without_running_again() {
    let store = store();
    let id = Uuid::new_v4();
    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));
    let p = cash(2, 72, "50000", "0", "50000", "Mua nước");
    let first = run(&store, &cash_out(&p, Some(id)));
    let replay = run(&store, &cash_out(&p, Some(id)));

    assert_eq!(replay.result, first.result);
    assert_eq!(store.work_runs(), 2);
    assert_eq!(rows(&store, "DailyCashLog").len(), 1);
}

#[test]
fn a_cash_movement_of_a_shift_the_store_does_not_have_is_refused_with_no_log_row_and_a_redrive_applies_after_the_open()
 {
    let store = store();
    let id = Uuid::new_v4();
    let p = cash(2, 71, "100000", "100000", "0", "Mua nước");
    let refused = run(&store, &cash_in(&p, Some(id)));

    assert_eq!(refused.status, CommandStatus::Rejected);
    assert_eq!(refused.reason.as_deref(), Some(txn_reasons::SHIFT_UNKNOWN));
    assert!(
        !store.logged(id),
        "a store-state refusal rolls back with no log row"
    );
    assert!(rows(&store, "DailyCashLog").is_empty());
    assert!(store.state().txn.applied.is_empty());

    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));
    body(&run(&store, &cash_in(&p, Some(id))));
    assert_eq!(rows(&store, "DailyCashLog").len(), 1);
}

#[test]
fn a_value_that_does_not_fit_the_live_column_is_refused_before_any_write() {
    let store = store();
    for c in store.state().txn.columns.get_mut("CashierShift").unwrap() {
        if c.name == "ShiftNotes" {
            c.max_length = 5;
        }
    }
    let r = run(
        &store,
        &open_cmd(&open(1, Some("far too long for varchar(5)"))),
    );

    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some(txn_reasons::VALUE_INVALID));
    let calls = store.calls();
    assert!(
        !calls.iter().any(|c| c.starts_with("lock:")
            || c.starts_with("insert:")
            || c.starts_with("replace:")),
        "{calls:?}"
    );
    assert!(store.state().txn.applied.is_empty());
}

#[test]
fn a_live_table_without_an_expected_column_is_refused_as_column_unknown() {
    let store = store();
    store
        .state()
        .txn
        .columns
        .get_mut("DailyCashLog")
        .unwrap()
        .retain(|c| c.name != "PostdBy");
    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));

    let r = run(
        &store,
        &cash_in(&cash(2, 71, "100000", "100000", "0", "Mua nước"), None),
    );

    assert_eq!(r.reason.as_deref(), Some(txn_reasons::COLUMN_UNKNOWN));
    assert!(rows(&store, "DailyCashLog").is_empty());
}

#[test]
fn a_not_null_column_the_verb_does_not_fill_is_refused_not_guessed() {
    let store = store();
    store
        .state()
        .txn
        .columns
        .get_mut("DailyCashLog")
        .unwrap()
        .push(TxnLiveColumn {
            nullable: false,
            ..TxnLiveColumn::new("CashAudit", "bit", 1, 1, 0)
        });
    body(&run(&store, &open_cmd(&open(1, Some("Ca sáng")))));

    let r = run(
        &store,
        &cash_in(&cash(2, 71, "100000", "100000", "0", "Mua nước"), None),
    );

    assert_eq!(r.reason.as_deref(), Some(txn_reasons::VALUE_INVALID));
    assert!(rows(&store, "DailyCashLog").is_empty());
}

#[test]
fn a_malformed_payload_or_a_mismatched_aggregate_key_is_invalid_and_runs_no_work() {
    let store = store();
    let invalid = |c: &SyncCommand| {
        let r = run(&store, c);
        let reason = r.reason.unwrap_or_default();
        assert!(reason.starts_with(reasons::INVALID_PAYLOAD), "{reason}");
    };
    invalid(&close_cmd(&TxnShiftPayload {
        end_time: None,
        ..close(2, "300000", "100000")
    }));
    invalid(&open_cmd(&TxnShiftPayload {
        cash_end: Some(d("5")),
        ..open(1, None)
    }));
    invalid(&cash_in(&cash(2, 71, "0", "0", "0", "Mua nước"), None));
    invalid(&SyncCommand {
        aggregate_key: "txn:shift:1".into(),
        ..open_cmd(&open(1, None))
    });
    invalid(&SyncCommand {
        type_: txn_command_types::CASH_IN.into(),
        ..open_cmd(&open(1, None))
    });
    assert_eq!(store.work_runs(), 0);
}

#[test]
fn without_the_catalog_mapper_the_verbs_are_unknown() {
    let store = store();
    let e = executor(&store, false);
    for c in [
        open_cmd(&open(1, None)),
        cash_in(&cash(2, 71, "1", "1", "0", "Mua nước"), None),
    ] {
        let r = e.execute(&c, &Ctx::background()).unwrap();
        assert_eq!(r.reason.as_deref(), Some(reasons::UNKNOWN_TYPE));
    }
}
