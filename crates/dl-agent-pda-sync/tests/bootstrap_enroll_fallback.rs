//! RT-0365: `enroll` tries the Windows identity, then the SMILE default SQL login, and
//! records every attempt without ever printing the default password.

mod bootstrap_support;
mod support;

use bootstrap_support::{FakeCloudProbe, FakeLoginProbe, FakeSqlAdmin, new_workflow};
use dl_agent_pda_sync::bootstrap::admin::SqlAdminCredential;
use dl_agent_pda_sync::bootstrap::workflow::EnrollArgs;
use dl_agent_pda_sync::config::codes;
use dl_agent_pda_sync::config::time::SharedClock;
use dl_agent_pda_sync::contracts::wire::{SiteEvent, site_event_levels, site_event_sources};
use dl_agent_pda_sync::hosting::enroll_attempts::{
    ATTEMPT_CODE, DEFAULT_PASSWORD_ENV, DefaultLogin, attempts_for, default_attempts, run_attempts,
};
use dl_agent_pda_sync::hosting::secret_prompt::FixedSecretPrompt;
use dl_agent_pda_sync::hosting::{EXIT_FAILED, EXIT_OK, Options};
use dl_agent_pda_sync::reporting::tool_run::Console;
use dl_agent_pda_sync::reporting::{
    SiteEventSendResult, SiteEventSendStatus, SiteEventTransport, ToolRun, ToolRunOptions,
};
use dl_agent_pda_sync::util::console::{LineWriter, MemoryLines};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use support::{FakeSyncServer, TestEnv};

#[derive(Default)]
struct Transport(Mutex<Vec<SiteEvent>>);

impl SiteEventTransport for Transport {
    fn send(
        &self,
        events: &[SiteEvent],
        _ctx: &Ctx,
    ) -> Result<SiteEventSendResult, dl_agent_pda_sync::transport::Error> {
        self.0.lock().unwrap().extend(events.iter().cloned());
        Ok(SiteEventSendResult {
            status: SiteEventSendStatus::Accepted,
            http_status: 200,
            error: None,
        })
    }
}

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

struct F {
    env: TestEnv,
    server: FakeSyncServer,
    windows: FakeSqlAdmin,
    smile: FakeSqlAdmin,
    explicit: FakeSqlAdmin,
    builds: AtomicUsize,
    password: String,
    out: Out,
    err: Arc<MemoryLines>,
    events: Arc<Transport>,
}

impl F {
    fn new() -> F {
        let env = TestEnv::new();
        let server = FakeSyncServer::start(env.time.clone());
        F {
            env,
            server,
            windows: FakeSqlAdmin::new(),
            smile: FakeSqlAdmin::new(),
            explicit: FakeSqlAdmin::new(),
            builds: AtomicUsize::new(0),
            password: format!("pw-{}", dl_uuid::Uuid::new_v4().simple()),
            out: Out::default(),
            err: MemoryLines::shared(),
            events: Arc::default(),
        }
    }

    fn login(&self, with_password: bool) -> DefaultLogin {
        let pw = self.password.clone();
        DefaultLogin::from_env(
            &move |n| (with_password && n == DEFAULT_PASSWORD_ENV).then(|| pw.clone()),
            "",
        )
    }

    fn dry_args(&self) -> EnrollArgs {
        EnrollArgs {
            url: Some(self.server.base.clone()),
            dry_run: true,
            ..EnrollArgs::default()
        }
    }

    fn args(&self) -> EnrollArgs {
        EnrollArgs {
            code: Some(self.server.with(|s| s.valid_code.clone())),
            url: Some(self.server.base.clone()),
            ..EnrollArgs::default()
        }
    }

    fn run(&self, attempts: Vec<dl_agent_pda_sync::hosting::enroll_attempts::Attempt>) -> i32 {
        self.run_with_args(attempts, self.args())
    }

    fn run_with_args(
        &self,
        attempts: Vec<dl_agent_pda_sync::hosting::enroll_attempts::Attempt>,
        args: EnrollArgs,
    ) -> i32 {
        let mut o = ToolRunOptions::new(site_event_sources::ENROLL, "enroll", codes::ENROLL_FAILED);
        o.data_dir = Some(self.env.dir.to_string_lossy().into_owned());
        o.temp_dir = self.env.dir.join("tmp");
        o.out = self.out.0.clone() as Console;
        o.clock = self.env.time.clone() as SharedClock;
        let t = self.events.clone();
        o.transport = Some(Arc::new(move |_| {
            Some(t.clone() as Arc<dyn SiteEventTransport>)
        }));
        let build = |c: &SqlAdminCredential| {
            self.builds.fetch_add(1, Ordering::SeqCst);
            let sql = match c.user.as_deref() {
                None => &self.windows,
                Some("smile") => &self.smile,
                Some(_) => &self.explicit,
            };
            Ok(new_workflow(
                &self.env,
                sql,
                Arc::new(FakeLoginProbe::default()),
                Arc::new(FakeCloudProbe::default()),
            ))
        };
        ToolRun::execute(o, None, |run| {
            run_attempts(
                attempts,
                &build,
                &self.env.paths,
                &args,
                run,
                &self.out,
                &*self.err,
                &Ctx::background(),
            )
        })
        .unwrap()
    }

    fn sent(&self) -> Vec<SiteEvent> {
        self.events.0.lock().unwrap().clone()
    }

    fn attempt_events(&self) -> Vec<SiteEvent> {
        self.sent()
            .into_iter()
            .filter(|e| e.code == ATTEMPT_CODE)
            .collect()
    }

    fn enrolled(&self) -> bool {
        self.env.paths.identity_file().is_file()
    }

    /// Everything a run can show the outside world, as one string.
    fn everything(&self) -> String {
        let events: String = self
            .sent()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        let status = std::fs::read_to_string(self.env.paths.status_file()).unwrap_or_default();
        format!(
            "{}\n{}\n{events}\n{status}",
            self.out.text(),
            self.err.text()
        )
    }
}

#[test]
fn a_working_windows_identity_never_touches_the_default_login() {
    let f = F::new();
    assert_eq!(
        f.run(default_attempts(&f.login(true))),
        EXIT_OK,
        "{}",
        f.err.text()
    );
    assert!(f.enrolled());
    assert_eq!(f.builds.load(Ordering::SeqCst), 1);
    assert!(f.smile.executed().is_empty());
    let events = f.attempt_events();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].level, site_event_levels::INFO);
    let detail = events[0].detail.as_ref().unwrap();
    assert_eq!(
        detail.get("identity").and_then(dl_json::Value::as_str),
        Some("windows")
    );
    assert_eq!(
        detail.get("code").and_then(dl_json::Value::as_str),
        Some("used")
    );
    assert!(!f.everything().contains(&f.password));
}

#[test]
fn a_refused_windows_identity_falls_back_to_the_smile_login() {
    for deny_login_step in [false, true] {
        let f = F::new();
        if deny_login_step {
            f.windows.st().deny_login_step = true;
        } else {
            f.windows.st().deny_access = true;
        }
        assert_eq!(
            f.run(default_attempts(&f.login(true))),
            EXIT_OK,
            "{}",
            f.err.text()
        );
        assert!(f.enrolled());
        assert!(
            f.smile
                .executed()
                .iter()
                .any(|s| s.contains("CREATE LOGIN")),
            "the smile identity ran the login step"
        );
        let attempts = f.attempt_events();
        assert!(
            attempts.iter().any(|e| e.level == site_event_levels::WARN
                && e.message.contains("windows")
                && e.message.contains("refused")),
            "{attempts:?}"
        );
        assert!(
            f.out
                .text()
                .contains("SQL administrator identity used: sql:smile"),
            "{}",
            f.out.text()
        );
        let all = f.everything();
        assert!(!all.contains(&f.password), "the password leaked");
    }
}

#[test]
fn when_every_identity_is_refused_the_error_lists_them_and_suggests_an_explicit_login() {
    let f = F::new();
    f.windows.st().deny_access = true;
    f.smile.st().deny_access = true;
    assert_eq!(f.run(default_attempts(&f.login(true))), EXIT_FAILED);
    assert!(!f.enrolled());
    let err = f.err.text();
    for needle in [
        "windows",
        "sql:smile",
        codes::SQL_NO_ACCESS,
        "--sql-admin-user",
        "DL_AGENT_PDA_SQL_ADMIN_PASSWORD",
    ] {
        assert!(err.contains(needle), "{needle}: {err}");
    }
    let status = std::fs::read_to_string(f.env.paths.status_file()).unwrap();
    assert!(status.contains("enroll_failed"), "{status}");
    assert_eq!(f.attempt_events().len(), 2);
    assert!(!f.everything().contains(&f.password));
}

#[test]
fn without_a_default_password_the_second_attempt_is_recorded_as_skipped() {
    let f = F::new();
    f.windows.st().deny_access = true;
    assert_eq!(f.run(default_attempts(&f.login(false))), EXIT_FAILED);
    assert_eq!(f.builds.load(Ordering::SeqCst), 1);
    let err = f.err.text();
    assert!(
        err.contains("SKIPPED") && err.contains("no default password"),
        "{err}"
    );
    let events = f.attempt_events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().any(|e| e.message.contains("skipped")));
}

#[test]
fn an_explicit_login_is_one_attempt_with_no_fallback() {
    let f = F::new();
    f.explicit.st().deny_access = true;
    let mut opts = Options::default();
    opts.insert("sql-admin-user", "sa");
    let prompt = FixedSecretPrompt(vec![(
        dl_agent_pda_sync::bootstrap::admin::PASSWORD_ENV_VAR.to_owned(),
        "explicit-secret".to_owned(),
    )]);
    let attempts = attempts_for(&opts, &prompt, &f.login(true)).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(f.run(attempts), EXIT_FAILED);
    assert_eq!(f.builds.load(Ordering::SeqCst), 1);
    assert!(f.smile.executed().is_empty());
    assert!(f.attempt_events().is_empty());
    assert!(
        !f.err
            .text()
            .contains("No SQL Server administrator identity worked")
    );
}

#[test]
fn a_refusal_that_is_not_about_access_does_not_fall_back() {
    let f = F::new();
    f.windows.st().server.integrated_security_only = true;
    assert_eq!(f.run(default_attempts(&f.login(true))), EXIT_FAILED);
    assert_eq!(f.builds.load(Ordering::SeqCst), 1);
    assert!(f.err.text().contains(codes::SQL_WINDOWS_AUTH_ONLY));
    let events = f.attempt_events();
    assert_eq!(events.len(), 1, "{events:?}");
    let code = events[0]
        .detail
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(dl_json::Value::as_str);
    assert_eq!(code, Some("accepted_then_failed"));
}

#[test]
fn the_default_login_comes_from_the_environment_before_the_build_default() {
    let from = |user: Option<&str>, pw: Option<&str>, build: &str| {
        let (user, pw) = (user.map(str::to_owned), pw.map(str::to_owned));
        let login = DefaultLogin::from_env(
            &move |n| match n {
                "DL_AGENT_PDA_SQL_DEFAULT_USER" => user.clone(),
                DEFAULT_PASSWORD_ENV => pw.clone(),
                _ => None,
            },
            build,
        );
        format!("{:?}", default_attempts(&login))
    };
    assert!(from(None, None, "built").contains("sql:smile"));
    assert!(from(Some("other"), Some("x-env"), "built").contains("sql:other"));
    assert!(from(Some(""), Some(""), "").contains("Skipped"));
    assert!(!from(None, None, "built").contains("Skipped"));
}

#[test]
fn a_dry_run_uses_the_preflight_of_the_identity_that_is_not_refused() {
    let f = F::new();
    f.windows.st().deny_access = true;
    assert_eq!(
        f.run_with_args(default_attempts(&f.login(true)), f.dry_args()),
        EXIT_OK,
        "{}",
        f.err.text()
    );
    assert!(f.out.text().contains("PREFLIGHT OK"), "{}", f.out.text());
    assert_eq!(f.builds.load(Ordering::SeqCst), 2);
    assert!(!f.enrolled());
    assert!(!f.everything().contains(&f.password));
}

#[test]
fn a_dry_run_refused_everywhere_lists_the_attempts_above_the_failed_verdict() {
    let f = F::new();
    f.windows.st().deny_access = true;
    f.smile.st().deny_access = true;
    assert_ne!(
        f.run_with_args(default_attempts(&f.login(true)), f.dry_args()),
        EXIT_OK
    );
    let text = f.out.text();
    let summary = text.find("No SQL Server administrator identity worked");
    let verdict = text.find("PREFLIGHT FAILED");
    let (Some(summary), Some(verdict)) = (summary, verdict) else {
        panic!("missing summary or verdict: {text} / {}", f.err.text());
    };
    assert!(summary < verdict, "{text}");
    assert!(
        text.contains("sql:smile") && text.contains("--sql-admin-user"),
        "{text}"
    );
    assert!(!f.everything().contains(&f.password));
}
