//! `master.upsert` for the outlet layout and parameters (ADR 0152) on a real SQL Server, a
//! port of the .NET `SqlMasterOutletIntegrationTests`: create from a template row, a partial
//! table edit through `COALESCE`, the POS-first conflict, racing creates of one key on a heap,
//! protected parameters and the INSERT/UPDATE grants of `pda_sync`. Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may create databases); each test creates
//! its own `CT_RT0833_RS_*` database with the vendor tables as heaps (all columns nullable)
//! and drops it afterwards.

use std::sync::Arc;

use dl_agent_pda_sync::capture::plan;
use dl_agent_pda_sync::capture::{
    CaptureSource, SourceQuery, SqlServerCaptureSource, SqlServerSourceOptions,
};
use dl_agent_pda_sync::commands::master::{
    KeyValue, MasterEntity, OUTLET_PARAM, TABLE, TABLE_AREA, master_reasons,
};
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

/// The vendor outlet tables as the stores have them: heaps, every column nullable.
const OUTLET_DDL: &str = "CREATE TABLE dbo.TableArea (TableArea int NULL, TableAreaName nvarchar(50) NULL, RVC int NULL, Img1 image NULL, BuildingCode varchar(2) NULL, OrderInRed int NULL, SectionID int NULL, HHExclude int NULL);
CREATE TABLE dbo.TableList (TableNum varchar(20) NULL, TableType varchar(5) NULL, TableArea int NULL, Description varchar(60) NULL, RVC int NULL, TableStatus int NULL, LastChangedBy varchar(30) NULL, LastChangedTime datetime NULL, TableGroup varchar(30) NULL, GrpOrder int NULL, ShowBookBeforeMin int NULL, IsActive bit NULL);
CREATE TABLE dbo.Para (ParaName varchar(50) NULL, ParaStr varchar(500) NULL, ParaType int NULL, ParaDesc char(30) NULL, RVC int NULL, Status int NULL);";

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
            database: format!("CT_RT0833_RS_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(OUTLET_DDL);
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

    /// The first column of the first row of `select`, as text (`CONCAT` makes any value
    /// text).
    fn value(&self, select: &str) -> String {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c
            .query(&format!("SELECT CONCAT(({select}), N'');"), &[])
            .unwrap_or_else(|e| panic!("{select}: {e}"));
        let v: String = r.rows()[0].get(0).unwrap();
        c.close();
        v
    }

    fn executor(&self) -> SiteCommandExecutor {
        let cfg = self.config();
        let source: ConfigSource = Arc::new(move || Ok(cfg.clone()));
        SiteCommandExecutor::new(
            Arc::new(SqlSiteCommandStore::new(source)),
            SystemClock::shared(),
            &Logger::none(),
        )
        .with_mapper(mapper())
    }

    fn run(
        &self,
        entity: &MasterEntity,
        key: &str,
        fields: &[(&str, &str)],
        expected: Option<&str>,
    ) -> SyncCommandResult {
        self.executor()
            .execute(&upsert(entity, key, fields, expected), &Ctx::background())
            .unwrap()
    }

    /// The editor's read: capture's own source (live columns, TCVN3 as bytes) on the key
    /// columns, then the mirror's hash of that row.
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

fn applied(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

#[test]
fn table_area_is_created_in_unicode_and_renamed_and_a_second_create_is_refused() {
    let db = store!();
    let e = &TABLE_AREA;
    applied(&db.run(e, "41:3", &[("TableAreaName", "Khu vườn")], None));
    assert_eq!(
        db.value("SELECT TableAreaName FROM dbo.TableArea WHERE RVC = 41 AND TableArea = 3"),
        "Khu vườn"
    );
    assert_eq!(
        db.value("SELECT CONCAT(OrderInRed, '/', SectionID, '/', HHExclude) FROM dbo.TableArea WHERE RVC = 41 AND TableArea = 3"),
        "0/0/0"
    );

    refused(
        &db.run(e, "41:3", &[("TableAreaName", "Again")], None),
        master_reasons::ROW_EXISTS,
    );
    let seen = db.hash(e, "41:3");
    applied(&db.run(e, "41:3", &[("TableAreaName", "Sân thượng")], Some(&seen)));
    assert_eq!(
        db.value("SELECT COUNT(*) FROM dbo.TableArea WHERE RVC = 41"),
        "1"
    );
    assert_eq!(
        db.value("SELECT TableAreaName FROM dbo.TableArea WHERE RVC = 41 AND TableArea = 3"),
        "Sân thượng"
    );
}

#[test]
fn table_is_created_from_a_template_edited_one_field_at_a_time_and_a_pos_change_first_wins() {
    let db = store!();
    db.exec(
        "INSERT dbo.TableList (TableNum, TableType, TableArea, Description, RVC, TableStatus, TableGroup, GrpOrder, ShowBookBeforeMin, IsActive)
VALUES ('B1', 'BED', 2, 'Bed 1', 42, 0, NULL, 0, 30, 1);",
    );
    let e = &TABLE;
    let row = "FROM dbo.TableList WHERE RVC = 42 AND TableNum = 'B2'";

    applied(&db.run(
        e,
        "42:B2",
        &[
            ("Description", "Bed 2"),
            ("TableArea", "2"),
            ("IsActive", "1"),
        ],
        None,
    ));
    assert_eq!(
        db.value(&format!("SELECT CONCAT(TableType, '/', TableArea, '/', Description, '/', TableStatus, '/', ShowBookBeforeMin, '/', CAST(IsActive AS int), '/', LastChangedBy) {row}")),
        "BED/2/Bed 2/0/30/1/CLOUD"
    );

    // Only IsActive is sent: Description and TableArea keep their store values.
    let seen = db.hash(e, "42:B2");
    applied(&db.run(e, "42:B2", &[("IsActive", "0")], Some(&seen)));
    assert_eq!(
        db.value(&format!(
            "SELECT CONCAT(TableArea, '/', Description, '/', CAST(IsActive AS int)) {row}"
        )),
        "2/Bed 2/0"
    );

    // The POS opens a bill on the bed after the editor read it: the cloud edit is refused
    // and the store row is untouched.
    let seen = db.hash(e, "42:B2");
    db.exec("UPDATE dbo.TableList SET TableStatus = 1 WHERE RVC = 42 AND TableNum = 'B2';");
    refused(
        &db.run(e, "42:B2", &[("Description", "Cloud")], Some(&seen)),
        master_reasons::CHANGED,
    );
    assert_eq!(
        db.value(&format!(
            "SELECT CONCAT(Description, '/', TableStatus) {row}"
        )),
        "Bed 2/1"
    );

    // On the fresh hash the edit lands and never touches TableStatus.
    let fresh = db.hash(e, "42:B2");
    applied(&db.run(e, "42:B2", &[("Description", "Cloud")], Some(&fresh)));
    assert_eq!(
        db.value(&format!(
            "SELECT CONCAT(Description, '/', TableStatus) {row}"
        )),
        "Cloud/1"
    );
}

#[test]
fn racing_creates_of_one_table_area_insert_exactly_one_row() {
    let db = store!();
    let results: Vec<SyncCommandResult> = std::thread::scope(|s| {
        let runs: Vec<_> = (0..4)
            .map(|i| {
                let db = &db;
                s.spawn(move || {
                    let name = format!("Race {i}");
                    db.run(&TABLE_AREA, "43:7", &[("TableAreaName", &name)], None)
                })
            })
            .collect();
        runs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let won = results
        .iter()
        .filter(|r| r.status == CommandStatus::Applied)
        .count();
    assert_eq!(won, 1, "{results:?}");
    for r in results
        .iter()
        .filter(|r| r.status != CommandStatus::Applied)
    {
        refused(r, master_reasons::ROW_EXISTS);
    }
    assert_eq!(
        db.value("SELECT COUNT(*) FROM dbo.TableArea WHERE RVC = 43 AND TableArea = 7"),
        "1"
    );
}

#[test]
fn outlet_param_copies_a_known_name_refuses_an_unknown_one_and_never_touches_a_protected_one() {
    let db = store!();
    db.exec(
        "INSERT dbo.Para (ParaName, ParaStr, ParaType, ParaDesc, RVC, Status) VALUES ('TipPercent', '10', 1, 'Tip', 44, 0), ('LastCheck', '120', 1, 'Seq', 45, 0);",
    );
    let e = &OUTLET_PARAM;

    refused(
        &db.run(e, "45:NoSuchParam", &[("ParaStr", "1")], None),
        master_reasons::NO_TEMPLATE,
    );
    assert_eq!(
        db.value("SELECT COUNT(*) FROM dbo.Para WHERE ParaName = 'NoSuchParam'"),
        "0"
    );

    applied(&db.run(e, "45:TipPercent", &[("ParaStr", "5")], None));
    assert_eq!(
        db.value("SELECT CONCAT(ParaStr, '/', ParaType, '/', RTRIM(ParaDesc), '/', Status) FROM dbo.Para WHERE RVC = 45 AND ParaName = 'TipPercent'"),
        "5/1/Tip/0"
    );
    assert_eq!(
        db.value("SELECT ParaStr FROM dbo.Para WHERE RVC = 44 AND ParaName = 'TipPercent'"),
        "10"
    );

    refused(
        &db.run(e, "45:LastCheck", &[("ParaStr", "1")], Some("any")),
        master_reasons::ROW_PROTECTED,
    );
    assert_eq!(
        db.value("SELECT ParaStr FROM dbo.Para WHERE RVC = 45 AND ParaName = 'LastCheck'"),
        "120"
    );
}

#[test]
fn the_pda_sync_user_may_insert_and_update_the_granted_columns_but_never_delete_or_touch_the_rest()
{
    let db = store!();
    db.exec(
        "CREATE USER [pda_sync] WITHOUT LOGIN; ALTER ROLE db_datareader ADD MEMBER [pda_sync];",
    );
    db.exec(MASTER_GRANTS);

    for table in ["TableArea", "TableList", "Para"] {
        assert_eq!(db.perm(table, "INSERT", None), 1, "{table}");
        assert_eq!(db.perm(table, "DELETE", None), 0, "{table}");
    }
    assert_eq!(db.perm("TableArea", "UPDATE", Some("TableAreaName")), 1);
    assert_eq!(db.perm("TableArea", "UPDATE", Some("RVC")), 0);
    for column in [
        "Description",
        "TableArea",
        "IsActive",
        "LastChangedBy",
        "LastChangedTime",
    ] {
        assert_eq!(db.perm("TableList", "UPDATE", Some(column)), 1, "{column}");
    }
    for column in ["TableStatus", "TableNum", "RVC", "TableType", "TableGroup"] {
        assert_eq!(db.perm("TableList", "UPDATE", Some(column)), 0, "{column}");
    }
    assert_eq!(db.perm("Para", "UPDATE", Some("ParaStr")), 1);
    for column in ["ParaName", "RVC", "ParaType", "Status"] {
        assert_eq!(db.perm("Para", "UPDATE", Some(column)), 0, "{column}");
    }
    // Running the script again is a no-op (idempotent).
    db.exec(MASTER_GRANTS);
}
