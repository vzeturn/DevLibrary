//! `txn.snapshot` on the in-memory fake (ADR 0159, RT-0849; port of `SiteCommandTxnTests`):
//! the rules. The real SQL Server cases are in `commands_txn_sql.rs`.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::{
    SiteCommandExecutor, TxnLiveColumn, txn_aggregate_hash, txn_aggregate_line,
};
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue, VendorRow};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, TxnSnapshotResult, order_command_reasons,
    txn_command_types, txn_outcomes, txn_reasons,
};
use dl_agent_pda_sync::contracts::{SyncCatalog, WireJson};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

const CHECK: i64 = 9001;

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("UTC", 0)),
    ))
}

fn num(name: &str, nullable: bool, precision: u8, scale: u8) -> TxnLiveColumn {
    TxnLiveColumn {
        nullable,
        ..TxnLiveColumn::new(name, "numeric", 9, precision, scale)
    }
}

fn varchar(name: &str, length: i32) -> TxnLiveColumn {
    TxnLiveColumn {
        nullable: true,
        ..TxnLiveColumn::new(name, "varchar", length, 0, 0)
    }
}

fn store() -> Arc<FakeStore> {
    let store = FakeStore::new();
    let at = TxnLiveColumn {
        nullable: true,
        ..TxnLiveColumn::new("OpenTime", "datetime", 8, 23, 3)
    };
    let mut s = store.state();
    for t in ["TmpInvoice", "Invoice"] {
        s.txn.columns.insert(
            t.into(),
            vec![
                num("CheckNo", false, 18, 0),
                num("SubTotal", true, 16, 2),
                at.clone(),
                varchar("Remark", 50),
            ],
        );
    }
    for t in ["TmpTrn", "Trn"] {
        s.txn.columns.insert(
            t.into(),
            vec![
                num("TrnSeq", false, 18, 0),
                num("CheckNo", true, 18, 0),
                varchar("TrnDesc", 100),
                num("Qty", true, 10, 2),
            ],
        );
    }
    for t in ["TmpPayment", "Payment"] {
        s.txn.columns.insert(
            t.into(),
            vec![
                num("PaymentNo", false, 18, 0),
                num("CheckNo", true, 18, 0),
                num("Amount", true, 16, 2),
            ],
        );
    }
    drop(s);
    store
}

fn executor(store: &Arc<FakeStore>, with_mapper: bool) -> SiteCommandExecutor {
    let e = SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none());
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

/// The payload JSON of a bill: `tables` in scope order, each a JSON array of rows.
fn bill(version: i64, tables: [&str; 6]) -> String {
    let names = [
        "TmpInvoice",
        "TmpTrn",
        "TmpPayment",
        "Invoice",
        "Trn",
        "Payment",
    ];
    let list: Vec<String> = names
        .iter()
        .zip(tables)
        .map(|(n, rows)| format!(r#"{{"table":"{n}","rows":{rows}}}"#))
        .collect();
    format!(
        r#"{{"scope":"bill","scope_key":{CHECK},"version":{version},"tables":[{}],"actor":"cashier-1"}}"#,
        list.join(",")
    )
}

/// An open bill: one header, one line, no payment; the closed tables empty.
fn open_bill(version: i64, remark: &str, sub_total: &str) -> String {
    let header = format!(
        r#"[{{"CheckNo":{CHECK},"SubTotal":{sub_total},"OpenTime":"2026-11-23T08:00:00.123","Remark":"{remark}"}}]"#
    );
    let line = format!(r#"[{{"TrnSeq":1,"CheckNo":{CHECK},"TrnDesc":"Massage 60","Qty":1}}]"#);
    bill(version, [&header, &line, "[]", "[]", "[]", "[]"])
}

fn snapshot(payload: &str) -> SyncCommand {
    snapshot_with(payload, Uuid::new_v4(), &format!("txn:bill:{CHECK}"))
}

fn snapshot_with(payload: &str, id: Uuid, aggregate_key: &str) -> SyncCommand {
    SyncCommand {
        command_id: id,
        type_: txn_command_types::SNAPSHOT.into(),
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

fn result_of(r: &SyncCommandResult) -> TxnSnapshotResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    TxnSnapshotResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn rows(store: &FakeStore, table: &str) -> Vec<VendorRow> {
    store.state().txn.of(table)
}

fn count(res: &TxnSnapshotResult, table: &str) -> i32 {
    res.row_counts.iter().find(|(t, _)| t == table).unwrap().1
}

#[test]
fn a_new_bill_is_written_and_the_result_carries_the_hash_of_the_re_read_rows() {
    let store = store();
    let r = run(&store, &snapshot(&open_bill(1, "table 4", "120000.50")));
    let res = result_of(&r);
    assert_eq!(res.outcome, txn_outcomes::APPLIED);
    assert_eq!(res.aggregate, "bill:9001");
    assert_eq!((res.version, res.applied_version), (1, 1));
    assert_eq!(count(&res, "TmpInvoice"), 1);
    assert_eq!(count(&res, "Invoice"), 0);
    let order: Vec<&str> = res.row_counts.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        order,
        [
            "TmpInvoice",
            "TmpTrn",
            "TmpPayment",
            "Invoice",
            "Trn",
            "Payment"
        ]
    );

    let line = &rows(&store, "TmpTrn")[0];
    // TmpTrn has no TCVN3 column in the catalog.
    assert_eq!(
        line.get("TrnDesc"),
        Some(&RawValue::String("Massage 60".into()))
    );
    let m = mapper();
    let expected = txn_aggregate_hash(vec![
        txn_aggregate_line(
            "TmpInvoice",
            &m.canonicalize("TmpInvoice", &rows(&store, "TmpInvoice")[0])
                .unwrap()
                .hash,
        ),
        txn_aggregate_line("TmpTrn", &m.canonicalize("TmpTrn", line).unwrap().hash),
    ]);
    assert_eq!(res.hash, expected);
    assert_eq!(res.hash.len(), 64);
    assert!(
        res.hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(
        store.state().txn.applied["bill:9001"],
        (1, expected, r.command_id)
    );
}

#[test]
fn closing_the_bill_empties_the_open_tables_and_encodes_the_tcvn3_column_of_the_closed_line() {
    let store = store();
    result_of(&run(
        &store,
        &snapshot(&open_bill(1, "table 4", "120000.50")),
    ));
    let closed = bill(
        2,
        [
            "[]",
            "[]",
            "[]",
            &format!(r#"[{{"CheckNo":{CHECK},"SubTotal":120000.5}}]"#),
            &format!(r#"[{{"TrnSeq":1,"CheckNo":{CHECK},"TrnDesc":"Massage 60"}}]"#),
            &format!(r#"[{{"PaymentNo":1,"CheckNo":{CHECK},"Amount":120000.5}}]"#),
        ],
    );
    let res = result_of(&run(&store, &snapshot(&closed)));
    assert!(rows(&store, "TmpInvoice").is_empty());
    assert!(rows(&store, "TmpTrn").is_empty());
    // Trn.TrnDesc goes through dbo.Unicode2TCVN.
    assert_eq!(
        rows(&store, "Trn")[0].get("TrnDesc"),
        Some(&RawValue::Bytes(b"Massage 60".to_vec()))
    );
    assert_eq!(count(&res, "Payment"), 1);
}

#[test]
fn a_version_at_or_below_the_applied_one_is_stale_writes_nothing_and_returns_the_current_hash() {
    let store = store();
    let applied = result_of(&run(&store, &snapshot(&open_bill(2, "first", "1"))));
    let before = store.calls().len();
    let res = result_of(&run(&store, &snapshot(&open_bill(1, "older", "1"))));
    assert_eq!(res.outcome, txn_outcomes::STALE);
    assert_eq!((res.version, res.applied_version), (1, 2));
    assert_eq!(res.hash, applied.hash);
    assert!(
        store.calls()[before..]
            .iter()
            .all(|c| !c.starts_with("replace:") && !c.starts_with("save:"))
    );
    assert_eq!(
        rows(&store, "TmpInvoice")[0].get("Remark"),
        Some(&RawValue::String("first".into()))
    );
}

#[test]
fn a_column_the_live_table_lacks_is_refused_before_any_write() {
    let store = store();
    let p = bill(
        1,
        [
            "[]",
            &format!(r#"[{{"TrnSeq":1,"CheckNo":{CHECK},"NoSuchColumn":1}}]"#),
            "[]",
            "[]",
            "[]",
            "[]",
        ],
    );
    refused(&run(&store, &snapshot(&p)), txn_reasons::COLUMN_UNKNOWN);
    assert!(
        store
            .calls()
            .iter()
            .all(|c| !c.starts_with("lock:") && !c.starts_with("replace:"))
    );
    assert!(store.state().txn.applied.is_empty());
}

#[test]
fn a_case_insensitive_duplicate_key_is_an_unknown_column() {
    let store = store();
    let p = bill(
        1,
        [
            "[]",
            &format!(r#"[{{"TrnSeq":1,"CheckNo":{CHECK},"TrnDesc":"a","trndesc":"b"}}]"#),
            "[]",
            "[]",
            "[]",
            "[]",
        ],
    );
    refused(&run(&store, &snapshot(&p)), txn_reasons::COLUMN_UNKNOWN);
}

#[test]
fn a_column_name_matches_case_insensitively_and_is_written_under_its_live_name() {
    let store = store();
    let p = bill(
        1,
        [
            "[]",
            &format!(r#"[{{"trnseq":1,"CheckNo":{CHECK},"TRNDESC":"Tea"}}]"#),
            "[]",
            "[]",
            "[]",
            "[]",
        ],
    );
    result_of(&run(&store, &snapshot(&p)));
    let row = &rows(&store, "TmpTrn")[0];
    let keys: Vec<&str> = row.keys().collect();
    assert_eq!(keys, ["TrnSeq", "CheckNo", "TrnDesc"]);
}

#[test]
fn a_value_that_does_not_fit_its_column_is_refused_before_any_write() {
    // A string for a numeric column, more decimals than numeric(16,2) keeps, more integer
    // digits than it holds.
    for json in [r#""12.5""#, "12.345", "123456789012345"] {
        let store = store();
        refused(
            &run(&store, &snapshot(&open_bill(1, "x", json))),
            txn_reasons::VALUE_INVALID,
        );
        assert!(
            store.calls().iter().all(|c| !c.starts_with("replace:")),
            "{json}"
        );
    }
}

#[test]
fn a_missing_not_null_column_without_default_is_a_bad_value() {
    let store = store();
    let p = bill(
        1,
        [
            "[]",
            &format!(r#"[{{"CheckNo":{CHECK}}}]"#),
            "[]",
            "[]",
            "[]",
            "[]",
        ],
    );
    refused(&run(&store, &snapshot(&p)), txn_reasons::VALUE_INVALID);
}

#[test]
fn a_datetime_with_an_offset_or_another_format_is_refused() {
    for at in [
        "2026-11-23T08:00:00+07:00",
        "2026-11-23 08:00:00",
        "23/11/2026",
    ] {
        let store = store();
        let p = bill(
            1,
            [
                &format!(r#"[{{"CheckNo":{CHECK},"OpenTime":"{at}"}}]"#),
                "[]",
                "[]",
                "[]",
                "[]",
                "[]",
            ],
        );
        refused(&run(&store, &snapshot(&p)), txn_reasons::VALUE_INVALID);
    }
}

#[test]
fn a_key_conflict_rejects_and_keeps_the_previous_state() {
    let store = store();
    result_of(&run(&store, &snapshot(&open_bill(1, "kept", "1"))));
    store.knobs().txn_key_conflict = true;
    let r = run(&store, &snapshot(&open_bill(2, "lost", "1")));
    refused(&r, txn_reasons::KEY_CONFLICT);
    // A store-state refusal rolls back with no log row, so a redrive runs again.
    assert!(!store.logged(r.command_id));
    assert_eq!(store.state().txn.applied["bill:9001"].0, 1);
    assert_eq!(
        rows(&store, "TmpInvoice")[0].get("Remark"),
        Some(&RawValue::String("kept".into()))
    );
}

#[test]
fn a_redrive_of_a_refused_command_applies_once_the_store_is_fixed() {
    let store = store();
    let id = Uuid::new_v4();
    let key = format!("txn:bill:{CHECK}");
    store.knobs().txn_key_conflict = true;
    let first = run(
        &store,
        &snapshot_with(&open_bill(1, "redriven", "1"), id, &key),
    );
    refused(&first, txn_reasons::KEY_CONFLICT);

    store.knobs().txn_key_conflict = false;
    let redrive = run(
        &store,
        &snapshot_with(&open_bill(1, "redriven", "1"), id, &key),
    );
    result_of(&redrive);
    assert_eq!(
        store.work_runs(),
        2,
        "the refusal left no log row to answer from"
    );
    assert_eq!(
        rows(&store, "TmpInvoice")[0].get("Remark"),
        Some(&RawValue::String("redriven".into()))
    );
}

#[test]
fn a_replayed_command_id_answers_the_logged_result_without_running_again() {
    let store = store();
    let id = Uuid::new_v4();
    let key = format!("txn:bill:{CHECK}");
    let first = run(&store, &snapshot_with(&open_bill(1, "x", "1"), id, &key));
    let replay = run(&store, &snapshot_with(&open_bill(1, "x", "1"), id, &key));
    assert_eq!(replay, first);
    assert_eq!(store.work_runs(), 1);
}

#[test]
fn a_malformed_payload_or_a_mismatched_aggregate_key_is_invalid() {
    let store = store();
    let no_tables = format!(
        r#"{{"scope":"bill","scope_key":{CHECK},"version":1,"tables":[],"actor":"cashier-1"}}"#
    );
    let r = run(&store, &snapshot(&no_tables));
    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: Tables must list every table of the scope exactly once.")
    );
    let wrong = snapshot_with(&open_bill(1, "x", "1"), Uuid::new_v4(), "txn:bill:1");
    let r = run(&store, &wrong);
    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: AggregateKey does not match Scope and ScopeKey")
    );
    let r = run(&store, &snapshot("[1]"));
    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: payload is not a txn.snapshot object")
    );
    assert_eq!(store.work_runs(), 0);
}

#[test]
fn without_the_catalog_mapper_the_verb_is_unknown() {
    let store = store();
    let r = executor(&store, false)
        .execute(&snapshot(&open_bill(1, "x", "1")), &Ctx::background())
        .unwrap();
    assert_eq!(r.reason.as_deref(), Some("unknown_command_type"));
}

#[test]
fn a_permission_denied_error_on_a_txn_verb_is_the_terminal_grants_reject() {
    use commands_support::Throw;
    let store = store();
    store.knobs().throw_in_work = Some(Throw::Sql(229, "permission denied".into()));
    let r = run(&store, &snapshot(&open_bill(1, "x", "1")));
    refused(&r, order_command_reasons::SQL_GRANTS_MISSING);
    assert!(store.state().txn.applied.is_empty());
}
