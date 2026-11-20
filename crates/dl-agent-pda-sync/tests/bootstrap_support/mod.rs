//! Bootstrap fakes ported from `tests/MM.PdaOrder.SyncAgent.Tests`: `FakeSqlAdmin` (an
//! in-memory SQL admin and discovery that remembers created objects), `FakeLoginProbe`,
//! `FakeCloudProbe` and the grants probe of `GrantsCommandTests`.

#![allow(dead_code)]

use dl_agent_pda_sync::bootstrap::cloud_probe::{CloudProbe, CloudProbeResult};
use dl_agent_pda_sync::bootstrap::error::Result;
use dl_agent_pda_sync::bootstrap::workflow::{EnrollParts, EnrollWorkflow, HttpFactory};
use dl_agent_pda_sync::bootstrap::{
    AgentLoginProbe, BootstrapStep, Error, OrderGrantsProbe, STEPS, SchemaFingerprint, SqlAdmin,
    SqlBootstrapper, SqlInstance, SqlInstanceDiscovery, SqlServerInfo, StepKind, TableSet,
};
use dl_agent_pda_sync::config::{AgentError, codes};
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::security::SharedSecretStore;
use dl_agent_pda_sync::transport::{RequestSigner, SyncHttpClient};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

/// What the fake SQL Server holds.
pub struct SqlState {
    pub instances: Vec<SqlInstance>,
    /// Database name (compared ignoring case) and its tables.
    pub databases: BTreeMap<String, TableSet>,
    pub created: HashSet<String>,
    pub executed: Vec<String>,
    pub deny_access: bool,
    /// The password SQL Server holds for `pda_sync`, set by the login script.
    pub login_password: Option<String>,
    /// The setup identity lacks ALTER ANY LOGIN: the login script fails (error 15247).
    pub deny_login_step: bool,
    /// SQL Server 2022, mixed-mode authentication by default.
    pub server: SqlServerInfo,
}

/// In-memory SQL admin + discovery.
#[derive(Clone)]
pub struct FakeSqlAdmin(pub Arc<Mutex<SqlState>>);

pub fn catalog_tables() -> TableSet {
    SyncCatalog::load()
        .unwrap()
        .classified()
        .map(|t| t.table.clone())
        .collect()
}

impl FakeSqlAdmin {
    pub fn new() -> FakeSqlAdmin {
        let mut databases = BTreeMap::new();
        databases.insert("SMILE_POS".to_owned(), catalog_tables());
        databases.insert("master".to_owned(), TableSet::new());
        FakeSqlAdmin(Arc::new(Mutex::new(SqlState {
            instances: vec![SqlInstance::new(r".\SQLEXPRESS", "test")],
            databases,
            created: HashSet::new(),
            executed: Vec::new(),
            deny_access: false,
            login_password: None,
            deny_login_step: false,
            server: SqlServerInfo::new("16.0.4135.4", "Developer Edition (64-bit)", "RTM", false),
        })))
    }

    pub fn st(&self) -> MutexGuard<'_, SqlState> {
        self.0.lock().unwrap()
    }

    pub fn executed(&self) -> Vec<String> {
        self.st().executed.clone()
    }

    fn check(&self) -> Result<()> {
        if self.st().deny_access {
            return Err(Error::Agent(AgentError::new(
                codes::SQL_NO_ACCESS,
                "denied",
            )));
        }
        Ok(())
    }

    pub fn key(database: &str, step: &BootstrapStep) -> String {
        format!(
            "{}/{}#{}",
            if step.server_level {
                "server"
            } else {
                database
            },
            step.object_name,
            step.kind.name()
        )
    }
}

fn matches(step: &BootstrapStep, script: &str) -> bool {
    let grant = script.contains("GRANT");
    match step.kind {
        StepKind::Login => script.contains("CREATE LOGIN"),
        StepKind::User => script.contains("CREATE USER"),
        StepKind::Grants => grant && script.contains("Pda_SyncState"),
        StepKind::CommandGrants => grant && script.contains("dbo.TableReservation"),
        StepKind::OrderGrants => grant && script.contains("dbo.PostITrn"),
        StepKind::OrderGrantsOw1c => grant && script.contains("dbo.tmpInvoice (NGuest"),
        StepKind::OrderGrantsOw1e => grant && script.contains("dbo.tmpTrn (CheckNo"),
        StepKind::Table => script.contains(&format!("CREATE TABLE dbo.{}", step.object_name)),
    }
}

impl SqlInstanceDiscovery for FakeSqlAdmin {
    fn discover(&self, _ctx: &Ctx) -> Result<Vec<SqlInstance>> {
        Ok(self.st().instances.clone())
    }
}

impl SqlAdmin for FakeSqlAdmin {
    fn list_databases(&self, _i: &SqlInstance, _ctx: &Ctx) -> Result<Vec<String>> {
        self.check()?;
        Ok(self.st().databases.keys().cloned().collect())
    }

    fn list_tables(&self, _i: &SqlInstance, database: &str, _ctx: &Ctx) -> Result<TableSet> {
        self.check()?;
        let st = self.st();
        Ok(st
            .databases
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(database))
            .map(|(_, v)| v.clone())
            .expect("KeyNotFoundException"))
    }

    fn exists(
        &self,
        _i: &SqlInstance,
        database: &str,
        step: &BootstrapStep,
        _ctx: &Ctx,
    ) -> Result<bool> {
        self.check()?;
        Ok(self
            .st()
            .created
            .contains(&FakeSqlAdmin::key(database, step)))
    }

    fn execute(&self, _i: &SqlInstance, database: &str, script: &str, _ctx: &Ctx) -> Result<()> {
        self.check()?;
        let steps: Vec<&BootstrapStep> = STEPS
            .iter()
            .filter(|s| script.contains(s.object_name) && matches(s, script))
            .collect();
        assert_eq!(steps.len(), 1, "one step per script: {script}");
        let mut st = self.st();
        if steps[0].kind == StepKind::Login {
            if st.deny_login_step {
                return Err(Error::Agent(AgentError::new(
                    codes::SQL_NO_ACCESS,
                    "SQL setup failed (error 15247).",
                )));
            }
            st.login_password = login_password(script);
        }
        st.created.insert(FakeSqlAdmin::key(database, steps[0]));
        st.executed.push(format!("{}:{script}", steps[0].id));
        Ok(())
    }

    fn server_info(&self, _i: &SqlInstance, _ctx: &Ctx) -> Result<SqlServerInfo> {
        self.check()?;
        Ok(self.st().server.clone())
    }

    fn signs_in(&self, _i: &SqlInstance, password: &str, _ctx: &Ctx) -> Result<bool> {
        Ok(self.st().login_password.as_deref() == Some(password))
    }
}

/// The password the (last statement of the) login script sets.
fn login_password(script: &str) -> Option<String> {
    let start = script.rfind("PASSWORD = N'")? + "PASSWORD = N'".len();
    let len = script[start..].find('\'')?;
    Some(script[start..start + len].to_owned())
}

/// Records each `pda_sync` login check; fails with `failure` when set.
#[derive(Default)]
pub struct FakeLoginProbe {
    pub calls: Mutex<Vec<(String, String, String)>>,
    pub failure: Mutex<Option<AgentError>>,
}

impl AgentLoginProbe for FakeLoginProbe {
    fn verify(
        &self,
        instance: &SqlInstance,
        database: &str,
        password: &str,
        _ctx: &Ctx,
    ) -> Result<()> {
        self.calls.lock().unwrap().push((
            instance.server_name.clone(),
            database.to_owned(),
            password.to_owned(),
        ));
        match self.failure.lock().unwrap().clone() {
            Some(e) => Err(Error::Agent(e)),
            None => Ok(()),
        }
    }
}

/// Answers every probe with `result` (reachable, in sync by default) and records URLs.
pub struct FakeCloudProbe {
    pub calls: Mutex<Vec<String>>,
    pub result: Mutex<CloudProbeResult>,
}

impl Default for FakeCloudProbe {
    fn default() -> FakeCloudProbe {
        FakeCloudProbe {
            calls: Mutex::new(Vec::new()),
            result: Mutex::new(CloudProbeResult {
                findings: Vec::new(),
                http_status: Some(200),
                clock_skew: Some(10_000_000),
            }),
        }
    }
}

impl CloudProbe for FakeCloudProbe {
    fn probe(&self, base_url: &str, _ctx: &Ctx) -> CloudProbeResult {
        self.calls.lock().unwrap().push(base_url.to_owned());
        self.result.lock().unwrap().clone()
    }
}

/// The grants probe of `GrantsCommandTests`: answers `missing`.
#[derive(Default)]
pub struct FakeGrantsProbe {
    pub missing: Mutex<Vec<String>>,
}

impl OrderGrantsProbe for FakeGrantsProbe {
    fn missing(&self, _i: &str, _d: &str, _p: &str, _ctx: &Ctx) -> Result<Vec<String>> {
        Ok(self.missing.lock().unwrap().clone())
    }
}

/// `TestEnv.NewWorkflow`: the fakes, the env's clock and recording retry policy, no
/// service recovery.
pub fn new_workflow(
    env: &crate::support::TestEnv,
    sql: &FakeSqlAdmin,
    login: Arc<FakeLoginProbe>,
    cloud: Arc<FakeCloudProbe>,
) -> EnrollWorkflow {
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
    EnrollWorkflow::new(EnrollParts {
        paths: env.paths.clone(),
        secrets: env.secrets.clone(),
        bootstrapper: Arc::new(new_bootstrapper(sql, &env.secrets)),
        login_probe: login,
        cloud_probe: cloud,
        http,
        log: env.log.clone(),
        clock: env.time.clone(),
        service_recovery: Arc::new(|_| {}),
    })
}

/// `TestEnv.NewBootstrapper`.
pub fn new_bootstrapper(sql: &FakeSqlAdmin, secrets: &SharedSecretStore) -> SqlBootstrapper {
    SqlBootstrapper::new(
        Arc::new(sql.clone()),
        Arc::new(sql.clone()),
        SchemaFingerprint::new(SyncCatalog::load().unwrap(), 0.85),
        secrets.clone(),
        &Logger::none(),
    )
}
