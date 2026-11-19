//! `EnrollPreflightTests`, the enrollment cases of `EnrollmentTests`, `EnrollCommandTests`
//! and `GrantsCommandTests`: the workflow and the verbs against the fake SQL admin, the
//! fake probes and the fake cloud.

mod bootstrap_support;
mod support;

use bootstrap_support::{
    FakeCloudProbe, FakeGrantsProbe, FakeLoginProbe, FakeSqlAdmin, new_bootstrapper, new_workflow,
};
use dl_agent_pda_sync::bootstrap::cloud_probe::CloudProbeResult;
use dl_agent_pda_sync::bootstrap::workflow::{
    CLOUD_PROBED, EnrollArgs, EnrollResult, LOGIN_VERIFIED, REDEEMED, SQL_BOOTSTRAPPED,
    SQL_LOCATED, SQL_PLANNED,
};
use dl_agent_pda_sync::bootstrap::{Error, PreflightFinding, SqlServerInfo};
use dl_agent_pda_sync::config::time::SharedClock;
use dl_agent_pda_sync::config::{AgentError, AgentIdentity, AgentStatus, codes};
use dl_agent_pda_sync::contracts::wire::{SiteEvent, site_event_levels as lv, site_event_sources};
use dl_agent_pda_sync::hosting::enroll::{self, PREFLIGHT_FAILED, PREFLIGHT_OK};
use dl_agent_pda_sync::hosting::grants::{self, GrantsArgs};
use dl_agent_pda_sync::hosting::{EXIT_FAILED, EXIT_OK, EXIT_USAGE};
use dl_agent_pda_sync::reporting::tool_run::Console;
use dl_agent_pda_sync::reporting::{
    NullRunStages, SiteEventQueueStats, SiteEventSendResult, SiteEventSendStatus,
    SiteEventTransport, ToolRun, ToolRunOptions,
};
use dl_agent_pda_sync::security::names;
use dl_agent_pda_sync::util::console::{LineWriter, MemoryLines};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::sync::{Arc, Mutex};
use support::{FakeSyncServer, TestEnv};

struct E {
    env: TestEnv,
    sql: FakeSqlAdmin,
    login: Arc<FakeLoginProbe>,
    cloud: Arc<FakeCloudProbe>,
    server: FakeSyncServer,
    /// Stands for the cloud the default transport reaches once enrolled.
    default_transport: Arc<ScriptedTransport>,
}

enum Code {
    Valid,
    None,
    Is(&'static str),
}

impl E {
    fn new() -> E {
        let env = TestEnv::new();
        let server = FakeSyncServer::start(env.time.clone());
        E {
            env,
            sql: FakeSqlAdmin::new(),
            login: Arc::default(),
            cloud: Arc::default(),
            server,
            default_transport: ScriptedTransport::new(),
        }
    }

    fn valid_code(&self) -> String {
        self.server.with(|s| s.valid_code.clone())
    }

    fn args(&self, dry_run: bool, url: Option<&str>, code: Code) -> EnrollArgs {
        EnrollArgs {
            code: match code {
                Code::Valid => Some(self.valid_code()),
                Code::None => None,
                Code::Is(c) => Some(c.to_owned()),
            },
            url: url
                .map(str::to_owned)
                .or_else(|| (!dry_run).then(|| self.server.base.clone())),
            dry_run,
            sql_instance: None,
            database: None,
        }
    }

    fn enroll_with(&self, args: &EnrollArgs) -> Result<EnrollResult, Error> {
        new_workflow(&self.env, &self.sql, self.login.clone(), self.cloud.clone()).run(
            args,
            "TEST-PC",
            "1.0.0-test",
            &NullRunStages,
            &Ctx::background(),
        )
    }

    fn enroll(&self, dry_run: bool, url: Option<&str>, code: Code) -> Result<EnrollResult, Error> {
        self.enroll_with(&self.args(dry_run, url, code))
    }

    fn code_is_unused(&self) {
        assert_eq!(self.valid_code(), "CODE-1234");
        assert!(self.env.secrets.load(names::AGENT_TOKEN).unwrap().is_none());
        assert!(AgentIdentity::try_load(&self.env.paths).unwrap().is_none());
    }

    fn set_server(&self, f: impl FnOnce(&mut SqlServerInfo)) {
        f(&mut self.sql.st().server);
    }
}

fn code_of(r: Result<EnrollResult, Error>) -> String {
    r.unwrap_err().code().unwrap_or("<unexpected>").to_owned()
}

// ---- EnrollPreflightTests ----

#[test]
fn windows_only_authentication_refuses_a_real_run_before_any_change() {
    let e = E::new();
    e.set_server(|s| s.integrated_security_only = true);
    assert_eq!(
        code_of(e.enroll(false, None, Code::Valid)),
        codes::SQL_WINDOWS_AUTH_ONLY
    );
    assert!(e.sql.executed().is_empty());
    assert!(e.login.calls.lock().unwrap().is_empty());
    e.code_is_unused();
}

#[test]
fn windows_only_authentication_is_a_failed_finding_in_a_dry_run() {
    let e = E::new();
    e.set_server(|s| s.integrated_security_only = true);
    let r = e.enroll(true, None, Code::Valid).unwrap();
    assert!(!r.preflight_ok());
    assert_eq!(r.failed_codes(), [codes::SQL_WINDOWS_AUTH_ONLY]);
    assert!(e.sql.executed().is_empty());
}

#[test]
fn the_version_floor_is_sql_server_2012() {
    for (version, accepted) in [
        ("10.50.6000.34", false),
        ("10.0.6000.29", false),
        ("11.0.7001.0", true),
        ("16.0.4135.4", true),
    ] {
        let e = E::new();
        e.set_server(|s| s.product_version = version.to_owned());
        let r = e.enroll(true, None, Code::Valid).unwrap();
        assert_eq!(r.preflight_ok(), accepted, "{version}");
        if !accepted {
            assert_eq!(r.failed_codes(), [codes::SQL_SERVER_TOO_OLD]);
            assert_eq!(r.findings.len(), 1);
            assert!(r.findings[0].message.contains("2012"));
        }
    }
}

#[test]
fn a_server_below_2012_refuses_a_real_run() {
    let e = E::new();
    e.set_server(|s| s.product_version = "10.50.6000.34".to_owned());
    assert_eq!(
        code_of(e.enroll(false, None, Code::Valid)),
        codes::SQL_SERVER_TOO_OLD
    );
    e.code_is_unused();
}

#[test]
fn a_pda_sync_login_that_cannot_sign_in_leaves_the_code_unused() {
    let e = E::new();
    *e.login.failure.lock().unwrap() = Some(AgentError::new(
        codes::SQL_LOGIN_VERIFY_FAILED,
        "Login failed for user 'pda_sync'.",
    ));
    assert_eq!(
        code_of(e.enroll(false, None, Code::Valid)),
        codes::SQL_LOGIN_VERIFY_FAILED
    );
    let calls = e.login.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "SMILE_POS");
    let stored = e.env.secrets.load(names::SQL_PASSWORD).unwrap().unwrap();
    assert_eq!(calls[0].2, String::from_utf8(stored).unwrap());
    e.code_is_unused();
}

#[test]
fn the_login_is_verified_before_the_code_is_redeemed() {
    let e = E::new();
    let r = e.enroll(false, None, Code::Valid).unwrap();
    assert!(r.identity.is_some());
    assert_eq!(e.login.calls.lock().unwrap().len(), 1);
    assert_eq!(e.valid_code(), "USED");
}

#[test]
fn a_dry_run_without_a_url_does_not_probe_the_cloud() {
    let e = E::new();
    let r = e.enroll(true, None, Code::None).unwrap();
    assert!(r.preflight_ok());
    assert!(r.cloud.is_none());
    assert!(e.cloud.calls.lock().unwrap().is_empty());
    e.code_is_unused();
}

#[test]
fn a_dry_run_with_a_url_probes_the_cloud_and_a_warning_still_passes() {
    let e = E::new();
    *e.cloud.result.lock().unwrap() = CloudProbeResult {
        findings: vec![PreflightFinding::warn(
            codes::CLOCK_SKEW,
            "This machine's clock is 3m ahead of the cloud.",
        )],
        http_status: Some(200),
        clock_skew: Some(180 * 10_000_000),
    };
    let base = e.server.base.clone();
    let r = e.enroll(true, Some(&base), Code::Valid).unwrap();
    assert_eq!(*e.cloud.calls.lock().unwrap(), [base]);
    assert!(r.preflight_ok());
    let found: Vec<&str> = r.findings.iter().map(|f| f.code.as_str()).collect();
    assert_eq!(found, [codes::CLOCK_SKEW]);
    assert_eq!(r.cloud.unwrap().clock_skew, Some(180 * 10_000_000));
}

#[test]
fn a_dry_run_with_a_url_that_is_not_http_is_a_bad_argument() {
    let e = E::new();
    let r = e
        .enroll(true, Some("ftp://cloud.example"), Code::Valid)
        .unwrap();
    assert_eq!(r.failed_codes(), [codes::BAD_ARGUMENTS]);
    assert!(e.cloud.calls.lock().unwrap().is_empty());
}

// ---- EnrollmentTests ----

#[test]
fn enroll_redeems_code_with_signed_request_and_persists_identity_and_secrets() {
    let e = E::new();
    let identity = e
        .enroll(false, None, Code::Valid)
        .unwrap()
        .identity
        .unwrap();
    assert_eq!(identity.agent_id, e.server.agent_id);
    assert_eq!(identity.tenant_id, "tenant-1");
    assert_eq!(identity.sql_database.as_deref(), Some("SMILE_POS"));
    assert_eq!(identity.sql_instance.as_deref(), Some(r".\SQLEXPRESS"));
    assert_eq!(identity.machine_id, "TEST-PC");
    assert_eq!(
        format!("{}/", identity.cloud_url),
        e.server.base,
        "GetLeftPart(Authority) drops the path"
    );
    assert_eq!(
        AgentIdentity::try_load(&e.env.paths).unwrap().as_ref(),
        Some(&identity)
    );
    assert_eq!(
        e.env
            .secrets
            .load(names::SIGNING_SEED)
            .unwrap()
            .unwrap()
            .len(),
        32
    );
    assert_eq!(
        e.env.secrets.load(names::AGENT_TOKEN).unwrap().unwrap(),
        e.server.agent_token.as_bytes()
    );
    assert!(e.server.rejections().is_empty());
    assert_eq!(
        AgentStatus::try_read(&e.env.paths).unwrap().unwrap().state,
        "enrolled"
    );
}

#[test]
fn enroll_with_wrong_code_fails_with_stable_code_and_does_not_store_a_token() {
    let e = E::new();
    assert_eq!(
        code_of(e.enroll(false, None, Code::Is("WRONG"))),
        codes::ENROLL_CODE_INVALID
    );
    assert!(e.env.secrets.load(names::AGENT_TOKEN).unwrap().is_none());
    assert!(AgentIdentity::try_load(&e.env.paths).unwrap().is_none());
}

#[test]
fn sql_failure_never_burns_the_one_time_code() {
    let e = E::new();
    e.sql.st().deny_access = true;
    assert_eq!(
        code_of(e.enroll(false, None, Code::Valid)),
        codes::SQL_NO_ACCESS
    );
    assert_eq!(e.valid_code(), "CODE-1234");
}

#[test]
fn reenroll_reuses_the_stored_key() {
    let e = E::new();
    e.enroll(false, None, Code::Valid).unwrap();
    let seed = e.env.secrets.load(names::SIGNING_SEED).unwrap();
    e.server.with(|s| s.valid_code = "CODE-1234".into());
    e.enroll(false, None, Code::Valid).unwrap();
    assert_eq!(e.env.secrets.load(names::SIGNING_SEED).unwrap(), seed);
}

#[test]
fn missing_arguments_of_a_real_run_are_a_usage_error() {
    let e = E::new();
    let args = EnrollArgs::default();
    assert_eq!(code_of(e.enroll_with(&args)), codes::BAD_ARGUMENTS);
    assert!(e.sql.executed().is_empty());
}

// ---- EnrollCommandTests ----

struct ScriptedTransport {
    accepted: Mutex<Vec<SiteEvent>>,
}

impl ScriptedTransport {
    fn new() -> Arc<ScriptedTransport> {
        Arc::new(ScriptedTransport {
            accepted: Mutex::default(),
        })
    }
    fn accepted(&self) -> Vec<SiteEvent> {
        self.accepted.lock().unwrap().clone()
    }
    fn codes(&self) -> Vec<String> {
        self.accepted().into_iter().map(|e| e.code).collect()
    }
}

impl SiteEventTransport for ScriptedTransport {
    fn send(
        &self,
        events: &[SiteEvent],
        _ctx: &Ctx,
    ) -> Result<SiteEventSendResult, dl_agent_pda_sync::transport::Error> {
        self.accepted.lock().unwrap().extend(events.iter().cloned());
        Ok(SiteEventSendResult {
            status: SiteEventSendStatus::Accepted,
            http_status: 200,
            error: None,
        })
    }
}

/// `Console.Out` of a C# test: the tool run's lines and the verb's lines in one buffer.
#[derive(Default)]
struct Out(Arc<Mutex<Vec<u8>>>);

impl LineWriter for Out {
    fn write(&self, text: &str) {
        self.0.lock().unwrap().extend_from_slice(text.as_bytes());
    }
}

impl Out {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn contains_in_order(all: &[String], wanted: &[&str]) -> bool {
    let mut it = all.iter();
    wanted.iter().all(|w| it.any(|c| c == w))
}

fn detail<'a>(e: &'a SiteEvent, k: &str) -> Option<&'a dl_json::Value> {
    e.detail.as_ref().and_then(|d| d.get(k))
}

impl E {
    /// The verb as the CLI runs it, against the fakes. `None` transport: the events go
    /// out once the data folder has an identity (the default transport's rule).
    fn run_command(
        &self,
        args: &EnrollArgs,
        transport: Option<Arc<ScriptedTransport>>,
        out: &Out,
        err: &MemoryLines,
    ) -> i32 {
        let mut o = ToolRunOptions::new(site_event_sources::ENROLL, "enroll", codes::ENROLL_FAILED);
        o.data_dir = Some(self.env.dir.to_string_lossy().into_owned());
        o.temp_dir = self.env.dir.join("tmp");
        o.out = out.0.clone() as Console;
        o.clock = self.env.time.clone() as SharedClock;
        let t = transport.unwrap_or_else(|| self.default_transport.clone());
        let id_file = self.env.paths.identity_file();
        let identity_only = Arc::ptr_eq(&t, &self.default_transport);
        o.transport = Some(Arc::new(move |_| {
            (!identity_only || id_file.is_file()).then(|| t.clone() as Arc<dyn SiteEventTransport>)
        }));
        let workflow = new_workflow(&self.env, &self.sql, self.login.clone(), self.cloud.clone());
        ToolRun::execute(o, Some(enroll::start_detail(args)), |run| {
            enroll::run_workflow(
                &workflow,
                &self.env.paths,
                args,
                run,
                out,
                err,
                &Ctx::background(),
            )
        })
        .unwrap()
    }
}

struct CmdEnv {
    e: E,
    out: Out,
    err: Arc<MemoryLines>,
}

impl std::ops::Deref for CmdEnv {
    type Target = E;
    fn deref(&self) -> &E {
        &self.e
    }
}

impl CmdEnv {
    fn new() -> CmdEnv {
        CmdEnv {
            e: E::new(),
            out: Out::default(),
            err: MemoryLines::shared(),
        }
    }
    fn dry(&self) -> EnrollArgs {
        EnrollArgs {
            url: Some(self.server.base.clone()),
            dry_run: true,
            ..EnrollArgs::default()
        }
    }
    fn real(&self) -> EnrollArgs {
        EnrollArgs {
            code: Some(self.valid_code()),
            url: Some(self.server.base.clone()),
            ..EnrollArgs::default()
        }
    }
    fn run(&self, args: &EnrollArgs, transport: Option<Arc<ScriptedTransport>>) -> i32 {
        self.e.run_command(args, transport, &self.out, &self.err)
    }
}

#[test]
fn events_of_a_dry_run_before_enrollment_are_sent_by_the_enroll_that_follows() {
    let c = CmdEnv::new();
    assert_eq!(c.run(&c.dry(), None), EXIT_OK);
    let console = c.out.text();
    assert!(
        console.contains(PREFLIGHT_OK) && console.contains("site events: "),
        "{console}"
    );
    assert!(SiteEventQueueStats::read(c.env.paths.events_db()).depth > 0);
    assert!(c.default_transport.accepted().is_empty());

    assert_eq!(c.run(&c.real(), None), EXIT_OK, "{}", c.err.text());

    assert_eq!(SiteEventQueueStats::read(c.env.paths.events_db()).depth, 0);
    let sent = c.default_transport.accepted();
    let mut runs: Vec<&str> = sent.iter().filter_map(|e| e.run_id.as_deref()).collect();
    runs.dedup();
    assert_eq!(runs.len(), 2, "{runs:?}");
    let codes_sent: Vec<String> = sent.iter().map(|e| e.code.clone()).collect();
    assert!(
        contains_in_order(
            &codes_sent,
            &[
                "ENROLL_STARTED",
                SQL_PLANNED,
                CLOUD_PROBED,
                "ENROLL_FINISHED",
                "ENROLL_STARTED",
                SQL_BOOTSTRAPPED,
                LOGIN_VERIFIED,
                REDEEMED,
                "ENROLL_FINISHED"
            ]
        ),
        "{codes_sent:?}"
    );
    assert!(sent.iter().all(|e| e.source == site_event_sources::ENROLL));
    assert_eq!(
        detail(&sent[0], "dry_run").and_then(dl_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        detail(&sent[0], "has_code").and_then(dl_json::Value::as_bool),
        Some(false)
    );
    assert!(c.server.rejections().is_empty());
    assert!(c.out.text().contains("Enrolled as agent"));
}

#[test]
fn windows_only_authentication_is_refused_before_the_code_is_used() {
    let c = CmdEnv::new();
    c.set_server(|s| s.integrated_security_only = true);
    let t = ScriptedTransport::new();
    assert_eq!(c.run(&c.real(), Some(t.clone())), EXIT_FAILED);
    assert!(
        c.err
            .text()
            .contains(&format!("[{}]", codes::SQL_WINDOWS_AUTH_ONLY))
    );
    assert_eq!(
        c.valid_code(),
        "CODE-1234",
        "the enrollment code is still unused"
    );
    assert!(c.login.calls.lock().unwrap().is_empty());
    assert!(c.sql.executed().is_empty());
    assert!(contains_in_order(
        &t.codes(),
        &[
            "ENROLL_STARTED",
            SQL_LOCATED,
            codes::SQL_WINDOWS_AUTH_ONLY,
            "ENROLL_FAILED"
        ]
    ));
    let last = t.accepted().pop().unwrap();
    assert_eq!(
        detail(&last, "code").and_then(dl_json::Value::as_str),
        Some(codes::SQL_WINDOWS_AUTH_ONLY)
    );
    assert_eq!(
        AgentStatus::try_read(&c.env.paths)
            .unwrap()
            .unwrap()
            .error_code
            .as_deref(),
        Some(codes::SQL_WINDOWS_AUTH_ONLY)
    );
}

#[test]
fn a_dry_run_on_windows_only_authentication_ends_in_preflight_failed() {
    let c = CmdEnv::new();
    c.set_server(|s| s.integrated_security_only = true);
    let t = ScriptedTransport::new();
    assert_eq!(c.run(&c.dry(), Some(t.clone())), EXIT_FAILED);
    let out = c.out.text();
    assert!(
        out.contains(&format!(
            "{PREFLIGHT_FAILED}: {}",
            codes::SQL_WINDOWS_AUTH_ONLY
        )),
        "{out}"
    );
    assert!(out.contains("Windows Authentication"));
    assert!(c.sql.executed().is_empty());
    let refusal = t
        .accepted()
        .into_iter()
        .find(|e| e.code == codes::SQL_WINDOWS_AUTH_ONLY)
        .unwrap();
    assert_eq!(refusal.level, lv::ERROR);
    let last = t.accepted().pop().unwrap();
    assert_eq!(
        detail(&last, "code").and_then(dl_json::Value::as_str),
        Some(codes::SQL_WINDOWS_AUTH_ONLY)
    );
}

#[test]
fn a_dry_run_with_a_clock_too_far_off_ends_in_preflight_failed() {
    let c = CmdEnv::new();
    *c.cloud.result.lock().unwrap() = CloudProbeResult {
        findings: vec![PreflightFinding::error(
            codes::CLOCK_SKEW_TOO_LARGE,
            "This machine's clock is 6m ahead of the cloud.",
        )],
        http_status: Some(200),
        clock_skew: Some(360 * 10_000_000),
    };
    assert_eq!(c.run(&c.dry(), Some(ScriptedTransport::new())), EXIT_FAILED);
    assert!(c.out.text().contains(&format!(
        "{PREFLIGHT_FAILED}: {}",
        codes::CLOCK_SKEW_TOO_LARGE
    )));
    assert_eq!(
        *c.cloud.calls.lock().unwrap(),
        std::slice::from_ref(&c.server.base)
    );
    assert!(
        c.out.text().contains("clock skew +360 s"),
        "{}",
        c.out.text()
    );
}

#[test]
fn missing_arguments_are_a_usage_refusal_at_warn() {
    let c = CmdEnv::new();
    let t = ScriptedTransport::new();
    assert_eq!(c.run(&EnrollArgs::default(), Some(t.clone())), EXIT_USAGE);
    assert!(
        c.err
            .text()
            .contains(&format!("[{}]", codes::BAD_ARGUMENTS))
    );
    let refusal = t
        .accepted()
        .into_iter()
        .find(|e| e.code == codes::BAD_ARGUMENTS)
        .unwrap();
    assert_eq!(refusal.level, lv::WARN);
    assert!(c.env.secrets.load(names::AGENT_TOKEN).unwrap().is_none());
}

// ---- GrantsCommandTests ----

struct G {
    env: TestEnv,
    sql: FakeSqlAdmin,
    probe: FakeGrantsProbe,
    out: Arc<MemoryLines>,
    err: Arc<MemoryLines>,
}

impl G {
    fn new() -> G {
        G {
            env: TestEnv::new(),
            sql: FakeSqlAdmin::new(),
            probe: FakeGrantsProbe::default(),
            out: MemoryLines::shared(),
            err: MemoryLines::shared(),
        }
    }

    fn run(&self, dry_run: bool) -> i32 {
        grants::run_parts(
            &new_bootstrapper(&self.sql, &self.env.secrets),
            &self.probe,
            &*self.env.secrets,
            &self.env.paths,
            &GrantsArgs {
                dry_run,
                ..GrantsArgs::default()
            },
            &NullRunStages,
            &*self.out,
            &*self.err,
            &Ctx::background(),
        )
        .unwrap()
    }

    /// A store enrolled before OW1b: identity, login with its password, every step but
    /// the order grants.
    fn enroll_old(&self) {
        new_bootstrapper(&self.sql, &self.env.secrets)
            .run(false, None, None, &Ctx::background())
            .unwrap();
        self.sql
            .st()
            .created
            .remove("SMILE_POS/pda_sync#OrderGrants");
        AgentIdentity {
            agent_id: dl_uuid::Uuid::new_v4(),
            tenant_id: "t1".into(),
            cloud_url: "https://cloud.test".into(),
            machine_id: "PC".into(),
            sql_instance: Some(r".\SQLEXPRESS".into()),
            sql_database: Some("SMILE_POS".into()),
        }
        .save(&self.env.paths)
        .unwrap();
    }
}

#[test]
fn grants_applies_only_the_missing_order_grants_and_keeps_the_pda_sync_password() {
    let g = G::new();
    g.enroll_old();
    let password = g.env.secrets.load(names::SQL_PASSWORD).unwrap();
    let before = g.sql.executed().len();

    assert_eq!(g.run(false), EXIT_OK, "{}", g.err.text());

    let new: Vec<String> = g.sql.executed()[before..].to_vec();
    assert_eq!(new.len(), 1);
    assert!(new[0].starts_with("order_grants:"));
    assert_eq!(g.env.secrets.load(names::SQL_PASSWORD).unwrap(), password);
    let out = g.out.text();
    assert!(out.contains("sql step order_grants: applied"), "{out}");
    assert!(out.contains("sql step login: already present"));
    assert!(out.contains("has the order grants"));
}

#[test]
fn grants_dry_run_changes_nothing() {
    let g = G::new();
    g.enroll_old();
    let before = g.sql.executed().len();
    assert_eq!(g.run(true), EXIT_OK);
    assert_eq!(g.sql.executed().len(), before);
    assert!(g.out.text().contains("would apply (dry-run)"));
}

#[test]
fn grants_still_missing_after_the_run_fail_the_verb() {
    let g = G::new();
    g.enroll_old();
    *g.probe.missing.lock().unwrap() = vec!["EXECUTE dbo.PostITrn".into()];
    assert_eq!(g.run(false), EXIT_FAILED);
    assert!(g.err.text().contains(codes::SQL_GRANTS_MISSING));
    assert!(
        g.err
            .text()
            .contains("pda_sync still lacks: EXECUTE dbo.PostITrn.")
    );
}

#[test]
fn grants_on_a_machine_that_is_not_enrolled_is_refused_before_any_sql_write() {
    let g = G::new();
    assert_eq!(g.run(false), EXIT_FAILED);
    assert!(g.err.text().contains(codes::NOT_ENROLLED));
    assert!(g.sql.executed().is_empty());
}
