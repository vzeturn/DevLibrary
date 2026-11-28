//! Two-way integration of the master-data group "floor areas, tables, beds and outlet
//! parameters" on a real SQL Server (RT-0987, RT-0833, ADR 0154): cloud -> SMILE_POS
//! (`master.upsert` entities `table_area`, `table`, `outlet_param`), SMILE_POS -> cloud (the
//! capture read of the same row and its mirror hash), the conflict in both orders, the
//! branch (RVC) scope of a write and the exactly-once audit row (`Pda_SyncCommandLog`).
//! Synthetic outlets only. Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL of a login that may
//! create databases); each test creates its own `CT_RT0987_*` database with the vendor
//! tables as heaps and drops it afterwards.

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
            database: format!("CT_RT0987_{}", &tag[..10]),
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

const SEED: &str = "INSERT dbo.TableArea (TableArea, TableAreaName, RVC, OrderInRed, SectionID, HHExclude) VALUES (1, N'Khu A', 51, 0, 0, 0), (1, N'Khu A', 52, 0, 0, 0);
INSERT dbo.TableList (TableNum, TableType, TableArea, Description, RVC, TableStatus, GrpOrder, ShowBookBeforeMin, IsActive)
VALUES ('G1', 'BED', 1, 'Giuong 1', 51, 0, 0, 30, 1), ('G1', 'BED', 1, 'Giuong 1', 52, 0, 0, 30, 1);
INSERT dbo.Para (ParaName, ParaStr, ParaType, ParaDesc, RVC, Status) VALUES ('TipPercent', '10', 1, 'Tip', 51, 0), ('TipPercent', '10', 1, 'Tip', 52, 0);";

impl Store {
    fn seeded(url: &str) -> Store {
        let s = Store::create(url);
        s.exec(SEED);
        s
    }

    fn log_rows(&self, id: Uuid) -> String {
        self.value(&format!(
            "SELECT COUNT(*) FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{id}'"
        ))
    }
}

macro_rules! seeded {
    () => {{
        let Some(url) = admin_url() else {
            eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
            return;
        };
        Store::seeded(&url)
    }};
}

#[test]
fn cloud_to_pos_edit_writes_area_bed_and_parameter_which_the_capture_read_hashes_back() {
    let db = seeded!();
    let area = db.hash(&TABLE_AREA, "51:1");
    applied(&db.run(
        &TABLE_AREA,
        "51:1",
        &[("TableAreaName", "Khu vườn")],
        Some(&area),
    ));
    assert_eq!(
        db.value("SELECT TableAreaName FROM dbo.TableArea WHERE RVC = 51 AND TableArea = 1"),
        "Khu vườn"
    );
    assert_ne!(db.hash(&TABLE_AREA, "51:1"), area);

    let bed = db.hash(&TABLE, "51:G1");
    applied(&db.run(
        &TABLE,
        "51:G1",
        &[("Description", "Giuong VIP"), ("IsActive", "0")],
        Some(&bed),
    ));
    assert_eq!(
        db.value("SELECT CONCAT(Description, '/', CAST(IsActive AS int), '/', TableStatus, '/', LastChangedBy) FROM dbo.TableList WHERE RVC = 51 AND TableNum = 'G1'"),
        "Giuong VIP/0/0/CLOUD"
    );

    let para = db.hash(&OUTLET_PARAM, "51:TipPercent");
    applied(&db.run(
        &OUTLET_PARAM,
        "51:TipPercent",
        &[("ParaStr", "7")],
        Some(&para),
    ));
    assert_eq!(
        db.value("SELECT ParaStr FROM dbo.Para WHERE RVC = 51 AND ParaName = 'TipPercent'"),
        "7"
    );
    assert_ne!(db.hash(&OUTLET_PARAM, "51:TipPercent"), para);
}

#[test]
fn pos_to_cloud_edit_in_pos_win_changes_the_captured_row_and_its_mirror_hash() {
    let db = seeded!();
    let area = db.hash(&TABLE_AREA, "51:1");
    let bed = db.hash(&TABLE, "51:G1");
    let para = db.hash(&OUTLET_PARAM, "51:TipPercent");
    // An unchanged row hashes the same on every read.
    assert_eq!(db.hash(&TABLE, "51:G1"), bed);
    db.exec(
        "UPDATE dbo.TableArea SET TableAreaName = N'Sân thượng' WHERE RVC = 51 AND TableArea = 1;
UPDATE dbo.TableList SET TableStatus = 1, Description = 'POS bed' WHERE RVC = 51 AND TableNum = 'G1';
UPDATE dbo.Para SET ParaStr = '15' WHERE RVC = 51 AND ParaName = 'TipPercent';",
    );
    assert_ne!(db.hash(&TABLE_AREA, "51:1"), area);
    assert_ne!(db.hash(&TABLE, "51:G1"), bed);
    assert_ne!(db.hash(&OUTLET_PARAM, "51:TipPercent"), para);
    // The other outlet's rows are not part of the change.
    assert_eq!(
        db.value("SELECT CONCAT(TableAreaName, '/', ParaStr) FROM dbo.TableArea a JOIN dbo.Para p ON p.RVC = a.RVC WHERE a.RVC = 52"),
        "Khu A/10"
    );
}

#[test]
fn conflict_pos_edit_first_refuses_the_cloud_write_and_cloud_write_first_refuses_the_older_hash() {
    let db = seeded!();
    for (entity, key, field, pos_sql, probe) in [
        (
            &TABLE_AREA,
            "51:1",
            "TableAreaName",
            "UPDATE dbo.TableArea SET TableAreaName = N'POS' WHERE RVC = 51 AND TableArea = 1;",
            "SELECT TableAreaName FROM dbo.TableArea WHERE RVC = 51 AND TableArea = 1",
        ),
        (
            &OUTLET_PARAM,
            "51:TipPercent",
            "ParaStr",
            "UPDATE dbo.Para SET ParaStr = '99' WHERE RVC = 51 AND ParaName = 'TipPercent';",
            "SELECT ParaStr FROM dbo.Para WHERE RVC = 51 AND ParaName = 'TipPercent'",
        ),
    ] {
        // POS first: the editor read the hash before the POS Win edit.
        let seen = db.hash(entity, key);
        db.exec(pos_sql);
        let pos_value = db.value(probe);
        refused(
            &db.run(entity, key, &[(field, "Cloud")], Some(&seen)),
            master_reasons::CHANGED,
        );
        assert_eq!(db.value(probe), pos_value, "{}", entity.wire);
        // Cloud first: the fresh hash wins, the second write on the old hash loses.
        let fresh = db.hash(entity, key);
        applied(&db.run(entity, key, &[(field, "Cloud")], Some(&fresh)));
        refused(
            &db.run(entity, key, &[(field, "Second")], Some(&fresh)),
            master_reasons::CHANGED,
        );
        assert_eq!(db.value(probe), "Cloud", "{}", entity.wire);
    }
}

#[test]
fn permission_branch_scope_a_write_changes_only_its_own_outlet_and_never_a_protected_parameter() {
    let db = seeded!();
    db.exec("INSERT dbo.Para (ParaName, ParaStr, ParaType, ParaDesc, RVC, Status) VALUES ('LastCheck', '120', 1, 'Seq', 51, 0);");
    let other_area = db.hash(&TABLE_AREA, "52:1");
    let other_bed = db.hash(&TABLE, "52:G1");
    let other_para = db.hash(&OUTLET_PARAM, "52:TipPercent");

    let seen = db.hash(&TABLE_AREA, "51:1");
    applied(&db.run(
        &TABLE_AREA,
        "51:1",
        &[("TableAreaName", "Chi nhánh 51")],
        Some(&seen),
    ));
    let seen = db.hash(&TABLE, "51:G1");
    applied(&db.run(
        &TABLE,
        "51:G1",
        &[("Description", "Outlet 51")],
        Some(&seen),
    ));
    let seen = db.hash(&OUTLET_PARAM, "51:TipPercent");
    applied(&db.run(
        &OUTLET_PARAM,
        "51:TipPercent",
        &[("ParaStr", "3")],
        Some(&seen),
    ));

    assert_eq!(db.hash(&TABLE_AREA, "52:1"), other_area);
    assert_eq!(db.hash(&TABLE, "52:G1"), other_bed);
    assert_eq!(db.hash(&OUTLET_PARAM, "52:TipPercent"), other_para);

    // An outlet that has no such row cannot be written through another outlet's key, and a
    // protected parameter is never written.
    refused(
        &db.run(&TABLE_AREA, "53:1", &[("TableAreaName", "x")], Some("any")),
        master_reasons::ROW_NOT_FOUND,
    );
    refused(
        &db.run(
            &OUTLET_PARAM,
            "51:LastCheck",
            &[("ParaStr", "1")],
            Some("any"),
        ),
        master_reasons::ROW_PROTECTED,
    );
    assert_eq!(
        db.value("SELECT ParaStr FROM dbo.Para WHERE RVC = 51 AND ParaName = 'LastCheck'"),
        "120"
    );
}

#[test]
fn audit_row_each_command_leaves_one_log_row_and_a_redelivery_does_not_write_again() {
    let db = seeded!();
    let seen = db.hash(&TABLE_AREA, "51:1");
    let cmd = upsert(
        &TABLE_AREA,
        "51:1",
        &[("TableAreaName", "Audited")],
        Some(&seen),
    );
    let run = |c: &SyncCommand| db.executor().execute(c, &Ctx::background()).unwrap();
    assert_eq!(db.log_rows(cmd.command_id), "0");
    applied(&run(&cmd));
    assert_eq!(db.log_rows(cmd.command_id), "1");
    assert_eq!(
        db.value(&format!(
            "SELECT Status FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{}'",
            cmd.command_id
        )),
        "Applied"
    );
    // A POS edit after the first apply survives the redelivery of the same command.
    db.exec("UPDATE dbo.TableArea SET TableAreaName = N'POS' WHERE RVC = 51 AND TableArea = 1;");
    applied(&run(&cmd));
    assert_eq!(db.log_rows(cmd.command_id), "1");
    assert_eq!(
        db.value("SELECT TableAreaName FROM dbo.TableArea WHERE RVC = 51 AND TableArea = 1"),
        "POS"
    );
    // A refused command is logged too.
    let stale = upsert(
        &TABLE_AREA,
        "51:1",
        &[("TableAreaName", "Late")],
        Some(&seen),
    );
    refused(&run(&stale), master_reasons::CHANGED);
    assert_eq!(db.log_rows(stale.command_id), "1");
}
