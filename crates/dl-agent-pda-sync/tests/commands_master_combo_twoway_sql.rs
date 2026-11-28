//! Two-way integration of the master-data group "combo (PkgDef) and its price" (RT-0989, ADR 0160)
//! on a real SQL Server. Cloud -> SMILE_POS goes through the full `master.upsert` executor with the
//! hash the editor read from capture's own source; SMILE_POS -> cloud is what the capture mapper
//! sends (canonical columns + row hash) after a BO/POS edit of `PkgDef` or `ItemDef.isPkg`; the
//! conflict in both orders, the `pda_sync` grants (no DELETE, only the four component columns)
//! and the audit row (`Pda_SyncCommandLog`) are checked. Synthetic rows only. Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each test creates its
//! own `CT_RT0989_*` database and drops it afterwards.

use std::sync::Arc;

use dl_agent_pda_sync::capture::plan;
use dl_agent_pda_sync::capture::{
    CaptureSource, SourceQuery, SqlServerCaptureSource, SqlServerSourceOptions,
};
use dl_agent_pda_sync::commands::master::{KeyValue, MasterEntity, find_entity};
use dl_agent_pda_sync::commands::{ConfigSource, SiteCommandExecutor, SqlSiteCommandStore};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::contracts::mapping::{
    CanonicalRow, CatalogRowMapper, MapperOptions, RawValue,
};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, MasterUpsertResult, SyncCommand, SyncCommandResult, site_command_types,
};
use dl_agent_pda_sync::contracts::{SyncCatalog, WireJson};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");
const MASTER_GRANTS: &str = include_str!("../assets/sql/110_master_grants.sql");

/// The vendor-shaped tables of the one-way combo test, cut out so they are not copied twice.
const COMBO_TEST: &str = include_str!("commands_master_combo_sql.rs");
/// `dbo.Unicode2TCVN` verbatim from CT_SMILE_POS, as the kinds test holds it.
const KINDS_TEST: &str = include_str!("commands_master_kinds_sql.rs");

fn between<'a>(text: &'a str, from: &str, to: &str) -> &'a str {
    let start = text.find(from).unwrap() + from.len();
    let end = start + text[start..].find(to).unwrap();
    &text[start..end]
}

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("ICT", 7 * 60)),
    ))
}

fn item() -> &'static MasterEntity {
    find_entity("item").unwrap()
}

fn line() -> &'static MasterEntity {
    find_entity("combo_line").unwrap()
}

struct Store {
    admin: dl_tds::Config,
    database: String,
}

impl Store {
    fn create(url: &str) -> Store {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let tag = Uuid::new_v4().simple().to_string();
        let s = Store {
            admin: admin.with_database("master"),
            database: format!("CT_RT0989_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(between(COMBO_TEST, "const SCHEMA: &str = \"", "\";\n"));
        s.exec(between(
            KINDS_TEST,
            "const UNICODE2TCVN: &str = r#\"",
            "\"#;",
        ));
        s
    }

    fn exec_in(&self, database: &str, sql: &str) {
        let mut c = dl_tds::Client::connect(&self.admin.clone().with_database(database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn exec(&self, sql: &str) {
        self.exec_in(&self.database, sql);
    }

    fn config(&self) -> dl_tds::Config {
        self.admin.clone().with_database(&self.database)
    }

    /// The first column of the first row of `select`, as text.
    fn value(&self, select: &str) -> String {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c
            .query(&format!("SELECT CONCAT(({select}), N'');"), &[])
            .unwrap_or_else(|e| panic!("{select}: {e}"));
        let v: String = r.rows()[0].get(0).unwrap();
        c.close();
        v
    }

    fn run(
        &self,
        entity: &MasterEntity,
        key: &str,
        fields: &[(&str, &str)],
        expected: Option<&str>,
    ) -> SyncCommandResult {
        self.run_cmd(&upsert(entity, key, fields, expected))
    }

    fn run_cmd(&self, cmd: &SyncCommand) -> SyncCommandResult {
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(source)),
            SystemClock::shared(),
            &Logger::none(),
        )
        .with_mapper(mapper())
        .execute(cmd, &Ctx::background())
        .unwrap()
    }

    /// What the capture sends to the cloud for one row: its own source on the key columns through
    /// the catalog mapper. `None` when the row is absent.
    fn capture(&self, entity: &MasterEntity, key: &str) -> Option<CanonicalRow> {
        let m = mapper();
        let table = m.catalog().find(entity.table).unwrap();
        let source = SqlServerCaptureSource::new(SqlServerSourceOptions::new(self.config()));
        let mut q = SourceQuery::new(entity.table);
        q.exclude_columns = Some(plan::exclude_of(m.catalog(), table));
        q.bytes_columns = table
            .transforms
            .as_ref()
            .and_then(|t| t.tcvn3_decode.clone());
        let key = entity.parse_key(key).unwrap();
        q.where_ = Some(
            entity
                .key
                .iter()
                .zip(&key.values)
                .map(|(part, v)| {
                    let raw = match v {
                        KeyValue::Int(n) => RawValue::I32(*n),
                        KeyValue::Str(s) => RawValue::String(s.clone()),
                    };
                    (part.column.to_owned(), raw)
                })
                .collect(),
        );
        let rows = source.query(&q, &Ctx::background()).unwrap();
        assert!(rows.len() <= 1, "{key}: more than one row");
        rows.first()
            .map(|r| m.canonicalize(entity.table, r).unwrap())
    }

    /// The editor's read: the hash capture would mirror for the row.
    fn hash(&self, entity: &MasterEntity, key: &str) -> String {
        self.capture(entity, key).expect("row exists").hash
    }

    fn log_rows(&self, id: Uuid) -> String {
        self.value(&format!(
            "SELECT COUNT(*) FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{id}'"
        ))
    }

    /// The `pda_sync` database user of the bootstrap (no login), with the master grants.
    fn grant_agent_user(&self) {
        self.exec(
            "CREATE USER [pda_sync] WITHOUT LOGIN; ALTER ROLE db_datareader ADD MEMBER [pda_sync];",
        );
        self.exec(MASTER_GRANTS);
    }

    /// Runs `sql` as `pda_sync`; the error text when SQL Server refuses it.
    fn as_agent(&self, sql: &str) -> Result<(), String> {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        c.batch("EXECUTE AS USER = 'pda_sync';").unwrap();
        let r = c.batch(sql).map_err(|e| e.to_string());
        c.close();
        r.map(|_| ())
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let _ = c.batch(&format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;",
                db = self.database
            ));
            c.close();
        }
    }
}

macro_rules! store {
    () => {{
        let Some(url) = admin_url() else {
            eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
            return;
        };
        Store::create(&url)
    }};
}

fn upsert(
    entity: &MasterEntity,
    key: &str,
    fields: &[(&str, &str)],
    expected: Option<&str>,
) -> SyncCommand {
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
    SyncCommand {
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
    }
}

fn combo_line<'a>(
    qty: &'a str,
    price: &'a str,
    use_item_price: &'a str,
    order: &'a str,
) -> [(&'static str, &'a str); 4] {
    [
        ("IQty", qty),
        ("IPrice", price),
        ("UseItemPrice", use_item_price),
        ("DisplayOrder", order),
    ]
}

fn applied(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

const ITEMS: &str = "INSERT dbo.ItemDef (ICode, IName, PrintName, IGroup, IClass, IActive, Price, OpenPrice, ICurrency, ItemType, IUnit, Status, AllLoc, RVC,
    InTaxCode, isPOSItem, isBOItem, OpenItem, UseRecipe, StockInterface, isPkg, isVoucher)
VALUES ('CB_A', 'Massage', 'Massage', '8', '81', 1, 200000, 0, 'VND', 2, 'LAN', 0, 1, 4, 'VAT10', 1, 1, 0, 0, 0, 0, 0),
       ('CB_B', 'Goi dau', 'Goi dau', '8', '81', 1, 150000, 0, 'VND', 2, 'LAN', 0, 1, 4, 'VAT10', 1, 1, 0, 0, 0, 0, 0),
       ('CB_K', 'Combo', 'Combo', '8', '81', 1, 300000, 0, 'VND', 2, 'LAN', 0, 1, 4, 'VAT10', 1, 1, 0, 0, 0, 1, 0);
INSERT dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder)
VALUES ('CB_K', 'CB_A', 1, 180000, 0, 1), ('CB_K', 'CB_B', 2, 0, 1, 2);";

#[test]
fn cloud_to_pos_create_and_edit_return_the_hash_capture_will_mirror() {
    let db = store!();
    db.exec(ITEMS);
    let head = [
        ("IName", "Combo cloud"),
        ("Price", "480000"),
        ("IGroup", "8"),
        ("IClass", "81"),
        ("IActive", "1"),
        ("OpenPrice", "0"),
        ("isPkg", "1"),
    ];
    let res = applied(&db.run(item(), "CB_CL", &head, None));
    assert_eq!(res.row_hash, db.hash(item(), "CB_CL"), "item create hash");
    for (key, f) in [
        ("CB_CL:CB_A", combo_line("1", "180000", "0", "1")),
        ("CB_CL:CB_B", combo_line("2", "0", "1", "2")),
    ] {
        assert!(db.capture(line(), key).is_none(), "{key}");
        let res = applied(&db.run(line(), key, &f, None));
        assert_eq!(res.row_hash, db.hash(line(), key), "{key}: create hash");
        // A second create of the same key is refused and writes nothing.
        refused(&db.run(line(), key, &f, None), "master_row_exists");
    }
    assert_eq!(
        db.value("SELECT COUNT(*) FROM dbo.PkgDef WHERE PkgCode = 'CB_CL'"),
        "2"
    );
    // Edit with the hash the editor read: the price of one component.
    let seen = db.hash(line(), "CB_CL:CB_A");
    let res = applied(&db.run(line(), "CB_CL:CB_A", &[("IPrice", "170000")], Some(&seen)));
    assert_ne!(res.row_hash, seen);
    assert_eq!(res.row_hash, db.hash(line(), "CB_CL:CB_A"), "edit hash");
    assert_eq!(
        db.value("SELECT IPrice FROM dbo.PkgDef WHERE PkgCode = 'CB_CL' AND ItemCode = 'CB_A'"),
        "170000"
    );
    // The combo price is the header item's Price (hide = IActive), as for any item.
    let seen = db.hash(item(), "CB_CL");
    let res = applied(&db.run(
        item(),
        "CB_CL",
        &[("Price", "450000"), ("IActive", "0")],
        Some(&seen),
    ));
    assert_eq!(res.row_hash, db.hash(item(), "CB_CL"));
    assert_eq!(db.value("SELECT CONCAT(Price, '/', CAST(IActive AS int), '/', CAST(isPkg AS int), '/', LastChangeBy) FROM dbo.ItemDef WHERE ICode = 'CB_CL'"), "450000/0/1/CLOUD");
}

#[test]
fn pos_to_cloud_a_bo_edit_of_a_component_or_the_combo_flag_changes_what_capture_sends() {
    let db = store!();
    db.exec(ITEMS);
    let line_before = db.capture(line(), "CB_K:CB_A").unwrap();
    let item_before = db.capture(item(), "CB_K").unwrap();
    assert_eq!(
        item_before.columns.get("isPkg").and_then(|v| v.as_deref()),
        Some("true"),
        "isPkg reaches the cloud"
    );
    db.exec(
        "UPDATE dbo.PkgDef SET IQty = 3, IPrice = 120000, DisplayOrder = 5 WHERE PkgCode = 'CB_K' AND ItemCode = 'CB_A';
UPDATE dbo.ItemDef SET Price = 280000, LastChangeBy = 'BO', LastChangeTime = GETDATE() WHERE ICode = 'CB_K';",
    );
    let line_now = db.capture(line(), "CB_K:CB_A").unwrap();
    assert_ne!(line_now.hash, line_before.hash, "the cloud sees a new hash");
    assert!(
        line_now
            .columns
            .iter()
            .any(|(c, v)| line_before.columns.get(c) != Some(v))
    );
    assert_ne!(db.hash(item(), "CB_K"), item_before.hash);
    // A sibling component is not touched, and a component the BO adds is captured by its own key.
    assert_eq!(
        db.capture(line(), "CB_K:CB_B").unwrap().hash,
        db.hash(line(), "CB_K:CB_B")
    );
    assert!(db.capture(line(), "CB_K:CB_C").is_none());
    db.exec("INSERT dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder) VALUES ('CB_K', 'CB_C', 1, 5000, 0, 3);");
    assert!(
        db.capture(line(), "CB_K:CB_C").is_some(),
        "a BO-added component is captured"
    );
    // Clearing the combo flag in the BO is seen too.
    db.exec("UPDATE dbo.ItemDef SET isPkg = 0 WHERE ICode = 'CB_K';");
    assert_eq!(
        db.capture(item(), "CB_K")
            .unwrap()
            .columns
            .get("isPkg")
            .and_then(|v| v.as_deref()),
        Some("false")
    );
}

#[test]
fn conflict_store_first_refuses_the_stale_cloud_edit_and_leaves_the_pos_value() {
    let db = store!();
    db.exec(ITEMS);
    let seen_line = db.hash(line(), "CB_K:CB_A");
    let seen_item = db.hash(item(), "CB_K");
    db.exec(
        "UPDATE dbo.PkgDef SET IPrice = 111000 WHERE PkgCode = 'CB_K' AND ItemCode = 'CB_A';
UPDATE dbo.ItemDef SET Price = 270000 WHERE ICode = 'CB_K';",
    );
    let stale_line = upsert(
        line(),
        "CB_K:CB_A",
        &[("IPrice", "95000")],
        Some(&seen_line),
    );
    refused(&db.run_cmd(&stale_line), "master_changed");
    let stale_item = upsert(item(), "CB_K", &[("Price", "999")], Some(&seen_item));
    refused(&db.run_cmd(&stale_item), "master_changed");
    assert_eq!(
        db.value("SELECT IPrice FROM dbo.PkgDef WHERE ItemCode = 'CB_A'"),
        "111000"
    );
    assert_eq!(
        db.value("SELECT Price FROM dbo.ItemDef WHERE ICode = 'CB_K'"),
        "270000"
    );
    assert_eq!(
        db.log_rows(stale_line.command_id),
        "1",
        "the refusal is logged"
    );
    // An edit read after the POS change lands.
    applied(&db.run(
        line(),
        "CB_K:CB_A",
        &[("IPrice", "95000")],
        Some(&db.hash(line(), "CB_K:CB_A")),
    ));
}

#[test]
fn conflict_cloud_first_then_pos_is_not_blocked_and_the_older_cloud_hash_loses() {
    let db = store!();
    db.exec(ITEMS);
    let first = applied(&db.run(
        line(),
        "CB_K:CB_A",
        &[("IQty", "2")],
        Some(&db.hash(line(), "CB_K:CB_A")),
    ));
    assert_eq!(
        db.value("SELECT IQty FROM dbo.PkgDef WHERE ItemCode = 'CB_A'"),
        "2.00"
    );
    // The BO is never blocked by the cloud write.
    db.exec("UPDATE dbo.PkgDef SET IPrice = 777 WHERE PkgCode = 'CB_K' AND ItemCode = 'CB_A';");
    let after_pos = db.hash(line(), "CB_K:CB_A");
    assert_ne!(after_pos, first.row_hash);
    refused(
        &db.run(line(), "CB_K:CB_A", &[("IQty", "4")], Some(&first.row_hash)),
        "master_changed",
    );
    applied(&db.run(line(), "CB_K:CB_A", &[("IQty", "4")], Some(&after_pos)));
    assert_eq!(
        db.value("SELECT CONCAT(IQty, '/', IPrice) FROM dbo.PkgDef WHERE ItemCode = 'CB_A'"),
        "4.00/777",
        "the BO price is kept"
    );
}

#[test]
fn permission_pda_sync_inserts_and_updates_only_the_component_columns_and_can_never_delete() {
    let db = store!();
    db.exec(ITEMS);
    db.grant_agent_user();
    // Allowed: the writable component columns, the combo flag, a new component row.
    db.as_agent("UPDATE dbo.PkgDef SET IQty = IQty, IPrice = IPrice, UseItemPrice = UseItemPrice, DisplayOrder = DisplayOrder")
        .expect("component columns");
    db.as_agent("UPDATE dbo.ItemDef SET isPkg = isPkg")
        .expect("isPkg");
    db.as_agent("INSERT dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder) VALUES ('CB_K', 'CB_Z', 1, 1, 0, 9)")
        .expect("insert a component");
    // Denied: the key columns, DELETE, and a column of the header outside the master list.
    for (what, sql) in [
        ("PkgCode", "UPDATE dbo.PkgDef SET PkgCode = PkgCode"),
        ("ItemCode", "UPDATE dbo.PkgDef SET ItemCode = ItemCode"),
        ("DELETE PkgDef", "DELETE dbo.PkgDef"),
        ("DELETE ItemDef", "DELETE dbo.ItemDef"),
        ("ICurrency", "UPDATE dbo.ItemDef SET ICurrency = ICurrency"),
    ] {
        let r = db.as_agent(sql);
        assert!(
            r.as_ref().is_err_and(|e| e.contains("permission")),
            "{what} must be denied, got {r:?}"
        );
    }
    assert_eq!(
        db.value("SELECT COUNT(*) FROM dbo.PkgDef"),
        "3",
        "nothing deleted, one row inserted"
    );
    db.exec(MASTER_GRANTS); // idempotent
}

#[test]
fn audit_each_command_leaves_one_log_row_and_a_redelivery_does_not_write_again() {
    let db = store!();
    db.exec(ITEMS);
    let cmd = upsert(
        line(),
        "CB_K:CB_A",
        &[("IPrice", "150000")],
        Some(&db.hash(line(), "CB_K:CB_A")),
    );
    assert_eq!(db.log_rows(cmd.command_id), "0");
    let first = db.run_cmd(&cmd);
    applied(&first);
    assert_eq!(db.log_rows(cmd.command_id), "1");
    assert_eq!(
        db.value(&format!(
            "SELECT Status FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{}'",
            cmd.command_id
        )),
        "Applied"
    );
    // The second delivery would be refused (the row moved): equal results prove the log answered it.
    assert_eq!(db.run_cmd(&cmd), first);
    assert_eq!(db.log_rows(cmd.command_id), "1");
    // A field outside the writable list is refused before any transaction: no log row, no write.
    let bad = upsert(
        line(),
        "CB_K:CB_A",
        &[("PkgCode", "X")],
        Some(&db.hash(line(), "CB_K:CB_A")),
    );
    refused(&db.run_cmd(&bad), "master_field_not_allowed");
    assert_eq!(db.log_rows(bad.command_id), "0");
    // A row the store does not have is refused inside the transaction and logged with its reason.
    let missing = upsert(line(), "CB_K:CB_NONE", &[("IQty", "1")], Some("h"));
    refused(&db.run_cmd(&missing), "master_row_not_found");
    assert_eq!(db.log_rows(missing.command_id), "1");
}
