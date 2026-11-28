//! Two-way integration of the master-data group "menu and service groups" (RT-0985, ADR 0156
//! and 0157) on a real SQL Server: items (services), item groups, menu categories and their
//! items, discount codes and meal periods (sale hours). Cloud -> SMILE_POS goes through the
//! full `master.upsert` executor with the hash the editor read from capture's own source;
//! SMILE_POS -> cloud is what the capture mapper sends (canonical columns + row hash) after
//! an edit in POS Win; the conflict in both orders, the `pda_sync` column grants (no DELETE,
//! only the writable columns) and the audit row (`Pda_SyncCommandLog`) are checked per
//! entity. Synthetic rows only. Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that
//! may create databases); each test creates its own `CT_RT0985_*` database and drops it
//! afterwards.

use std::sync::Arc;

use dl_agent_pda_sync::capture::plan;
use dl_agent_pda_sync::capture::{
    CaptureSource, SourceQuery, SqlServerCaptureSource, SqlServerSourceOptions,
};
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
/// The vendor-shaped tables and the seed rows of the one-way menu test, and the vendor
/// `dbo.Unicode2TCVN` of the kinds test, cut out so none of them is copied twice.
const MENU_TEST: &str = include_str!("commands_master_menu_sql.rs");
const KINDS_TEST: &str = include_str!("commands_master_kinds_sql.rs");

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn between<'a>(text: &'a str, from: &str, to: &str) -> &'a str {
    let start = text.find(from).unwrap() + from.len();
    let end = start + text[start..].find(to).unwrap();
    &text[start..end]
}

fn mapper() -> Arc<CatalogRowMapper> {
    Arc::new(CatalogRowMapper::new(
        SyncCatalog::load().unwrap().clone(),
        MapperOptions::new(TimeZone::fixed("ICT", 7 * 60)),
    ))
}

/// One entity of the group: its wire name, vendor table, an existing key (seeded row), the
/// capture filter of that row and the vendor columns `pda_sync` may update.
struct Kind {
    wire: &'static str,
    table: &'static str,
    key: &'static str,
    filter: fn() -> Vec<(String, RawValue)>,
    granted: &'static [&'static str],
    denied_column: &'static str,
}

fn s(v: &str) -> RawValue {
    RawValue::String(v.to_owned())
}

fn f(col: &str, v: RawValue) -> (String, RawValue) {
    (col.to_owned(), v)
}

const KINDS: [Kind; 6] = [
    Kind {
        wire: "item",
        table: "ItemDef",
        key: "A1",
        filter: || vec![f("ICode", s("A1"))],
        granted: &["IName", "Price", "IGroup", "IClass", "IActive", "OpenPrice"],
        denied_column: "ICurrency",
    },
    Kind {
        wire: "item_group",
        table: "IGroupDef",
        key: "3",
        filter: || vec![f("GCode", RawValue::I32(3))],
        granted: &["GName"],
        denied_column: "RptGroup",
    },
    Kind {
        wire: "menu_category",
        table: "ItemMenu",
        key: "1:M1",
        filter: || vec![f("RVC", RawValue::I32(1)), f("MenuNo", s("M1"))],
        granted: &["MenuName", "MenuActive", "MenuOrder"],
        denied_column: "MainMenuNo",
    },
    Kind {
        wire: "menu_item",
        table: "RVCItemMenu",
        key: "1:M1:A1",
        filter: || {
            vec![
                f("RVC", RawValue::I32(1)),
                f("ItemCode", s("A1")),
                f("ItemMenu", s("M1")),
            ]
        },
        granted: &["ItemShow"],
        denied_column: "Status",
    },
    Kind {
        wire: "discount_code",
        table: "DiscountCode",
        key: "0:D10",
        filter: || vec![f("RVC", RawValue::Null), f("DscCode", s("D10"))],
        granted: &["DscName", "DscByAmount", "DscAmount", "isOpen", "IsVoided"],
        denied_column: "DscPriority",
    },
    Kind {
        wire: "meal_period",
        table: "MealPeriod",
        key: "1:2",
        filter: || vec![f("RVCNo", RawValue::I32(1)), f("MealNo", RawValue::I32(2))],
        granted: &["MealName", "FromTime", "ToTime", "Status"],
        denied_column: "AutoChange",
    },
];

fn kind(wire: &str) -> &'static Kind {
    KINDS.iter().find(|k| k.wire == wire).unwrap()
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
            database: format!("CT_RT0985_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(between(MENU_TEST, "const SCHEMA: &str = \"", "\";\n"));
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

    fn text(&self, sql: &str) -> String {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, &[]).unwrap();
        let v: Option<String> = r.rows()[0].get(0).unwrap();
        c.close();
        v.unwrap_or_else(|| "NULL".into())
    }

    fn count(&self, sql: &str) -> i64 {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, &[]).unwrap();
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
    }

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(source)),
            SystemClock::shared(),
            &Logger::none(),
        )
        .with_mapper(mapper())
        .execute(c, &Ctx::background())
        .unwrap()
    }

    /// What the capture sends to the cloud for one row: its own source (live columns, TCVN3
    /// as bytes) through the catalog mapper. `None` when the row is absent.
    fn capture(&self, k: &Kind) -> Option<CanonicalRow> {
        self.capture_where(k.table, (k.filter)())
    }

    fn capture_where(&self, table: &str, filter: Vec<(String, RawValue)>) -> Option<CanonicalRow> {
        let m = mapper();
        let t = m.catalog().find(table).unwrap();
        let source = SqlServerCaptureSource::new(SqlServerSourceOptions::new(self.config()));
        let mut q = SourceQuery::new(table);
        q.exclude_columns = Some(plan::exclude_of(m.catalog(), t));
        q.bytes_columns = t.transforms.as_ref().and_then(|x| x.tcvn3_decode.clone());
        q.where_ = Some(filter);
        let rows = source.query(&q, &Ctx::background()).unwrap();
        assert!(rows.len() <= 1, "{table}: more than one row");
        rows.first().map(|r| m.canonicalize(table, r).unwrap())
    }

    fn hash(&self, k: &Kind) -> String {
        self.capture(k).unwrap().hash
    }

    fn log_rows(&self, id: Uuid) -> i64 {
        self.count(&format!(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{id}'"
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

fn upsert(wire: &str, key: &str, fields: &str, expected: Option<&str>) -> SyncCommand {
    let expected = expected.map_or(String::new(), |h| format!(r#","expected_hash":"{h}""#));
    let payload = stj::parse(&format!(
        r#"{{"entity":"{wire}","key":"{key}","fields":{fields}{expected},"actor":"admin-1"}}"#
    ))
    .unwrap();
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::MASTER_UPSERT.into(),
        aggregate_key: format!("master:{wire}:{key}"),
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

fn applied(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
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

/// The edit each entity gets, a SQL probe of the stored cell, and the expected cell.
const EDITS: [(&str, &str, &str, &str); 6] = [
    (
        "item",
        r#"{"Price":"31000","IActive":"0"}"#,
        "SELECT CONVERT(varchar(20), Price) + '|' + CONVERT(varchar(1), IActive) + '|' + IGroup FROM dbo.ItemDef",
        "31000|0|G1",
    ),
    (
        "item_group",
        r#"{"GName":"Spa"}"#,
        "SELECT RTRIM(GName) FROM dbo.IGroupDef",
        "Spa",
    ),
    (
        "menu_category",
        r#"{"MenuActive":"0","MenuOrder":"8"}"#,
        "SELECT CONVERT(varchar(1), MenuActive) + '|' + CONVERT(varchar(4), MenuOrder) + '|' + MenuName FROM dbo.ItemMenu",
        "0|8|Old menu",
    ),
    (
        "menu_item",
        r#"{"ItemShow":"0"}"#,
        "SELECT CONVERT(varchar(1), ItemShow) + '|' + CONVERT(varchar(4), Status) FROM dbo.RVCItemMenu",
        "0|4",
    ),
    (
        "discount_code",
        r#"{"DscAmount":"12.5","IsVoided":"1"}"#,
        "SELECT CONVERT(varchar(20), DscAmount) + '|' + CONVERT(varchar(1), IsVoided) + '|' + VoidedBy FROM dbo.DiscountCode",
        "12.50|1|CLOUD",
    ),
    (
        "meal_period",
        r#"{"FromTime":"1130","ToTime":"1430"}"#,
        "SELECT CONVERT(varchar(4), FromTime) + '|' + CONVERT(varchar(4), ToTime) + '|' + MealName FROM dbo.MealPeriod",
        "1130|1430|Lunch",
    ),
];

#[test]
fn cloud_to_pos_every_entity_applies_on_the_hash_capture_sent_and_returns_the_hash_capture_will_mirror()
 {
    let db = store!();
    for (wire, fields, probe, want) in EDITS {
        let k = kind(wire);
        let seen = db.hash(k);
        let res = applied(&db.run(&upsert(wire, k.key, fields, Some(&seen))));
        assert_eq!(db.text(probe), want, "{wire}");
        assert_eq!(
            res.row_hash,
            db.hash(k),
            "{wire}: result hash = next capture hash"
        );
        assert_ne!(res.row_hash, seen, "{wire}: the row changed");
    }
}

#[test]
fn cloud_to_pos_a_create_lands_as_a_row_capture_reads_with_the_returned_hash() {
    let db = store!();
    let created = [
        (
            "item",
            "B2",
            r#"{"IName":"Massage","Price":"300000","IGroup":"G1","IClass":"C1","IActive":"1","OpenPrice":"0"}"#,
            "ItemDef",
            vec![f("ICode", s("B2"))],
        ),
        (
            "item_group",
            "8",
            r#"{"GName":"Facial"}"#,
            "IGroupDef",
            vec![f("GCode", RawValue::I32(8))],
        ),
        (
            "menu_category",
            "1:M2",
            r#"{"MenuName":"Spa menu","MenuActive":"1","MenuOrder":"6"}"#,
            "ItemMenu",
            vec![f("RVC", RawValue::I32(1)), f("MenuNo", s("M2"))],
        ),
        (
            "menu_item",
            "1:M2:A1",
            r#"{"ItemShow":"1"}"#,
            "RVCItemMenu",
            vec![
                f("RVC", RawValue::I32(1)),
                f("ItemCode", s("A1")),
                f("ItemMenu", s("M2")),
            ],
        ),
        (
            "discount_code",
            "0:D20",
            r#"{"DscName":"Twenty","DscByAmount":"0","DscAmount":"20","isOpen":"0","IsVoided":"0"}"#,
            "DiscountCode",
            vec![f("RVC", RawValue::Null), f("DscCode", s("D20"))],
        ),
        (
            "meal_period",
            "1:5",
            r#"{"MealName":"Dinner","FromTime":"1800","ToTime":"2200","Status":"1"}"#,
            "MealPeriod",
            vec![f("RVCNo", RawValue::I32(1)), f("MealNo", RawValue::I32(5))],
        ),
    ];
    for (wire, key, fields, table, filter) in created {
        assert!(db.capture_where(table, filter.clone()).is_none(), "{wire}");
        let res = applied(&db.run(&upsert(wire, key, fields, None)));
        let row = db
            .capture_where(table, filter)
            .unwrap_or_else(|| panic!("{wire}: not captured"));
        assert_eq!(res.row_hash, row.hash, "{wire}");
        // A second create of the same key is refused and writes nothing.
        refused(
            &db.run(&upsert(wire, key, fields, None)),
            "master_row_exists",
        );
    }
}

#[test]
fn pos_to_cloud_an_edit_in_pos_win_changes_what_capture_sends_for_every_entity() {
    let db = store!();
    let before: Vec<_> = KINDS.iter().map(|k| db.capture(k).unwrap()).collect();
    db.exec(
        "UPDATE dbo.ItemDef SET Price = 99000, IActive = 0, LastChangeBy = 'POS', LastChangeTime = GETDATE() WHERE ICode = 'A1';
UPDATE dbo.IGroupDef SET GName = dbo.Unicode2TCVN(N'Đồ uống') WHERE GCode = 3;
UPDATE dbo.ItemMenu SET MenuActive = 0, MenuOrder = 2 WHERE RVC = 1 AND MenuNo = 'M1';
UPDATE dbo.RVCItemMenu SET ItemShow = 0 WHERE RVC = 1 AND ItemCode = 'A1' AND ItemMenu = 'M1';
UPDATE dbo.DiscountCode SET DscAmount = 15, IsVoided = 1 WHERE DscCode = 'D10';
UPDATE dbo.MealPeriod SET FromTime = 1000, ToTime = 1300 WHERE RVCNo = 1 AND MealNo = 2;",
    );
    for (k, old) in KINDS.iter().zip(&before) {
        let now = db.capture(k).unwrap();
        assert_ne!(now.hash, old.hash, "{}: the cloud sees a new hash", k.wire);
        assert!(
            now.columns
                .iter()
                .any(|(c, v)| old.columns.get(c) != Some(v)),
            "{}: a canonical column moved",
            k.wire
        );
    }
    // The TCVN3 group name reaches the cloud decoded, not as vendor bytes.
    let group = db.capture(kind("item_group")).unwrap();
    assert_eq!(
        group
            .columns
            .get("GName")
            .and_then(|v| v.as_deref())
            .map(str::trim_end),
        Some("Đồ uống")
    );
    // And a row of another key is not touched.
    db.exec("INSERT dbo.MealPeriod VALUES (1, 3, 'Dinner', 1800, 2200, 1, 1);");
    let other = db.capture_where(
        "MealPeriod",
        vec![f("RVCNo", RawValue::I32(1)), f("MealNo", RawValue::I32(3))],
    );
    assert_eq!(
        other
            .unwrap()
            .columns
            .get("MealName")
            .and_then(|v| v.as_deref()),
        Some("Dinner")
    );
}

#[test]
fn conflict_store_first_refuses_the_cloud_edit_for_every_entity_and_leaves_the_pos_value() {
    let db = store!();
    for (wire, fields, probe, want) in EDITS {
        let k = kind(wire);
        let seen = db.hash(k);
        // POS Win edits the row after the editor read its hash.
        let pos_edit = match wire {
            "item" => "UPDATE dbo.ItemDef SET Price = 1 WHERE ICode = 'A1'",
            "item_group" => "UPDATE dbo.IGroupDef SET RptGroup = 9 WHERE GCode = 3",
            "menu_category" => {
                "UPDATE dbo.ItemMenu SET MenuName = 'POS menu' WHERE RVC = 1 AND MenuNo = 'M1'"
            }
            "menu_item" => "UPDATE dbo.RVCItemMenu SET Status = 6 WHERE ItemMenu = 'M1'",
            "discount_code" => "UPDATE dbo.DiscountCode SET DscName = 'POS' WHERE DscCode = 'D10'",
            _ => "UPDATE dbo.MealPeriod SET MealName = 'POS' WHERE MealNo = 2",
        };
        db.exec(pos_edit);
        let stale = upsert(wire, k.key, fields, Some(&seen));
        refused(&db.run(&stale), "master_changed");
        assert_ne!(db.text(probe), want, "{wire}: the stale edit wrote nothing");
        assert_eq!(
            db.log_rows(stale.command_id),
            1,
            "{wire}: the refusal is logged"
        );
    }
}

#[test]
fn conflict_cloud_first_then_pos_is_not_blocked_and_the_older_cloud_hash_loses() {
    let db = store!();
    for (wire, fields, probe, want) in EDITS {
        let k = kind(wire);
        let first = applied(&db.run(&upsert(wire, k.key, fields, Some(&db.hash(k)))));
        assert_eq!(db.text(probe), want, "{wire}");
        // POS Win is never blocked by the cloud write.
        db.exec(match wire {
            "item" => "UPDATE dbo.ItemDef SET Price = 777 WHERE ICode = 'A1'",
            "item_group" => "UPDATE dbo.IGroupDef SET GName = 'POS grp' WHERE GCode = 3",
            "menu_category" => "UPDATE dbo.ItemMenu SET MenuName = 'POS menu' WHERE MenuNo = 'M1'",
            "menu_item" => "UPDATE dbo.RVCItemMenu SET Status = 6 WHERE ItemMenu = 'M1'",
            "discount_code" => "UPDATE dbo.DiscountCode SET DscName = 'POS' WHERE DscCode = 'D10'",
            _ => "UPDATE dbo.MealPeriod SET MealName = 'POS' WHERE MealNo = 2",
        });
        let after_pos = db.hash(k);
        assert_ne!(after_pos, first.row_hash, "{wire}");
        // The cloud edit made on the first result hash is now stale; the one on the fresh
        // capture hash applies.
        refused(
            &db.run(&upsert(wire, k.key, fields, Some(&first.row_hash))),
            "master_changed",
        );
        applied(&db.run(&upsert(wire, k.key, fields, Some(&after_pos))));
    }
}

#[test]
fn permission_pda_sync_updates_only_the_granted_columns_and_can_never_delete() {
    let db = store!();
    db.grant_agent_user();
    for k in &KINDS {
        let granted = k.granted[0];
        // The column the write path touches: allowed (a no-op assignment is enough to be checked).
        db.as_agent(&format!(
            "UPDATE dbo.{t} SET {c} = {c}",
            t = k.table,
            c = granted
        ))
        .unwrap_or_else(|e| panic!("{}: granted column {granted}: {e}", k.wire));
        let denied = db.as_agent(&format!(
            "UPDATE dbo.{t} SET {c} = {c}",
            t = k.table,
            c = k.denied_column
        ));
        assert!(
            denied.as_ref().is_err_and(|e| e.contains("permission")),
            "{}: column {} must be denied, got {denied:?}",
            k.wire,
            k.denied_column
        );
        let del = db.as_agent(&format!("DELETE dbo.{}", k.table));
        assert!(
            del.as_ref().is_err_and(|e| e.contains("permission")),
            "{}: DELETE must be denied, got {del:?}",
            k.wire
        );
    }
    assert_eq!(db.count("SELECT CAST((SELECT COUNT(*) FROM dbo.ItemDef) + (SELECT COUNT(*) FROM dbo.IGroupDef) + (SELECT COUNT(*) FROM dbo.ItemMenu) + (SELECT COUNT(*) FROM dbo.RVCItemMenu) + (SELECT COUNT(*) FROM dbo.DiscountCode) + (SELECT COUNT(*) FROM dbo.MealPeriod) AS bigint)"), 6, "nothing was deleted");
    // Running the grants again is a no-op.
    db.exec(MASTER_GRANTS);
}

#[test]
fn audit_each_command_leaves_one_log_row_and_a_redelivery_does_not_write_again() {
    let db = store!();
    for (wire, fields, probe, want) in EDITS {
        let k = kind(wire);
        let cmd = upsert(wire, k.key, fields, Some(&db.hash(k)));
        assert_eq!(db.log_rows(cmd.command_id), 0, "{wire}");
        let first = db.run(&cmd);
        applied(&first);
        assert_eq!(db.log_rows(cmd.command_id), 1, "{wire}");
        assert_eq!(
            db.text(&format!(
                "SELECT Status FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{}'",
                cmd.command_id
            )),
            "Applied"
        );
        // The second delivery would be refused (the row moved): equal results prove the log answered it.
        assert_eq!(db.run(&cmd), first, "{wire}");
        assert_eq!(db.log_rows(cmd.command_id), 1, "{wire}");
        assert_eq!(db.text(probe), want, "{wire}");
    }
    // Refusals write nothing.
    let bad = upsert(
        "item",
        "A1",
        r#"{"ICurrency":"USD"}"#,
        Some(&db.hash(kind("item"))),
    );
    refused(&db.run(&bad), "master_field_not_allowed");
    let missing = upsert(
        "menu_category",
        "1:NONE",
        r#"{"MenuActive":"0"}"#,
        Some("h"),
    );
    refused(&db.run(&missing), "master_row_not_found");
    // A field outside the writable list is refused before any transaction: no log row, no write.
    assert_eq!(db.log_rows(bad.command_id), 0);
    assert_eq!(
        db.text("SELECT RTRIM(ICurrency) FROM dbo.ItemDef WHERE ICode = 'A1'"),
        "VND"
    );
    // A row the store does not have is refused inside the transaction and logged with its reason.
    assert_eq!(db.log_rows(missing.command_id), 1);
}
