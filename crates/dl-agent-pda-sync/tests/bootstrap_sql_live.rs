//! `SqlPreflightIntegrationTests` and a live `enroll` against a real SQL Server. Gated on
//! `PDA_SYNC_TEST_SQL` (a dl-tds URL of a SQL login that may create databases and
//! logins). The test creates a `pda_sync_test_*` database shaped like SMILE_POS and the
//! server login `pda_sync` (skipped when that login already exists), and drops both.

mod bootstrap_support;
mod support;

use bootstrap_support::FakeCloudProbe;
use dl_agent_pda_sync::bootstrap::admin::{SqlAdminCredential, SqlServerAdmin};
use dl_agent_pda_sync::bootstrap::discovery::LocalSqlDiscovery;
use dl_agent_pda_sync::bootstrap::workflow::{
    EnrollArgs, EnrollParts, EnrollWorkflow, HttpFactory,
};
use dl_agent_pda_sync::bootstrap::{
    AgentLoginProbe, OrderGrantsProbe, SchemaFingerprint, SqlAgentLoginProbe, SqlBootstrapper,
    SqlInstance, SqlOrderGrantsProbe, SqlServerInfo, check_server,
};
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::sql::agent_connection;
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::reporting::{Logger, NullRunStages};
use dl_agent_pda_sync::security::names;
use dl_agent_pda_sync::transport::{RequestSigner, SyncHttpClient};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::sync::Arc;
use support::{FakeSyncServer, TestEnv};

fn admin_url() -> Option<String> {
    std::env::var("PDA_SYNC_TEST_SQL")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// A throwaway SMILE_POS-shaped database: every catalog table, plus the vendor columns,
/// procedures and function the grant scripts name.
struct Pos {
    admin: dl_tds::Config,
    instance: String,
    database: String,
    created_login: bool,
}

const VENDOR: &str = "CREATE TABLE dbo.TmpInvoice (CheckNo int NOT NULL PRIMARY KEY, NGuest int NULL, NChild int NULL, LastChgCashier int NULL, LastChgTime datetime NULL, TableNo int NULL);
CREATE TABLE dbo.tmpInvoiceInfo (CheckNo int NULL);
CREATE TABLE dbo.checkopenlog (CheckNo int NULL, isOpen bit NULL, CloseTime datetime NULL, LastAction int NULL);
CREATE TABLE dbo.tmpTrn (TrnSeq int NULL, CheckNo int NULL, Rsv2 int NULL, DscRef int NULL);
CREATE TABLE dbo.tmpTrnInfo (CheckNo int NULL, IsDone bit NULL, DoneTime datetime NULL, PToOrder int NULL, POrderTime datetime NULL, OrderQty int NULL, Void bit NULL, TableNo int NULL);
CREATE TABLE dbo.VoidTrn (CheckNo int NULL);
CREATE TABLE dbo.TableReservation (nID int NULL);
INSERT dbo.TmpInvoice (CheckNo) VALUES (1);";

const ROUTINES: [&str; 4] = [
    "CREATE PROCEDURE dbo.GetRVCSeqNumber AS RETURN 0;",
    "CREATE PROCEDURE dbo.PostITrn AS RETURN 0;",
    "CREATE PROCEDURE dbo.UpdateTmpInvoice AS RETURN 0;",
    "CREATE FUNCTION dbo.Unicode2TCVN (@s nvarchar(4000)) RETURNS nvarchar(4000) AS BEGIN RETURN @s; END;",
];

impl Pos {
    fn create(url: &str) -> Option<Pos> {
        let admin: dl_tds::Config = url.parse().expect("PDA_SYNC_TEST_SQL is a dl-tds URL");
        let admin = admin.with_database("master");
        let instance = match admin.instance() {
            Some(i) => format!("{}\\{i}", admin.host()),
            None => format!("{},{}", admin.host(), admin.port().unwrap_or(1433)),
        };
        let tag = dl_uuid::Uuid::new_v4().simple().to_string();
        let mut pos = Pos {
            admin,
            instance,
            database: format!("pda_sync_test_{}", &tag[..12]),
            created_login: false,
        };
        let mut c = dl_tds::Client::connect(&pos.admin).unwrap();
        let r = c.query("SELECT SUSER_ID(N'pda_sync')", &[]).unwrap();
        if r.rows()[0].get::<Option<i32>>(0).unwrap().is_some() {
            eprintln!("skipped: the server already has a pda_sync login");
            return None;
        }
        c.batch(&format!("CREATE DATABASE [{}];", pos.database))
            .unwrap();
        c.close();
        pos.created_login = true;
        pos.exec(VENDOR);
        for r in ROUTINES {
            pos.exec(r);
        }
        let mut ddl = String::new();
        for t in SyncCatalog::load().unwrap().classified() {
            let name = t.table.replace(']', "]]");
            ddl.push_str(&format!(
                "IF OBJECT_ID(N'dbo.[{name}]', N'U') IS NULL CREATE TABLE dbo.[{name}] (Id int NULL);\n"
            ));
        }
        pos.exec(&ddl);
        Some(pos)
    }

    fn exec(&self, sql: &str) {
        let mut c =
            dl_tds::Client::connect(&self.admin.clone().with_database(&self.database)).unwrap();
        c.batch(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        c.close();
    }

    fn sa(&self) -> SqlAdminCredential {
        SqlAdminCredential::sql(self.admin.user(), self.admin.password())
    }
}

impl Drop for Pos {
    fn drop(&mut self) {
        if let Ok(mut c) = dl_tds::Client::connect(&self.admin) {
            let mut sql = format!(
                "IF DB_ID(N'{db}') IS NOT NULL BEGIN ALTER DATABASE [{db}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{db}]; END;",
                db = self.database
            );
            if self.created_login {
                sql.push_str("\nIF SUSER_ID(N'pda_sync') IS NOT NULL DROP LOGIN [pda_sync];");
            }
            let _ = c.batch(&sql);
            c.close();
        }
    }
}

#[test]
fn a_live_enroll_bootstraps_a_real_server_and_the_preflight_reads_it() {
    let Some(url) = admin_url() else {
        eprintln!("skipped: PDA_SYNC_TEST_SQL is not set");
        return;
    };
    let Some(pos) = Pos::create(&url) else { return };
    let instance = SqlInstance::new(pos.instance.clone(), "argument");
    let ctx = Ctx::background();
    let env = TestEnv::new();
    let server = FakeSyncServer::start(env.time.clone());

    // A wrong password is a login verify failure that says the code was not used.
    let wrong = format!("Pwrong{}", dl_uuid::Uuid::new_v4().simple());
    let e = SqlAgentLoginProbe
        .verify(&instance, &pos.database, &wrong, &ctx)
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::SQL_LOGIN_VERIFY_FAILED));
    assert!(e.to_string().contains("18456"), "{e}");
    assert!(e.to_string().contains("The enrollment code was not used."));

    // The setup identity lists, reads the tables and the server.
    let admin = Arc::new(SqlServerAdmin::new(pos.sa()));
    use dl_agent_pda_sync::bootstrap::SqlAdmin;
    assert!(
        admin
            .list_databases(&instance, &ctx)
            .unwrap()
            .contains(&pos.database)
    );
    assert!(
        admin
            .list_tables(&instance, &pos.database, &ctx)
            .unwrap()
            .contains("TmpInvoice")
    );
    let info: SqlServerInfo = admin.server_info(&instance, &ctx).unwrap();
    assert!(info.major() >= 16, "{info}");
    assert!(!info.edition.trim().is_empty() && !info.product_level.trim().is_empty());
    assert!(
        !info.integrated_security_only,
        "the container accepts SQL logins"
    );
    assert!(check_server(&instance, &info).is_empty());

    // A refused administrator login names --sql-admin-user.
    let refused = SqlServerAdmin::new(SqlAdminCredential::sql(pos.admin.user(), &wrong));
    let e = refused.list_databases(&instance, &ctx).unwrap_err();
    assert_eq!(e.code(), Some(codes::SQL_LOGIN_FAILED));
    assert!(e.to_string().contains("--sql-admin-user"), "{e}");

    // The real enroll: bootstrap, sign-in as pda_sync, redeem.
    let bootstrapper = Arc::new(SqlBootstrapper::new(
        Arc::new(LocalSqlDiscovery::new(&Logger::none())),
        admin.clone(),
        SchemaFingerprint::new(SyncCatalog::load().unwrap(), 0.85),
        env.secrets.clone(),
        &env.log,
    ));
    let (time, retry, log) = (env.time.clone(), env.retry.clone(), env.log.clone());
    let http: HttpFactory = Arc::new(move |base, key| {
        SyncHttpClient::new(
            base,
            std::time::Duration::from_secs(30),
            Some(RequestSigner::new(key, time.clone())),
            retry.clone(),
            &log,
        )
    });
    let workflow = EnrollWorkflow::new(EnrollParts {
        paths: env.paths.clone(),
        secrets: env.secrets.clone(),
        bootstrapper: bootstrapper.clone(),
        login_probe: Arc::new(SqlAgentLoginProbe),
        cloud_probe: Arc::new(FakeCloudProbe::default()),
        http,
        log: env.log.clone(),
        clock: env.time.clone(),
        service_recovery: Arc::new(|_| {}),
    });
    let args = EnrollArgs {
        code: Some("CODE-1234".into()),
        url: Some(server.base.clone()),
        dry_run: false,
        sql_instance: Some(pos.instance.clone()),
        database: Some(pos.database.clone()),
    };
    let r = workflow
        .run(&args, "TEST-PC", "1.0.0-test", &NullRunStages, &ctx)
        .unwrap_or_else(|e| panic!("{e}"));
    let sql = r.sql.unwrap();
    assert_eq!(sql.database, pos.database);
    assert!(sql.steps.iter().all(|s| s.executed), "{:?}", sql.steps);
    assert_eq!(server.with(|s| s.valid_code.clone()), "USED");
    assert!(r.identity.is_some());

    // The runtime login reads the server version and the order grants are in place.
    let pw = String::from_utf8(env.secrets.load(names::SQL_PASSWORD).unwrap().unwrap()).unwrap();
    let mut c =
        dl_tds::Client::connect(&agent_connection(&pos.instance, &pos.database, &pw)).unwrap();
    let runtime = SqlServerInfo::read(&mut c).unwrap();
    c.close();
    assert_eq!(runtime, info);
    assert!(
        SqlOrderGrantsProbe
            .missing(&pos.instance, &pos.database, &pw, &ctx)
            .unwrap()
            .is_empty()
    );

    // A second bootstrap (the grants verb) finds everything in place and keeps the password.
    let again = bootstrapper
        .run(false, Some(&pos.instance), Some(&pos.database), &ctx)
        .unwrap();
    assert!(again.steps.iter().all(|s| s.skipped), "{:?}", again.steps);
    assert_eq!(
        String::from_utf8(env.secrets.load(names::SQL_PASSWORD).unwrap().unwrap()).unwrap(),
        pw
    );

    // The login check reads both agent tables: without one it names it.
    pos.exec("DROP TABLE dbo.Pda_SyncCommandLog;");
    let e = SqlAgentLoginProbe
        .verify(&instance, &pos.database, &pw, &ctx)
        .unwrap_err();
    assert_eq!(e.code(), Some(codes::SQL_LOGIN_VERIFY_FAILED));
    assert!(
        e.to_string().contains("Pda_SyncCommandLog") && e.to_string().contains("208"),
        "{e}"
    );

    // A probe that cannot sign in is the escaped SQL failure grants reports.
    let e = SqlOrderGrantsProbe
        .missing(&pos.instance, &pos.database, &wrong, &ctx)
        .unwrap_err();
    assert_eq!(e.sql().map(|f| f.number()), Some(18456));
}
