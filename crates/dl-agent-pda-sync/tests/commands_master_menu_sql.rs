//! The six menu entities of `master.upsert` (RT-0831, ADR 0156) on a real SQL Server: the
//! update statements run through the command unit against vendor-shaped tables, a field the
//! editor leaves out keeps the store value, a TCVN3 name goes through the vendor
//! `dbo.Unicode2TCVN`, and the multi-part and NULL-outlet keys find their row. Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each test
//! creates its own `CT_RT0961_*` database and drops it afterwards.

use std::sync::Arc;

use dl_agent_pda_sync::commands::master::find_entity;
use dl_agent_pda_sync::commands::{ConfigSource, SiteCommandStore, SqlSiteCommandStore};
use dl_agent_pda_sync::contracts::mapping::tcvn3;
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{CommandStatus, SyncCommandResult};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_tds::Param;
use dl_uuid::Uuid;

const COMMAND_LOG: &str = include_str!("../assets/sql/030_command_log.sql");
/// `dbo.Unicode2TCVN` verbatim from CT_SMILE_POS, as the kinds test holds it.
const KINDS_TEST: &str = include_str!("commands_master_kinds_sql.rs");

const SCHEMA: &str = "
CREATE TABLE dbo.ItemDef (ICode char(12) NOT NULL PRIMARY KEY, IName char(70) NULL, PrintName char(70) NULL, Price numeric(16,0) NULL,
    IGroup varchar(20) NULL, IClass varchar(20) NULL, IActive bit NULL, OpenPrice bit NULL, ICurrency varchar(5) NULL, ItemType int NULL,
    IUnit varchar(10) NULL, Status int NULL, AllLoc bit NULL, RVC int NULL, InTaxCode varchar(10) NULL, isPOSItem bit NULL, isBOItem bit NULL,
    OpenItem bit NULL, UseRecipe bit NULL, StockInterface bit NULL, isPkg bit NULL, isVoucher bit NULL, LastChangeBy varchar(20) NULL,
    LastChangeTime datetime NULL);
CREATE TABLE dbo.IGroupDef (GCode int NOT NULL PRIMARY KEY, GName char(30) NULL, RptGroup int NULL, GLevel int NULL, GParent int NULL,
    RVC int NULL, Status int NULL);
CREATE TABLE dbo.ItemMenu (MenuNo varchar(30) NOT NULL, RVC int NOT NULL, MenuName varchar(100) NULL, MenuActive bit NULL, MenuOrder int NULL,
    MainMenuNo varchar(30) NULL, Status int NULL, TvM bit NULL, WebActive bit NULL, HasSubMenu bit NULL, PNo int NULL, ShowWPrice bit NULL,
    ParentMenu varchar(30) NULL, PRIMARY KEY (MenuNo, RVC));
CREATE TABLE dbo.RVCItemMenu (RVC int NOT NULL, ItemCode varchar(30) NOT NULL, ItemMenu varchar(30) NOT NULL, ItemShow bit NULL, Status int NULL,
    PRIMARY KEY (RVC, ItemCode, ItemMenu));
CREATE TABLE dbo.DiscountCode (RVC int NULL, DscCode varchar(20) NOT NULL, DscName varchar(100) NULL, DscByAmount bit NULL,
    DscAmount numeric(18,2) NULL, isOpen bit NULL, IsVoided bit NULL, VoidedBy nvarchar(50) NULL, VoidedAt datetime2 NULL, MbrDefault bit NULL,
    isAuto bit NULL, isVoucher bit NULL, DscOnTotal bit NULL, DscPriority int NULL, RequireStaff bit NULL);
CREATE TABLE dbo.MealPeriod (RVCNo int NOT NULL, MealNo int NOT NULL, MealName varchar(16) NULL, FromTime int NULL, ToTime int NULL,
    Status int NULL, AutoChange bit NULL, PRIMARY KEY (RVCNo, MealNo));
INSERT dbo.ItemDef (ICode, IName, Price, IGroup, IClass, IActive, OpenPrice, ICurrency, ItemType, IUnit, Status, AllLoc, RVC, InTaxCode,
    isPOSItem, isBOItem, OpenItem, UseRecipe, StockInterface, isPkg, isVoucher)
    VALUES ('A1', 'Old', 10000, 'G1', 'C1', 1, 0, 'VND', 2, 'LAN', 1, 1, 0, 'VAT10', 1, 1, 0, 0, 0, 0, 0);
INSERT dbo.IGroupDef VALUES (3, 'Old group', 7, 1, 0, 0, 1);
INSERT dbo.ItemMenu (MenuNo, RVC, MenuName, MenuActive, MenuOrder, MainMenuNo, Status, TvM, WebActive, HasSubMenu, PNo, ShowWPrice)
    VALUES ('M1', 1, 'Old menu', 1, 5, 'MAIN', 1, 1, 1, 0, 9, 1);
INSERT dbo.RVCItemMenu VALUES (1, 'A1', 'M1', 1, 4);
INSERT dbo.DiscountCode (RVC, DscCode, DscName, DscByAmount, DscAmount, isOpen, IsVoided, MbrDefault, isAuto, isVoucher, DscOnTotal, DscPriority,
    RequireStaff) VALUES (NULL, 'D10', 'Ten', 0, 10, 0, 0, 0, 0, 0, 1, 3, 1);
INSERT dbo.MealPeriod VALUES (1, 2, 'Lunch', 1100, 1400, 1, 1);
";

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// The vendor function text, cut out of the kinds test so it is not copied twice.
fn unicode2tcvn() -> &'static str {
    let from = KINDS_TEST.find("const UNICODE2TCVN: &str = r#\"").unwrap();
    let body = &KINDS_TEST[from..];
    let start = body.find("r#\"").unwrap() + 3;
    let end = body.find("\"#;").unwrap();
    &body[start..end]
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
            database: format!("CT_RT0961_{}", &tag[..10]),
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

    /// The command unit's update in its own transaction; whether the row changed.
    fn write(&self, wire: &str, key: &str, values: &[(&str, &str)]) -> bool {
        let entity = find_entity(wire).unwrap();
        let key = entity.parse_key(key).unwrap();
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        let id = Uuid::new_v4();
        let mut changed = false;
        SqlSiteCommandStore::new(source)
            .run_once(
                id,
                &mut |unit, ctx| {
                    changed = unit.update_master_row(entity, &key, values, ctx)?;
                    Ok(SyncCommandResult {
                        command_id: id,
                        status: CommandStatus::Applied,
                        reason: None,
                        result: None,
                        at_utc: DateTimeOffset::now_utc(),
                        exec_ms: None,
                        hops: Default::default(),
                    })
                },
                &Ctx::background(),
            )
            .unwrap();
        changed
    }

    /// The command unit's insert in its own transaction; whether a row was written.
    fn insert(&self, wire: &str, key: &str, values: &[(&str, &str)]) -> bool {
        let entity = find_entity(wire).unwrap();
        let key = entity.parse_key(key).unwrap();
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        let id = Uuid::new_v4();
        let mut inserted = false;
        SqlSiteCommandStore::new(source)
            .run_once(
                id,
                &mut |unit, ctx| {
                    inserted = unit.insert_master_row(entity, &key, values, ctx)?;
                    Ok(SyncCommandResult {
                        command_id: id,
                        status: CommandStatus::Applied,
                        reason: None,
                        result: None,
                        at_utc: DateTimeOffset::now_utc(),
                        exec_ms: None,
                        hops: Default::default(),
                    })
                },
                &Ctx::background(),
            )
            .unwrap();
        inserted
    }

    /// One text cell of a one-row query (NULL -> `None`).
    fn text(&self, sql: &str) -> Option<String> {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, &[] as &[Param]).unwrap();
        let v: Option<String> = r.rows()[0].get(0).unwrap();
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

#[test]
fn an_item_keeps_the_fields_the_editor_left_out_and_stores_a_tcvn3_name() {
    let db = store!();
    assert!(db.write("item", "A1", &[("IName", "Gội đầu"), ("Price", "25000")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(20), Price) + '|' + IGroup + '|' + IClass + '|' + CONVERT(varchar(1), IActive) + '|' + LastChangeBy FROM dbo.ItemDef")
            .as_deref(),
        Some("25000|G1|C1|1|CLOUD")
    );
    let name = db
        .text("SELECT CONVERT(varchar(140), CONVERT(varbinary(70), IName), 2) FROM dbo.ItemDef")
        .unwrap();
    let bytes: Vec<u8> = (0..name.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&name[i..i + 2], 16).unwrap())
        .collect();
    assert_eq!(tcvn3::decode(&bytes).trim_end_matches(' '), "Gội đầu");
    assert!(db.write("item", "A1", &[("IActive", "0")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(1), IActive) FROM dbo.ItemDef")
            .as_deref(),
        Some("0")
    );
    assert_eq!(
        db.text("SELECT CONVERT(varchar(20), Price) FROM dbo.ItemDef")
            .as_deref(),
        Some("25000")
    );
}

#[test]
fn a_menu_category_and_a_menu_item_are_found_by_their_multi_part_keys() {
    let db = store!();
    assert!(db.write("item_group", "3", &[("GName", "Đồ uống")]));
    assert!(db.write("menu_category", "1:M1", &[("MenuActive", "0")]));
    assert_eq!(
        db.text("SELECT MenuName + '|' + CONVERT(varchar(1), MenuActive) + '|' + CONVERT(varchar(4), MenuOrder) FROM dbo.ItemMenu")
            .as_deref(),
        Some("Old menu|0|5")
    );
    assert!(db.write("menu_item", "1:M1:A1", &[("ItemShow", "0")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(1), ItemShow) FROM dbo.RVCItemMenu")
            .as_deref(),
        Some("0")
    );
}

#[test]
fn a_discount_code_with_a_null_outlet_is_voided_stamped_and_restored() {
    let db = store!();
    assert!(db.write("discount_code", "0:D10", &[("DscAmount", "12.5")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(20), DscAmount) + '|' + DscName FROM dbo.DiscountCode")
            .as_deref(),
        Some("12.50|Ten")
    );
    assert!(db.write("discount_code", "0:D10", &[("IsVoided", "1")]));
    assert_eq!(
        db.text("SELECT VoidedBy + '|' + CONVERT(varchar(1), IsVoided) + '|' + CONVERT(varchar(1), IIF(VoidedAt IS NULL, 0, 1)) FROM dbo.DiscountCode")
            .as_deref(),
        Some("CLOUD|1|1")
    );
    assert!(db.write("discount_code", "0:D10", &[("IsVoided", "0")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(1), IsVoided) + CONVERT(varchar(1), IIF(VoidedBy IS NULL AND VoidedAt IS NULL, 1, 0)) FROM dbo.DiscountCode")
            .as_deref(),
        Some("01")
    );
}

#[test]
fn a_meal_period_changes_only_the_sent_hours() {
    let db = store!();
    assert!(db.write(
        "meal_period",
        "1:2",
        &[("FromTime", "1130"), ("ToTime", "1430")]
    ));
    assert_eq!(
        db.text("SELECT MealName + '|' + CONVERT(varchar(4), FromTime) + '|' + CONVERT(varchar(4), ToTime) + '|' + CONVERT(varchar(1), Status) FROM dbo.MealPeriod")
            .as_deref(),
        Some("Lunch|1130|1430|1")
    );
}

#[test]
fn a_new_item_copies_the_vendor_columns_of_a_template_of_its_class() {
    let db = store!();
    assert!(db.insert(
        "item",
        "B2",
        &[
            ("IName", "Massage"),
            ("Price", "300000"),
            ("IGroup", "G1"),
            ("IClass", "C1"),
            ("IActive", "1"),
            ("OpenPrice", "0"),
        ]
    ));
    assert_eq!(
        db.text("SELECT RTRIM(PrintName) + '|' + ICurrency + '|' + IUnit + '|' + InTaxCode + '|' + CONVERT(varchar(1), isPkg) + CONVERT(varchar(1), OpenItem) + '|' + LastChangeBy FROM dbo.ItemDef WHERE ICode = 'B2'")
            .as_deref(),
        Some("Massage|VND|LAN|VAT10|00|CLOUD")
    );
    assert_eq!(
        db.text("SELECT CONVERT(varchar(20), Price) FROM dbo.ItemDef WHERE ICode = 'B2'")
            .as_deref(),
        Some("300000")
    );
}

#[test]
fn a_create_with_no_template_inserts_nothing() {
    let db = store!();
    let item = [
        ("IName", "X"),
        ("Price", "1"),
        ("IGroup", "G9"),
        ("IClass", "NOCLASS"),
        ("IActive", "1"),
        ("OpenPrice", "0"),
    ];
    assert!(!db.insert("item", "B3", &item));
    assert!(!db.insert("menu_item", "1:NOMENU:A1", &[("ItemShow", "1")]));
    assert!(!db.insert("menu_item", "1:M1:NOITEM", &[("ItemShow", "1")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(4), (SELECT COUNT(*) FROM dbo.ItemDef) + (SELECT COUNT(*) FROM dbo.RVCItemMenu))")
            .as_deref(),
        Some("2")
    );
}

#[test]
fn a_new_group_category_and_menu_item_take_the_template_columns() {
    let db = store!();
    assert!(db.insert("item_group", "8", &[("GName", "Spa")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(4), RptGroup) + '|' + CONVERT(varchar(4), GLevel) + '|' + CONVERT(varchar(4), Status) FROM dbo.IGroupDef WHERE GCode = 8")
            .as_deref(),
        Some("7|1|1")
    );
    assert!(db.insert(
        "menu_category",
        "1:M2",
        &[
            ("MenuName", "Spa menu"),
            ("MenuActive", "1"),
            ("MenuOrder", "6")
        ]
    ));
    assert_eq!(
        db.text("SELECT MainMenuNo + '|' + CONVERT(varchar(1), HasSubMenu) + '|' + CONVERT(varchar(4), PNo) FROM dbo.ItemMenu WHERE MenuNo = 'M2'")
            .as_deref(),
        Some("MAIN|0|9")
    );
    assert!(db.insert("menu_item", "1:M2:A1", &[("ItemShow", "1")]));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(4), Status) FROM dbo.RVCItemMenu WHERE ItemMenu = 'M2'")
            .as_deref(),
        Some("4")
    );
}

#[test]
fn a_new_discount_code_and_meal_period_take_their_outlet_defaults() {
    let db = store!();
    assert!(db.insert(
        "discount_code",
        "0:D20",
        &[
            ("DscName", "Twenty"),
            ("DscByAmount", "0"),
            ("DscAmount", "20"),
            ("isOpen", "0"),
            ("IsVoided", "0")
        ]
    ));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(20), IIF(RVC IS NULL, 1, 0)) + '|' + CONVERT(varchar(4), DscPriority) + '|' + CONVERT(varchar(1), isAuto) FROM dbo.DiscountCode WHERE DscCode = 'D20'")
            .as_deref(),
        Some("1|3|0")
    );
    assert!(db.insert(
        "meal_period",
        "1:5",
        &[
            ("MealName", "Dinner"),
            ("FromTime", "1800"),
            ("ToTime", "2200"),
            ("Status", "1")
        ]
    ));
    assert_eq!(
        db.text("SELECT CONVERT(varchar(1), AutoChange) FROM dbo.MealPeriod WHERE MealNo = 5")
            .as_deref(),
        Some("1")
    );
}
