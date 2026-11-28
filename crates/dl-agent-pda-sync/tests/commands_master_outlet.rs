//! `master.upsert` for the outlet layout and parameters (ADR 0152) on the in-memory fake, a
//! port of the .NET `SiteCommandMasterOutletTests`: create under the absent check, the
//! template rule, partial table edits, the store-first conflict in both directions and the
//! protected parameter names. The SQL itself is in `commands_master_outlet_sql.rs`.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::SiteCommandExecutor;
use dl_agent_pda_sync::commands::master::{
    MasterEntity, OUTLET_PARAM, TABLE, TABLE_AREA, master_reasons,
};
use dl_agent_pda_sync::contracts::SyncCatalog;
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue, VendorRow};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, site_command_types,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("ICT", 7 * 60)),
    ))
}

struct T {
    store: Arc<FakeStore>,
}

impl T {
    fn new() -> T {
        T {
            store: FakeStore::new(),
        }
    }

    fn run(
        &self,
        entity: &MasterEntity,
        key: &str,
        fields: &[(&str, &str)],
        expected: Option<&str>,
    ) -> SyncCommandResult {
        let fields = fields
            .iter()
            .map(|(k, v)| format!("{}:{}", stj::quote(k), stj::quote(v)))
            .collect::<Vec<_>>()
            .join(",");
        let expected = expected.map_or(String::new(), |h| format!(r#","expected_hash":"{h}""#));
        let payload = stj::parse(&format!(
            r#"{{"entity":"{}","key":"{key}","fields":{{{fields}}}{expected},"actor":"admin-1"}}"#,
            entity.wire
        ))
        .unwrap();
        let c = SyncCommand {
            command_id: Uuid::new_v4(),
            type_: site_command_types::MASTER_UPSERT.into(),
            aggregate_key: format!("master:{}:{key}", entity.wire),
            expected_hash: None,
            payload,
            created_by: "admin-1".into(),
            expires_at_utc: None,
            contract_version: 1,
            aggregate_seq: None,
            idempotency_key: None,
            tenant_id: None,
        };
        SiteCommandExecutor::new(self.store.clone(), clock(1_795_939_200), &Logger::none())
            .with_mapper(mapper())
            .execute(&c, &Ctx::background())
            .unwrap()
    }

    fn has(&self, entity: &MasterEntity, key: &str) -> bool {
        self.store
            .state()
            .masters
            .contains_key(&(entity.wire.to_owned(), key.to_owned()))
    }

    fn row(&self, entity: &MasterEntity, key: &str) -> VendorRow {
        self.store.state().masters[&(entity.wire.to_owned(), key.to_owned())].clone()
    }

    fn set(&self, entity: &MasterEntity, key: &str, column: &str, v: RawValue) {
        self.store
            .state()
            .masters
            .get_mut(&(entity.wire.to_owned(), key.to_owned()))
            .unwrap()
            .insert(column, v);
    }

    fn hash(&self, entity: &MasterEntity, key: &str) -> String {
        mapper()
            .canonicalize(entity.table, &self.row(entity, key))
            .unwrap()
            .hash
    }

    fn seed(&self, entity: &MasterEntity, key: &str, row: VendorRow) {
        self.store
            .state()
            .masters
            .insert((entity.wire.to_owned(), key.to_owned()), row);
    }

    fn seed_table(&self, key: &str, description: &str) {
        let (rvc, num) = key.split_once(':').unwrap();
        let mut r = VendorRow::new();
        r.insert("TableNum", RawValue::String(num.into()));
        r.insert("TableType", RawValue::String("T".into()));
        r.insert("TableArea", RawValue::I32(1));
        r.insert("Description", RawValue::String(description.into()));
        r.insert("RVC", RawValue::I32(rvc.parse().unwrap()));
        r.insert("TableStatus", RawValue::I32(0));
        r.insert("IsActive", RawValue::Bool(true));
        self.seed(&TABLE, key, r);
    }

    /// The master unit calls, without the read's column detail.
    fn master_calls(&self) -> Vec<String> {
        self.store
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("master "))
            .map(|c| c.split(" exclude=").next().unwrap().to_owned())
            .collect()
    }
}

/// The table fields that are sent.
fn table<'a>(
    description: Option<&'a str>,
    area: Option<&'a str>,
    active: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    [
        ("Description", description),
        ("TableArea", area),
        ("IsActive", active),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect()
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn rejected(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
}

fn applied(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
}

fn text(row: &VendorRow, column: &str) -> String {
    match row.get(column).unwrap() {
        RawValue::String(s) => s.clone(),
        other => panic!("{column} is {other:?}"),
    }
}

#[test]
fn table_area_is_created_then_renamed_in_unicode_and_a_second_create_is_refused() {
    let t = T::new();
    applied(&t.run(&TABLE_AREA, "4:3", &[("TableAreaName", "Khu vườn")], None));
    assert_eq!(
        text(&t.row(&TABLE_AREA, "4:3"), "TableAreaName"),
        "Khu vườn"
    );
    assert_eq!(
        t.master_calls()[..2],
        [
            "master read-absent table_area 4:3",
            "master insert table_area 4:3"
        ]
    );

    refused(
        &t.run(&TABLE_AREA, "4:3", &[("TableAreaName", "Again")], None),
        master_reasons::ROW_EXISTS,
    );

    let seen = t.hash(&TABLE_AREA, "4:3");
    applied(&t.run(
        &TABLE_AREA,
        "4:3",
        &[("TableAreaName", "Sân thượng")],
        Some(&seen),
    ));
    assert_eq!(
        text(&t.row(&TABLE_AREA, "4:3"), "TableAreaName"),
        "Sân thượng"
    );
}

#[test]
fn table_create_needs_every_field_and_a_template_table() {
    let t = T::new();
    refused(
        &t.run(
            &TABLE,
            "4:A12",
            &table(Some("Bed 12"), Some("1"), Some("1")),
            None,
        ),
        master_reasons::NO_TEMPLATE,
    );
    assert!(!t.has(&TABLE, "4:A12"));

    t.seed_table("4:A1", "Bed 1");
    rejected(&t.run(
        &TABLE,
        "4:A12",
        &table(Some("Bed 12"), None, Some("1")),
        None,
    ));
    assert!(!t.has(&TABLE, "4:A12"));

    applied(&t.run(
        &TABLE,
        "4:A12",
        &table(Some("Bed 12"), Some("2"), Some("true")),
        None,
    ));
    let row = t.row(&TABLE, "4:A12");
    assert_eq!(text(&row, "Description"), "Bed 12");
    assert_eq!(row.get("TableArea"), Some(&RawValue::I32(2)));
    assert_eq!(row.get("IsActive"), Some(&RawValue::Bool(true)));
}

#[test]
fn table_update_writes_only_the_sent_fields_and_keeps_the_rest() {
    let t = T::new();
    t.seed_table("4:A1", "Bed 1");
    let seen = t.hash(&TABLE, "4:A1");
    applied(&t.run(&TABLE, "4:A1", &table(None, None, Some("0")), Some(&seen)));
    let row = t.row(&TABLE, "4:A1");
    assert_eq!(row.get("IsActive"), Some(&RawValue::Bool(false)));
    assert_eq!(text(&row, "Description"), "Bed 1");
    assert_eq!(row.get("TableArea"), Some(&RawValue::I32(1)));
}

#[test]
fn table_conflicts_both_ways_a_stale_cloud_edit_never_overwrites_the_store() {
    let t = T::new();
    t.seed_table("4:A1", "Bed 1");
    let seen = t.hash(&TABLE, "4:A1");

    // The POS opens a bill on the table after the editor read it: the row hash moves and
    // the cloud edit is refused.
    t.set(&TABLE, "4:A1", "TableStatus", RawValue::I32(1));
    refused(
        &t.run(
            &TABLE,
            "4:A1",
            &table(Some("Cloud"), None, None),
            Some(&seen),
        ),
        master_reasons::CHANGED,
    );
    assert_eq!(text(&t.row(&TABLE, "4:A1"), "Description"), "Bed 1");

    // On the fresh hash the cloud edit lands; then a store edit makes that hash stale in turn.
    let fresh = t.hash(&TABLE, "4:A1");
    applied(&t.run(
        &TABLE,
        "4:A1",
        &table(Some("Cloud"), None, None),
        Some(&fresh),
    ));
    assert_eq!(text(&t.row(&TABLE, "4:A1"), "Description"), "Cloud");
    let after_cloud = t.hash(&TABLE, "4:A1");
    t.set(
        &TABLE,
        "4:A1",
        "Description",
        RawValue::String("POS".into()),
    );
    refused(
        &t.run(
            &TABLE,
            "4:A1",
            &table(Some("Cloud again"), None, None),
            Some(&after_cloud),
        ),
        master_reasons::CHANGED,
    );
    assert_eq!(text(&t.row(&TABLE, "4:A1"), "Description"), "POS");
}

#[test]
fn table_values_out_of_the_column_rules_are_refused_before_any_read() {
    for (field, value) in [
        ("Description", "Bàn 1"),
        ("TableArea", "-1"),
        ("TableArea", "1.5"),
        ("IsActive", "yes"),
    ] {
        let t = T::new();
        t.seed_table("4:A1", "Bed 1");
        let seen = t.hash(&TABLE, "4:A1");
        rejected(&t.run(&TABLE, "4:A1", &[(field, value)], Some(&seen)));
        assert!(t.master_calls().is_empty(), "{field}={value}");
    }
}

#[test]
fn malformed_table_keys_are_refused_before_any_read() {
    for key in ["4", "4:", "x:A1", "4:A1 ", "4:A:1"] {
        let t = T::new();
        let r = t.run(&TABLE, key, &table(Some("x"), Some("1"), Some("1")), None);
        rejected(&r);
        assert_eq!(
            r.reason.as_deref(),
            Some("invalid_payload: Key must be <RVC>:<TableNum>"),
            "{key}"
        );
        assert!(t.master_calls().is_empty(), "{key}");
    }
}

#[test]
fn protected_outlet_params_are_never_read_or_written() {
    for name in [
        "LastCheck",
        "lasttrn",
        "CurDate",
        "BillNoLength",
        "InterfaceDB",
        "TrainingMode",
    ] {
        let t = T::new();
        let key = format!("4:{name}");
        for expected in [Some("any"), None] {
            refused(
                &t.run(&OUTLET_PARAM, &key, &[("ParaStr", "1")], expected),
                master_reasons::ROW_PROTECTED,
            );
        }
        assert!(t.master_calls().is_empty(), "{name}");
    }
}

#[test]
fn outlet_param_create_copies_a_known_name_and_refuses_an_unknown_one() {
    let t = T::new();
    refused(
        &t.run(&OUTLET_PARAM, "4:TipPercent", &[("ParaStr", "5")], None),
        master_reasons::NO_TEMPLATE,
    );

    let mut r = VendorRow::new();
    r.insert("ParaName", RawValue::String("TipPercent".into()));
    r.insert("ParaStr", RawValue::String("10".into()));
    r.insert("ParaType", RawValue::I32(1));
    r.insert("RVC", RawValue::I32(1));
    r.insert("Status", RawValue::I32(0));
    t.seed(&OUTLET_PARAM, "1:TipPercent", r);
    applied(&t.run(&OUTLET_PARAM, "4:TipPercent", &[("ParaStr", "5")], None));
    assert_eq!(text(&t.row(&OUTLET_PARAM, "4:TipPercent"), "ParaStr"), "5");

    let seen = t.hash(&OUTLET_PARAM, "4:TipPercent");
    applied(&t.run(
        &OUTLET_PARAM,
        "4:TipPercent",
        &[("ParaStr", "7")],
        Some(&seen),
    ));
    assert_eq!(text(&t.row(&OUTLET_PARAM, "4:TipPercent"), "ParaStr"), "7");
    assert_eq!(text(&t.row(&OUTLET_PARAM, "1:TipPercent"), "ParaStr"), "10");
}

#[test]
fn update_only_entities_refuse_a_create_and_unknown_fields_are_refused() {
    let t = T::new();
    refused(
        &t.run(
            &dl_agent_pda_sync::commands::master::PAYMENT_MODE,
            "3",
            &[("PDesc", "Cash")],
            None,
        ),
        master_reasons::CREATE_UNSUPPORTED,
    );
    refused(
        &t.run(&TABLE, "4:A1", &[("TableStatus", "1")], None),
        master_reasons::FIELD_NOT_ALLOWED,
    );
    // A create of a keeps-missing entity still needs every field.
    let r = t.run(&TABLE, "4:A1", &table(Some("Bed"), Some("1"), None), None);
    rejected(&r);
    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: IsActive is required")
    );
    assert!(t.master_calls().is_empty());
}
