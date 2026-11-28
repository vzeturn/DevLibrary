//! `master.upsert` for the payment-mode, tax and currency groups (ADR 0151) on the in-memory
//! fake: write-down with a typed decimal, the store-first conflict for each group, the base
//! currency guard, the composite currency key and the strict value rules.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::SiteCommandExecutor;
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue, VendorRow};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, MasterUpsertResult, SyncCommand, SyncCommandResult, site_command_types,
};
use dl_agent_pda_sync::contracts::{SyncCatalog, WireJson};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_decimal::Decimal;
use dl_uuid::Uuid;

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("ICT", 7 * 60)),
    ))
}

fn dec(text: &str) -> RawValue {
    RawValue::Decimal(Decimal::parse_rounded(text, dl_decimal::RoundingMode::HalfEven).unwrap())
}

fn payment_row(pno: i32, desc: &str) -> VendorRow {
    let mut r = VendorRow::new();
    r.insert("PNo", RawValue::I32(pno));
    r.insert("PMode", RawValue::String("CASH".into()));
    r.insert("PDesc", RawValue::String(desc.into()));
    r
}

fn tax_row(key_column: &str, code: i32, rate_column: &str, rate: &str) -> VendorRow {
    let mut r = VendorRow::new();
    r.insert(key_column, RawValue::I32(code));
    r.insert(rate_column, dec(rate));
    r
}

fn currency_row(rvc: i32, code: i32, rate: &str, base: bool) -> VendorRow {
    let mut r = VendorRow::new();
    r.insert("RVC", RawValue::I32(rvc));
    r.insert("CrCode", RawValue::I32(code));
    r.insert("ExRate", dec(rate));
    r.insert("IsBase", RawValue::Bool(base));
    r
}

/// The fake's key text: `rvc:key` for an outlet-keyed entity.
fn key_text(rvc: Option<i32>, key: i32) -> String {
    rvc.map_or(key.to_string(), |r| format!("{r}:{key}"))
}

struct T {
    store: Arc<FakeStore>,
}

impl T {
    fn with(entity: &str, rvc: Option<i32>, key: i32, row: VendorRow) -> T {
        let store = FakeStore::new();
        store
            .state()
            .masters
            .insert((entity.to_owned(), key_text(rvc, key)), row);
        T { store }
    }

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        SiteCommandExecutor::new(self.store.clone(), clock(1_794_729_600), &Logger::none())
            .with_mapper(mapper())
            .execute(c, &Ctx::background())
            .unwrap()
    }

    fn row(&self, entity: &str, rvc: Option<i32>, key: i32) -> VendorRow {
        self.store.state().masters[&(entity.to_owned(), key_text(rvc, key))].clone()
    }

    fn hash(&self, table: &str, entity: &str, rvc: Option<i32>, key: i32) -> String {
        mapper()
            .canonicalize(table, &self.row(entity, rvc, key))
            .unwrap()
            .hash
    }

    /// A POS/BO edit directly in the store, after the editor read.
    fn store_edit(&self, entity: &str, rvc: Option<i32>, key: i32, column: &str, v: RawValue) {
        self.store
            .state()
            .masters
            .get_mut(&(entity.to_owned(), key_text(rvc, key)))
            .unwrap()
            .insert(column, v);
    }
}

fn upsert(entity: &str, key: &str, fields: &str, expected: Option<&str>) -> SyncCommand {
    let expected = expected.map_or(String::new(), |h| format!(r#","expected_hash":"{h}""#));
    let payload = stj::parse(&format!(
        r#"{{"entity":"{entity}","key":"{key}","fields":{fields}{expected},"actor":"office-1"}}"#
    ))
    .unwrap();
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::MASTER_UPSERT.into(),
        aggregate_key: format!("master:{entity}:{key}"),
        expected_hash: None,
        payload,
        created_by: "office-1".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn invalid(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_ne!(r.reason.as_deref(), Some("master_changed"));
}

fn applied(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

fn text(row: &VendorRow, column: &str) -> String {
    match row.get(column).unwrap() {
        RawValue::String(s) => s.clone(),
        RawValue::Decimal(d) => d.to_string(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn payment_mode_description_is_written_and_a_store_edit_first_blocks_a_stale_hash() {
    let t = T::with("payment_mode", None, 3, payment_row(3, "Cash"));
    let seen = t.hash("PaymentMode", "payment_mode", None, 3);
    // A POS edit lands after the editor read: the stale hash must never overwrite it.
    t.store_edit(
        "payment_mode",
        None,
        3,
        "PDesc",
        RawValue::String("POS made this".into()),
    );
    refused(
        &t.run(&upsert(
            "payment_mode",
            "3",
            r#"{"PDesc":"Cloud"}"#,
            Some(&seen),
        )),
        "master_changed",
    );
    assert_eq!(
        text(&t.row("payment_mode", None, 3), "PDesc"),
        "POS made this"
    );
    let fresh = t.hash("PaymentMode", "payment_mode", None, 3);
    let res = applied(&t.run(&upsert(
        "payment_mode",
        "3",
        r#"{"PDesc":"Cloud"}"#,
        Some(&fresh),
    )));
    assert_eq!(text(&t.row("payment_mode", None, 3), "PDesc"), "Cloud");
    assert_eq!(res.row_hash, t.hash("PaymentMode", "payment_mode", None, 3));
    // The code and the posting columns are not writable.
    for field in ["PMode", "TrnCode", "PType"] {
        refused(
            &t.run(&upsert(
                "payment_mode",
                "3",
                &format!(r#"{{"{field}":"X"}}"#),
                Some(&res.row_hash),
            )),
            "master_field_not_allowed",
        );
    }
}

#[test]
fn tax_and_special_tax_rates_are_typed_decimals_and_conflict_checked() {
    for (entity, table, key_col, rate_col) in [
        ("tax_class", "TaxClassDef", "TaxCode", "TaxRate"),
        ("special_tax", "STaxDef", "STaxCode", "STaxRate"),
    ] {
        let t = T::with(entity, None, 2, tax_row(key_col, 2, rate_col, "10"));
        let seen = t.hash(table, entity, None, 2);
        t.store_edit(entity, None, 2, rate_col, dec("8"));
        let field = |v: &str| format!(r#"{{"{rate_col}":"{v}"}}"#);
        refused(
            &t.run(&upsert(entity, "2", &field("5.5"), Some(&seen))),
            "master_changed",
        );
        assert_eq!(text(&t.row(entity, None, 2), rate_col), "8");
        let fresh = t.hash(table, entity, None, 2);
        let res = applied(&t.run(&upsert(entity, "2", &field("5.5"), Some(&fresh))));
        assert_eq!(text(&t.row(entity, None, 2), rate_col), "5.5");
        assert_eq!(res.row_hash, t.hash(table, entity, None, 2));
        // Values the column cannot hold are invalid, never rounded or clamped.
        for bad in [
            "-1", "100.0001", "101", "1e1", " 5", "5,5", "5.12345", "", ".5", "5.", "+5",
        ] {
            invalid(&t.run(&upsert(entity, "2", &field(bad), Some(&res.row_hash))));
        }
        applied(&t.run(&upsert(entity, "2", &field("100"), Some(&res.row_hash))));
        // Update only: no create.
        refused(
            &t.run(&upsert(entity, "9", &field("5"), None)),
            "master_create_unsupported",
        );
    }
}

#[test]
fn currency_uses_the_outlet_and_code_key_and_never_writes_the_base_currency() {
    let t = T::with("currency", Some(1), 2, currency_row(1, 2, "24400", false));
    t.store.state().masters.insert(
        ("currency".into(), "1:1".into()),
        currency_row(1, 1, "1", true),
    );
    t.store.state().masters.insert(
        ("currency".into(), "0:2".into()),
        currency_row(0, 2, "23000", false),
    );
    let seen = t.hash("CurrencyDef", "currency", Some(1), 2);
    t.store_edit("currency", Some(1), 2, "ExRate", dec("24500"));
    refused(
        &t.run(&upsert(
            "currency",
            "1:2",
            r#"{"ExRate":"25000.5"}"#,
            Some(&seen),
        )),
        "master_changed",
    );
    assert_eq!(text(&t.row("currency", Some(1), 2), "ExRate"), "24500");
    let fresh = t.hash("CurrencyDef", "currency", Some(1), 2);
    let res = applied(&t.run(&upsert(
        "currency",
        "1:2",
        r#"{"ExRate":"25000.5"}"#,
        Some(&fresh),
    )));
    assert_eq!(res.key, "1:2");
    assert_eq!(text(&t.row("currency", Some(1), 2), "ExRate"), "25000.5");
    // The other outlet's row of the same CrCode is untouched.
    assert_eq!(text(&t.row("currency", Some(0), 2), "ExRate"), "23000");
    // The base currency is read-only for the cloud, with a matching hash or not.
    let base = t.hash("CurrencyDef", "currency", Some(1), 1);
    refused(
        &t.run(&upsert("currency", "1:1", r#"{"ExRate":"2"}"#, Some(&base))),
        "master_row_protected",
    );
    assert_eq!(text(&t.row("currency", Some(1), 1), "ExRate"), "1");
    // Only the composite key text is a key; a missing outlet or CrCode row is not found.
    for bad in ["2", "1:", ":2", "1:2:3", "a:2", "-1:2", "1: 2"] {
        invalid(&t.run(&upsert("currency", bad, r#"{"ExRate":"1"}"#, Some(&fresh))));
    }
    refused(
        &t.run(&upsert(
            "currency",
            "5:2",
            r#"{"ExRate":"1"}"#,
            Some(&fresh),
        )),
        "master_row_not_found",
    );
    // A single-column entity does not take the composite form.
    invalid(&t.run(&upsert(
        "payment_mode",
        "1:2",
        r#"{"PDesc":"x"}"#,
        Some(&fresh),
    )));
}
