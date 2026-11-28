//! Combos written from the cloud (RT-0979, ADR 0160) on a real SQL Server, a port of the .NET
//! `SqlMasterComboIntegrationTests`: the combo is an item with `isPkg = 1` and its components
//! `PkgDef` rows. What the POS sells is read with the component query of the vendor
//! `PostItem` proc, so a cloud-made combo is checked to split exactly like a BO-made twin. A
//! component needs a combo header and an existing item; a BO edit after the editor read wins
//! over the stale cloud edit; `pda_sync` may insert and update only the four columns. Gated
//! on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each test
//! creates its own `CT_RT0979_RS_*` database and drops it afterwards.

use std::sync::Arc;

use dl_agent_pda_sync::capture::plan;
use dl_agent_pda_sync::capture::{
    CaptureSource, SourceQuery, SqlServerCaptureSource, SqlServerSourceOptions,
};
use dl_agent_pda_sync::commands::master::{KeyValue, MasterEntity, find_entity, master_reasons};
use dl_agent_pda_sync::commands::{ConfigSource, SiteCommandExecutor, SqlSiteCommandStore};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::contracts::SyncCatalog;
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue};
use dl_agent_pda_sync::contracts::time::TimeZone;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, site_command_types,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");
const MASTER_GRANTS: &str = include_str!("../assets/sql/110_master_grants.sql");
/// `dbo.Unicode2TCVN` verbatim from CT_SMILE_POS, as the kinds test holds it.
const KINDS_TEST: &str = include_str!("commands_master_kinds_sql.rs");

/// `ItemDef` as the menu tests shape it and `PkgDef` exactly as the vendor heap.
const SCHEMA: &str = "
CREATE TABLE dbo.ItemDef (ICode char(12) NOT NULL PRIMARY KEY, IName char(70) NULL, PrintName char(70) NULL, Price numeric(16,0) NULL,
    IGroup varchar(20) NULL, IClass varchar(20) NULL, IActive bit NULL, OpenPrice bit NULL, ICurrency varchar(5) NULL, ItemType int NULL,
    IUnit varchar(10) NULL, Status int NULL, AllLoc bit NULL, RVC int NULL, InTaxCode varchar(10) NULL, isPOSItem bit NULL, isBOItem bit NULL,
    OpenItem bit NULL, UseRecipe bit NULL, StockInterface bit NULL, isPkg bit NULL, isVoucher bit NULL, LastChangeBy varchar(20) NULL,
    LastChangeTime datetime NULL);
CREATE TABLE dbo.PkgDef (PkgCode varchar(20) NULL, ItemCode varchar(20) NULL, IQty numeric(18,2) NULL, IPrice numeric(18,0) NULL,
    UseItemPrice bit NULL, DisplayOrder int NULL);
";

/// PostItem's cursor over the components (CT_SMILE_POS, 2026-11-24) without the currency
/// divisor, which is 1 for VND: code, quantity (0 read as 1) and unit price in DisplayOrder.
fn split_sql(pkg: &str) -> String {
    format!(
        "SELECT STRING_AGG(CONCAT(RTRIM(P.ItemCode), '/', CASE WHEN P.IQty = 0 THEN 1 ELSE P.IQty END, '/', \
         CASE WHEN P.UseItemPrice = 1 THEN I.Price ELSE P.IPrice END), '|') WITHIN GROUP (ORDER BY P.DisplayOrder) \
         FROM dbo.PkgDef P JOIN dbo.ItemDef I ON P.ItemCode = I.ICode WHERE P.PkgCode = '{pkg}'"
    )
}

fn rows_sql(pkg: &str) -> String {
    format!(
        "SELECT STRING_AGG(CONCAT(ItemCode, ':', IQty, ':', IPrice, ':', CAST(UseItemPrice AS int), ':', DisplayOrder), ',') \
         WITHIN GROUP (ORDER BY ItemCode) FROM dbo.PkgDef WHERE PkgCode = '{pkg}'"
    )
}

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn unicode2tcvn() -> &'static str {
    let from = KINDS_TEST.find("const UNICODE2TCVN: &str = r#\"").unwrap();
    let body = &KINDS_TEST[from..];
    let start = body.find("r#\"").unwrap() + 3;
    let end = body.find("\"#;").unwrap();
    &body[start..end]
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
            database: format!("CT_RT0979_RS_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(SCHEMA);
        s.exec(unicode2tcvn());
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
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(source)),
            SystemClock::shared(),
            &Logger::none(),
        )
        .with_mapper(mapper())
        .execute(&upsert(entity, key, fields, expected), &Ctx::background())
        .unwrap()
    }

    /// The editor's read: capture's own source on the key columns, then the mirror's hash.
    fn hash(&self, entity: &MasterEntity, key: &str) -> String {
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
        assert_eq!(rows.len(), 1);
        m.canonicalize(entity.table, &rows[0]).unwrap().hash
    }

    /// `HAS_PERMS_BY_NAME` of `pda_sync` on `dbo.<table>` (a column when given).
    fn perm(&self, table: &str, perm: &str, column: Option<&str>) -> i32 {
        let sub = column.map_or("NULL, NULL".to_owned(), |c| format!("'{c}', 'COLUMN'"));
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        c.batch("EXECUTE AS USER = 'pda_sync';").unwrap();
        let r = c
            .query(
                &format!(
                    "SELECT CAST(HAS_PERMS_BY_NAME('dbo.{table}', 'OBJECT', '{perm}', {sub}) AS int);"
                ),
                &[],
            )
            .unwrap();
        let v: i32 = r.rows()[0].get(0).unwrap();
        c.batch("REVERT;").unwrap();
        c.close();
        v
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

fn applied(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

#[test]
fn a_cloud_combo_splits_at_the_pos_exactly_like_a_bo_made_twin_and_is_edited_and_hidden() {
    let db = store!();
    // Two services of class 81 and the BO's own combo of them, as the vendor BO writes it.
    db.exec(
        "INSERT dbo.ItemDef (ICode, IName, PrintName, IGroup, IClass, IActive, Price, OpenPrice, ICurrency, ItemType, IUnit, Status, AllLoc, RVC,
    InTaxCode, isPOSItem, isBOItem, OpenItem, UseRecipe, StockInterface, isPkg, isVoucher)
VALUES ('CB_A', 'Massage', 'Massage', '8', '81', 1, 200000, 0, 'VND', 2, 'LAN', 0, 1, 4, 'VAT10', 1, 1, 0, 0, 0, 0, 0),
       ('CB_B', 'Goi dau', 'Goi dau', '8', '81', 1, 150000, 0, 'VND', 2, 'LAN', 0, 1, 4, 'VAT10', 1, 1, 0, 0, 0, 0, 0),
       ('CB_BO', 'Combo BO', 'Combo BO', '8', '81', 1, 480000, 0, 'VND', 2, 'LAN', 0, 1, 4, 'VAT10', 1, 1, 0, 0, 0, 1, 0);
INSERT dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder)
VALUES ('CB_BO', 'CB_A', 1, 180000, 0, 1), ('CB_BO', 'CB_B', 2, 0, 1, 2);",
    );

    applied(&db.run(
        item(),
        "CB_CL",
        &[
            ("IName", "Combo cloud"),
            ("Price", "480000"),
            ("IGroup", "8"),
            ("IClass", "81"),
            ("IActive", "1"),
            ("OpenPrice", "0"),
            ("isPkg", "1"),
        ],
        None,
    ));
    // Created out of order: the POS reads DisplayOrder, never the insert order.
    applied(&db.run(line(), "CB_CL:CB_B", &combo_line("2", "0", "1", "2"), None));
    applied(&db.run(
        line(),
        "CB_CL:CB_A",
        &combo_line("1", "180000", "0", "1"),
        None,
    ));

    assert_eq!(
        db.value("SELECT CONCAT(CAST(isPkg AS int), '/', Price, '/', LastChangeBy) FROM dbo.ItemDef WHERE ICode = 'CB_CL'"),
        "1/480000/CLOUD"
    );
    let bo = db.value(&split_sql("CB_BO"));
    assert_eq!(bo, "CB_A/1.00/180000|CB_B/2.00/150000");
    assert_eq!(
        db.value(&split_sql("CB_CL")),
        bo,
        "the cloud combo splits like the BO one"
    );
    assert_eq!(db.value(&rows_sql("CB_CL")), db.value(&rows_sql("CB_BO")));

    // An edit of one component keeps the fields it leaves out; hiding the combo is IActive.
    let seen = db.hash(line(), "CB_CL:CB_A");
    applied(&db.run(line(), "CB_CL:CB_A", &[("IPrice", "170000")], Some(&seen)));
    assert_eq!(
        db.value(&split_sql("CB_CL")),
        "CB_A/1.00/170000|CB_B/2.00/150000"
    );
    let seen = db.hash(item(), "CB_CL");
    applied(&db.run(item(), "CB_CL", &[("IActive", "0")], Some(&seen)));
    assert_eq!(
        db.value("SELECT CONCAT(CAST(IActive AS int), '/', CAST(isPkg AS int)) FROM dbo.ItemDef WHERE ICode = 'CB_CL'"),
        "0/1"
    );
}

#[test]
fn a_component_needs_a_combo_header_and_an_existing_item_and_one_the_bo_added_first_is_refused() {
    let db = store!();
    db.exec(
        "INSERT dbo.ItemDef (ICode, IName, PrintName, IGroup, IClass, IActive, Price, OpenPrice, isPkg)
VALUES ('CB_P1', 'Plain', 'Plain', '8', '82', 1, 100000, 0, 0), ('CB_K1', 'Combo', 'Combo', '8', '82', 1, 250000, 0, 1);",
    );
    // A plain header, a missing combo, a missing component, a combo containing itself.
    for key in [
        "CB_P1:CB_K1",
        "CB_NONE:CB_P1",
        "CB_K1:CB_GONE",
        "CB_K1:CB_K1",
    ] {
        let r = db.run(line(), key, &combo_line("1", "1000", "0", "1"), None);
        assert_eq!(
            r.reason.as_deref(),
            Some(master_reasons::NO_TEMPLATE),
            "{key}"
        );
    }
    assert_eq!(db.value("SELECT COUNT(*) FROM dbo.PkgDef"), "0");

    // The BO added the same component while the editor was open: never overwritten.
    db.exec(
        "INSERT dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder) VALUES ('CB_K1', 'CB_P1', 3, 90000, 0, 1);",
    );
    refused(
        &db.run(
            line(),
            "CB_K1:CB_P1",
            &combo_line("1", "1000", "0", "1"),
            None,
        ),
        master_reasons::ROW_EXISTS,
    );
    assert_eq!(
        db.value("SELECT CONCAT(COUNT(*), '/', MAX(IQty), '/', MAX(IPrice)) FROM dbo.PkgDef WHERE PkgCode = 'CB_K1'"),
        "1/3.00/90000"
    );

    let seen = db.hash(line(), "CB_K1:CB_P1");
    let r = db.run(line(), "CB_K1:CB_P1", &[("IQty", "10000")], Some(&seen));
    assert_eq!(r.status, CommandStatus::Rejected);
    assert!(
        r.reason
            .as_deref()
            .unwrap_or_default()
            .starts_with("invalid_payload: "),
        "{:?}",
        r.reason
    );
}

#[test]
fn a_bo_edit_after_the_editor_read_wins_and_a_fresh_cloud_edit_keeps_it() {
    let db = store!();
    db.exec(
        "INSERT dbo.ItemDef (ICode, IName, PrintName, IGroup, IClass, IActive, Price, OpenPrice, isPkg)
VALUES ('CB_S1', 'Xong hoi', 'Xong hoi', '8', '83', 1, 120000, 0, 0), ('CB_K3', 'Combo 3', 'Combo 3', '8', '83', 1, 300000, 0, 1);
INSERT dbo.PkgDef (PkgCode, ItemCode, IQty, IPrice, UseItemPrice, DisplayOrder) VALUES ('CB_K3', 'CB_S1', 1, 100000, 0, 1);",
    );
    let read = db.hash(line(), "CB_K3:CB_S1");
    db.exec(
        "UPDATE dbo.PkgDef SET IPrice = 110000 WHERE PkgCode = 'CB_K3' AND ItemCode = 'CB_S1';",
    );
    refused(
        &db.run(line(), "CB_K3:CB_S1", &[("IPrice", "95000")], Some(&read)),
        master_reasons::CHANGED,
    );
    assert_eq!(db.value(&split_sql("CB_K3")), "CB_S1/1.00/110000");

    let seen = db.hash(line(), "CB_K3:CB_S1");
    applied(&db.run(
        line(),
        "CB_K3:CB_S1",
        &[("IQty", "2"), ("DisplayOrder", "3")],
        Some(&seen),
    ));
    assert_eq!(
        db.value("SELECT CONCAT(IQty, '/', IPrice, '/', CAST(UseItemPrice AS int), '/', DisplayOrder) FROM dbo.PkgDef WHERE PkgCode = 'CB_K3'"),
        "2.00/110000/0/3"
    );
}

#[test]
fn the_pda_sync_user_may_insert_and_update_the_four_component_columns_only() {
    let db = store!();
    db.exec(
        "CREATE USER [pda_sync] WITHOUT LOGIN; ALTER ROLE db_datareader ADD MEMBER [pda_sync];",
    );
    db.exec(MASTER_GRANTS);
    assert_eq!(db.perm("PkgDef", "INSERT", None), 1);
    assert_eq!(db.perm("PkgDef", "DELETE", None), 0);
    for column in ["IQty", "IPrice", "UseItemPrice", "DisplayOrder"] {
        assert_eq!(db.perm("PkgDef", "UPDATE", Some(column)), 1, "{column}");
    }
    for column in ["PkgCode", "ItemCode"] {
        assert_eq!(db.perm("PkgDef", "UPDATE", Some(column)), 0, "{column}");
    }
    assert_eq!(db.perm("ItemDef", "UPDATE", Some("isPkg")), 1);
    db.exec(MASTER_GRANTS);
}
