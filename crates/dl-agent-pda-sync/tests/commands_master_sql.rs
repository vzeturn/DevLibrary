//! `master.upsert` on a real SQL Server (ADR 0147): write-down, store-first and
//! cloud-first conflicts, replay, rejections, and the `110_master_grants.sql` column grant
//! exercised as the `pda_sync` database user. Gated on `PDA_SYNC_TEST_SQL` (a dl-tds URL
//! of a login that may create databases); each test creates its own `CT_RT0844_*`
//! database and drops it afterwards. The executor runs as that login; the grants test
//! impersonates a user without login.

use std::sync::Arc;

use dl_agent_pda_sync::capture::plan;
use dl_agent_pda_sync::capture::{
    CaptureSource, SourceQuery, SqlServerCaptureSource, SqlServerSourceOptions,
};
use dl_agent_pda_sync::commands::{ConfigSource, SiteCommandExecutor, SqlSiteCommandStore};
use dl_agent_pda_sync::config::SystemClock;
use dl_agent_pda_sync::contracts::mapping::{CatalogRowMapper, MapperOptions, RawValue};
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
            database: format!("CT_RT0844_{}", &tag[..10]),
        };
        s.exec_in("master", &format!("CREATE DATABASE [{}];", s.database));
        s.exec(COMMAND_LOG);
        s.exec(
            "CREATE TABLE dbo.IClassDef (ClsCode int NOT NULL PRIMARY KEY, ClsName varchar(50) NULL, ClsEName varchar(50) NULL,
    LastChangeBy varchar(3) NULL, LastChangeTime datetime NULL);
INSERT dbo.IClassDef VALUES (7, 'Old name', 'Old en', 'POS', '2026-11-01T08:00:00'), (8, 'Other', 'Other en', 'POS', '2026-11-01T08:00:00');",
        );
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
        let v: String = r.rows()[0].get(0).unwrap();
        c.close();
        v
    }

    fn name(&self, key: i32) -> String {
        self.text(&format!(
            "SELECT ClsName FROM dbo.IClassDef WHERE ClsCode = {key}"
        ))
    }

    fn count(&self, sql: &str) -> i64 {
        let mut c = dl_tds::Client::connect(&self.config()).unwrap();
        let r = c.query(sql, &[]).unwrap();
        let n: i64 = r.rows()[0].get(0).unwrap();
        c.close();
        n
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

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        self.executor().execute(c, &Ctx::background()).unwrap()
    }

    /// The editor's read: capture's own source (live columns, TCVN3 as bytes), then the
    /// mirror's hash of that row.
    fn capture_hash(&self, key: i32) -> String {
        let m = mapper();
        let table = m.catalog().find("IClassDef").unwrap();
        let source = SqlServerCaptureSource::new(SqlServerSourceOptions::new(self.config()));
        let mut q = SourceQuery::new("IClassDef");
        q.exclude_columns = Some(plan::exclude_of(m.catalog(), table));
        q.bytes_columns = table
            .transforms
            .as_ref()
            .and_then(|t| t.tcvn3_decode.clone());
        q.where_ = Some(vec![("ClsCode".into(), RawValue::I32(key))]);
        let rows = source.query(&q, &Ctx::background()).unwrap();
        assert_eq!(rows.len(), 1);
        m.canonicalize("IClassDef", &rows[0]).unwrap().hash
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

fn upsert(key: &str, fields: &str, expected: Option<&str>) -> SyncCommand {
    let expected = expected.map_or(String::new(), |h| format!(r#","expected_hash":"{h}""#));
    let payload = stj::parse(&format!(
        r#"{{"entity":"item_class","key":"{key}","fields":{fields}{expected},"actor":"office-1"}}"#
    ))
    .unwrap();
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::MASTER_UPSERT.into(),
        aggregate_key: format!("master:item_class:{key}"),
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

fn rename(key: i32, name: &str, expected: Option<&str>) -> SyncCommand {
    upsert(
        &key.to_string(),
        &format!(r#"{{"ClsName":"{name}"}}"#),
        expected,
    )
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn applied(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
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
fn write_down_applies_and_the_result_hash_is_the_hash_capture_will_mirror() {
    let db = store!();
    let seen = db.capture_hash(7);
    let res = applied(&db.run(&rename(7, "New name", Some(&seen))));
    assert_eq!(db.name(7), "New name");
    assert_eq!(res.row_hash, db.capture_hash(7));
    assert_ne!(res.row_hash, seen);
    // Only the name and the change stamp moved; LastChangeBy is untouched.
    assert_eq!(
        db.text("SELECT LastChangeBy FROM dbo.IClassDef WHERE ClsCode = 7"),
        "POS"
    );
    assert_eq!(
        db.text("SELECT ClsEName FROM dbo.IClassDef WHERE ClsCode = 7"),
        "Old en"
    );
    assert_eq!(db.name(8), "Other");
}

#[test]
fn store_first_edit_after_the_editor_read_is_refused_and_the_row_is_unchanged() {
    let db = store!();
    let seen = db.capture_hash(7);
    db.exec("UPDATE dbo.IClassDef SET ClsName = 'POS edit', LastChangeTime = GETDATE() WHERE ClsCode = 7");
    refused(
        &db.run(&rename(7, "Cloud edit", Some(&seen))),
        "master_changed",
    );
    assert_eq!(db.name(7), "POS edit");
}

#[test]
fn cloud_first_then_a_pos_edit_is_not_blocked_and_the_stale_cloud_edit_is_refused() {
    let db = store!();
    let first = applied(&db.run(&rename(7, "Cloud name", Some(&db.capture_hash(7)))));
    db.exec("UPDATE dbo.IClassDef SET ClsName = 'POS name', LastChangeTime = GETDATE() WHERE ClsCode = 7");
    assert_eq!(db.name(7), "POS name");
    refused(
        &db.run(&rename(7, "Stale", Some(&first.row_hash))),
        "master_changed",
    );
    assert_eq!(db.name(7), "POS name");
    applied(&db.run(&rename(7, "After POS", Some(&db.capture_hash(7)))));
    assert_eq!(db.name(7), "After POS");
}

#[test]
fn a_redelivered_command_id_answers_the_logged_result_and_writes_once() {
    let db = store!();
    let cmd = rename(7, "New name", Some(&db.capture_hash(7)));
    let first = db.run(&cmd);
    // The second delivery would be refused (the row moved), so equal results prove the
    // log answered it.
    let again = db.run(&cmd);
    assert_eq!(first, again);
    assert_eq!(db.name(7), "New name");
    assert_eq!(
        db.count(&format!(
            "SELECT CAST(COUNT(*) AS bigint) FROM dbo.Pda_SyncCommandLog WHERE CommandId = '{}'",
            cmd.command_id
        )),
        1
    );
}

#[test]
fn rejections_leave_the_row_alone() {
    let db = store!();
    let h = db.capture_hash(7);
    refused(&db.run(&rename(99, "X", Some(&h))), "master_row_not_found");
    refused(&db.run(&rename(7, "X", None)), "master_create_unsupported");
    refused(
        &db.run(&upsert("7", r#"{"ClsName":"X","ClsEName":"x"}"#, Some(&h))),
        "master_field_not_allowed",
    );
    let mut other = upsert("7", r#"{"ClsName":"X"}"#, Some(&h));
    other.payload = stj::parse(
        r#"{"entity":"cashier","key":"7","fields":{"ClsName":"X"},"expected_hash":"h","actor":"a"}"#,
    )
    .unwrap();
    refused(&db.run(&other), "master_unsupported_entity");
    for (fields, why) in [
        (r#"{"ClsName":"Ca phê"}"#, "ClsName must be ASCII"),
        (r#"{"ClsName":"a\u0001b"}"#, "ClsName must be ASCII"),
    ] {
        let r = db.run(&upsert("7", fields, Some(&h)));
        assert_eq!(
            r.reason.as_deref(),
            Some(format!("invalid_payload: {why}").as_str())
        );
    }
    let mut bad = rename(7, "X", Some(&h));
    bad.payload =
        stj::parse(r#"{"entity":"item_class","key":"7","fields":{},"actor":"a"}"#).unwrap();
    assert!(
        db.run(&bad)
            .reason
            .unwrap()
            .starts_with("invalid_payload: ")
    );
    assert_eq!(db.name(7), "Old name");
}

#[test]
fn the_master_grants_let_pda_sync_write_only_the_name_and_the_change_stamp() {
    let db = store!();
    db.exec(
        "CREATE USER [pda_sync] WITHOUT LOGIN; ALTER ROLE db_datareader ADD MEMBER [pda_sync];",
    );
    db.exec(MASTER_GRANTS);
    // The statement the executor runs, as pda_sync.
    db.exec(
        "EXECUTE AS USER = 'pda_sync';
SELECT ClsName FROM dbo.IClassDef WITH (UPDLOCK, ROWLOCK) WHERE ClsCode = 7;
UPDATE dbo.IClassDef SET ClsName = 'As agent', LastChangeTime = GETDATE() WHERE ClsCode = 7;
REVERT;",
    );
    assert_eq!(db.name(7), "As agent");
    for denied in [
        "UPDATE dbo.IClassDef SET ClsEName = 'x' WHERE ClsCode = 7",
        "UPDATE dbo.IClassDef SET LastChangeBy = 'x' WHERE ClsCode = 7",
        "DELETE dbo.IClassDef WHERE ClsCode = 7",
    ] {
        let mut c = dl_tds::Client::connect(&db.config()).unwrap();
        c.batch("EXECUTE AS USER = 'pda_sync';").unwrap();
        let err = c.batch(denied).expect_err(denied);
        assert!(err.to_string().contains("permission"), "{denied}: {err}");
        c.close();
    }
    // Running the script again is a no-op (idempotent).
    db.exec(MASTER_GRANTS);
}
