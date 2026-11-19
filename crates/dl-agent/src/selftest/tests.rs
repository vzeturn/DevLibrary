use std::sync::Arc;
use std::time::Duration;

use dl_json::{Value, json};

use super::checks_hub::{self, BEAT_FRESH_SECONDS};
use super::legacy::{self, Found};
use super::runner::{self, ModulePlan};
use super::*;
use crate::cli::App;
use crate::config::RemoteConfig;
use crate::hub::tests_support::identity;
use crate::sql::SqlPools;
use crate::store::testdir::TestDir;
use crate::supervisor::{Module, ModuleContext};
use crate::watchdog::{Beat, Service, write_beat};

fn check(status: Status) -> Check {
    let o = Outcome {
        status,
        detail: "d".into(),
        remedy: None,
    };
    Check::new("core", "x", "X", o, Duration::from_millis(7))
}

fn run(checks: Vec<Check>) -> Run {
    Run {
        run_id: run_id(),
        trigger: Trigger::Cli,
        started_at: 10,
        finished_at: 12,
        agent_version: "1.2.3".into(),
        os: "linux".into(),
        hostname: "pc".into(),
        checks,
    }
}

#[test]
fn check_json_omits_an_empty_remedy() {
    let c = Check::new(
        "core",
        "clock",
        "Clock",
        Outcome::warn("off").remedy("sync"),
        Duration::from_millis(3),
    );
    let v = c.to_json();
    assert_eq!(v["status"].as_str(), Some("warn"));
    assert_eq!(v["remedy"].as_str(), Some("sync"));
    assert_eq!(v["duration_ms"].as_u64(), Some(3));
    assert!(check(Status::Pass).to_json().get("remedy").is_none());
}

#[test]
fn run_body_has_the_hub_contract_and_counts() {
    let r = run(vec![
        check(Status::Pass),
        check(Status::Fail),
        check(Status::Skip),
    ]);
    let v = r.to_json();
    for k in [
        "run_id",
        "trigger",
        "started_at",
        "finished_at",
        "agent_version",
        "os",
        "hostname",
    ] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert_eq!(v["trigger"].as_str(), Some("cli"));
    assert_eq!(v["checks"].as_array().map(Vec::len), Some(3));
    assert_eq!(r.run_id.len(), 32);
    assert!(r.failed());
    assert_eq!(
        r.summary(),
        json!({"pass": 1, "warn": 0, "fail": 1, "skip": 1})
    );
    assert_eq!(cli::exit_code(&r), cli::EXIT_FAILED);
    assert_eq!(cli::exit_code(&run(vec![check(Status::Warn)])), 0);
}

#[test]
fn run_body_stays_under_the_hub_caps() {
    let long = "word ".repeat(200);
    let many: Vec<Check> = (0..700)
        .map(|i| {
            Check::new(
                "m",
                &format!("c{i}"),
                "T",
                Outcome::pass(long.clone()),
                Duration::ZERO,
            )
        })
        .collect();
    let v = run(many).to_json();
    assert_eq!(v["checks"].as_array().map(Vec::len), Some(MAX_CHECKS));
    assert!(dl_json::to_string(&v).len() <= MAX_BODY);
}

#[test]
fn trigger_names_round_trip() {
    for t in [Trigger::Cli, Trigger::Command, Trigger::Install] {
        assert_eq!(Trigger::parse(t.as_str()), Some(t));
    }
    assert_eq!(Trigger::parse("cron"), None);
}

#[test]
fn sanitize_masks_secrets_and_keeps_the_rest() {
    let cases = [
        (
            "Server=db;User Id=sa;Password=Sup3r;Encrypt=no",
            "Server=db;User Id=sa;Password=***;Encrypt=no",
        ),
        ("pwd = 'a b c' next", "pwd = '***' next"),
        (
            r#"{"agent_token":"t0k","api_key": "k"}"#,
            r#"{"agent_token":"***","api_key": "***"}"#,
        ),
        ("Authorization: Bearer abc.def", "Authorization: ***"),
        (
            "authorization=Basic dXNlcjpw; next",
            "authorization=***; next",
        ),
        ("got bearer xyz from hub", "got bearer *** from hub"),
        (
            "https://u:p@host/x and ftp://host/y",
            "https://***@host/x and ftp://host/y",
        ),
        (
            "key AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/ end",
            "key ***/ end",
        ),
        ("connected in 12 ms over tls", "connected in 12 ms over tls"),
        ("token expired", "token expired"),
    ];
    for (input, want) in cases {
        assert_eq!(sanitize(input, 600), want, "{input}");
    }
    assert_eq!(sanitize("abcdef", 4), "abc…");
    let c = Check::new(
        "m",
        "i",
        "t",
        Outcome::fail("Password=x").remedy("use Pwd=y"),
        Duration::ZERO,
    );
    assert_eq!(
        (c.detail.as_str(), c.remedy.as_deref()),
        ("Password=***", Some("use Pwd=***"))
    );
}

#[test]
fn bounded_returns_times_out_and_catches_panics() {
    assert_eq!(bounded("ok", Duration::from_secs(5), || 7), Ok(7));
    let slow = bounded("slow", Duration::from_millis(20), || {
        std::thread::sleep(Duration::from_secs(2))
    });
    assert_eq!(slow, Err(Stalled::TimedOut));
    let boom = bounded("boom", Duration::from_secs(5), || -> u8 { panic!("kaput") });
    assert_eq!(boom, Err(Stalled::Panicked("kaput".into())));
}

struct Dummy(&'static str, bool);

impl Module for Dummy {
    fn name(&self) -> &str {
        self.0
    }
    fn enabled_by_default(&self) -> bool {
        self.1
    }
    fn run(&mut self, _: &ModuleContext) -> Result<(), crate::Error> {
        Ok(())
    }
}

fn reads_config(ctx: &ModuleContext) -> Vec<Check> {
    let dir = ctx
        .config()
        .get("config_dir")
        .and_then(Value::as_str)
        .unwrap_or("");
    if dir.is_empty() {
        return vec![Check::skip("ignored", "config", "Config", "not enrolled")];
    }
    vec![Check::timed("ignored", "config", "Config", || {
        Outcome::pass(format!("dir {dir}"))
    })]
}

fn panics(_: &ModuleContext) -> Vec<Check> {
    panic!("module self-test bug")
}

fn hangs(_: &ModuleContext) -> Vec<Check> {
    std::thread::sleep(Duration::from_secs(3));
    Vec::new()
}

fn config(body: Value) -> RemoteConfig {
    RemoteConfig { version: 3, body }
}

#[test]
fn modules_run_with_their_configured_dir_and_skip_when_off() {
    let d = TestDir::new("st-modules");
    let defaults = vec![
        ("alpha".to_string(), true),
        ("beta".to_string(), false),
        ("gamma".to_string(), true),
        ("delta".to_string(), true),
    ];
    let tests = [
        SelfTest {
            module: "alpha",
            run: reads_config,
        },
        SelfTest {
            module: "beta",
            run: reads_config,
        },
        SelfTest {
            module: "delta",
            run: reads_config,
        },
    ];
    let cfg = config(json!({"modules": {
        "alpha": {"config_dir": "C:\\ProgramData\\dl-agent\\bench\\alpha"},
        "delta": {"enabled": false},
    }}));
    let plans = runner::plan(&defaults, &tests, &cfg, None);
    let checks = runner::run_modules(
        &plans,
        &d.0,
        &cfg,
        &SqlPools::default(),
        Duration::from_secs(10),
    );
    let got: Vec<(&str, Status, &str)> = checks
        .iter()
        .map(|c| (c.module.as_str(), c.status, c.detail.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (
                "alpha",
                Status::Pass,
                "dir C:\\ProgramData\\dl-agent\\bench\\alpha"
            ),
            ("beta", Status::Skip, "module disabled in the remote config"),
            ("gamma", Status::Skip, "this module has no self-test"),
            (
                "delta",
                Status::Skip,
                "module disabled in the remote config"
            ),
        ]
    );
    let only = runner::plan(&defaults, &tests, &cfg, Some("gamma"));
    assert_eq!(only.len(), 1);
}

#[test]
fn a_crashed_or_stuck_module_test_is_one_fail() {
    let d = TestDir::new("st-stuck");
    let plans = vec![
        ModulePlan {
            name: "crash".into(),
            enabled: true,
            test: Some(SelfTest {
                module: "crash",
                run: panics,
            }),
        },
        ModulePlan {
            name: "slow".into(),
            enabled: true,
            test: Some(SelfTest {
                module: "slow",
                run: hangs,
            }),
        },
    ];
    let checks = runner::run_modules(
        &plans,
        &d.0,
        &RemoteConfig::default(),
        &SqlPools::default(),
        // Well under the 3 s `hangs` sleep, yet long enough that a panicking module whose
        // backtrace is symbolized (RUST_BACKTRACE=1) on a loaded machine still reports in time.
        Duration::from_millis(1500),
    );
    assert_eq!(checks.len(), 2);
    assert!(
        checks
            .iter()
            .all(|c| c.status == Status::Fail && c.id == "module")
    );
    assert!(checks[0].detail.contains("module self-test bug"));
    assert!(checks[1].detail.contains("did not finish in time"));
}

#[test]
fn legacy_agent_warns_only_when_it_doubles_an_enabled_module() {
    let found = |name: &str, running: bool| Found {
        name: name.into(),
        display: name.into(),
        state: if running { "running" } else { "stopped" }.into(),
        running,
    };
    let enabled = vec!["einvoice".to_string(), "backup".to_string()];
    assert_eq!(legacy::assess(&[], &enabled).status, Status::Pass);
    let stopped = legacy::assess(&[found("SRBackupAgent", false)], &enabled);
    assert_eq!(stopped.status, Status::Pass);
    assert!(
        stopped
            .detail
            .contains("SRBackupAgent (stopped, module backup)")
    );
    let other = legacy::assess(
        &[found("SmileHLAgent", true), found("PdaSyncAgent", true)],
        &enabled,
    );
    assert_eq!(other.status, Status::Pass);
    let doubled = legacy::assess(
        &[
            found("MM-EInvoice Agent", true),
            found("SmileHLAgent", true),
        ],
        &enabled,
    );
    assert_eq!(doubled.status, Status::Warn);
    assert!(
        doubled
            .detail
            .ends_with("(work done twice): MM-EInvoice Agent")
    );
    assert!(
        doubled
            .remedy
            .unwrap()
            .contains("sc stop \"MM-EInvoice Agent\"")
    );
    if !cfg!(windows) {
        assert_eq!(legacy::check(&enabled).status, Status::Skip);
    }
}

#[test]
fn legacy_services_match_like_the_bench_script() {
    assert_eq!(
        legacy::classify("MMEInvoiceAgent", ""),
        Some(Some("einvoice"))
    );
    assert_eq!(
        legacy::classify("svc1", "MM E-Invoice Agent"),
        Some(Some("einvoice"))
    );
    assert_eq!(
        legacy::classify("SmilePmsAgent", ""),
        Some(Some("pms-site"))
    );
    assert_eq!(
        legacy::classify("SmilePmsDeviceAgent", ""),
        Some(Some("pms-device"))
    );
    assert_eq!(
        legacy::classify("SmileHLAgent", ""),
        Some(Some("hotellink"))
    );
    assert_eq!(
        legacy::classify("MM.Arrival.Agent", ""),
        Some(Some("arrival"))
    );
    assert_eq!(
        legacy::classify("MM.Membership.Agent", ""),
        Some(Some("membership"))
    );
    assert_eq!(
        legacy::classify("SRBackup", "SR Backup Agent"),
        Some(Some("backup"))
    );
    assert_eq!(legacy::classify("PdaSyncAgent", ""), Some(None));
    assert_eq!(
        legacy::classify("dl-agent", "DevLibrary Agent (einvoice)"),
        None
    );
    assert_eq!(legacy::classify("Spooler", "Print Spooler"), None);
}

#[test]
fn clock_thresholds_follow_the_hub_limit() {
    let status = |s| checks_hub::clock(Some(s)).status;
    assert_eq!(
        (status(0), status(-60), status(61), status(-300)),
        (Status::Pass, Status::Pass, Status::Warn, Status::Warn)
    );
    assert_eq!(status(301), Status::Fail);
    assert_eq!(checks_hub::clock(None).status, Status::Skip);
}

fn app() -> App {
    App::new("dl-agent-test", "Test agent")
        .module(Arc::new(|| Box::new(Dummy("alpha", true))))
        .selftest(SelfTest {
            module: "alpha",
            run: reads_config,
        })
}

#[test]
fn hub_check_uses_the_service_heartbeat_inside_the_service() {
    let d = TestDir::new("st-hub");
    let a = app();
    let id = identity(&["http://127.0.0.1:9"]);
    let beat = |hub_at| Beat {
        at: crate::now(),
        hub_at,
        version: "1.0.0".into(),
        pid: 1,
        channel: "canary".into(),
        skew: 4,
        links: Vec::new(),
    };
    write_beat(&d.0, &beat(crate::now() - 5)).unwrap();
    let (c, link) = checks_hub::hub(&a, &d.0, Some(id.clone()), None, Trigger::Command);
    assert_eq!(c.status, Status::Pass, "{}", c.detail);
    assert_eq!((link.channel.as_str(), link.skew), ("canary", Some(4)));
    write_beat(&d.0, &beat(crate::now() - BEAT_FRESH_SECONDS - 120)).unwrap();
    let (c, link) = checks_hub::hub(&a, &d.0, Some(id), Some(Service::Running), Trigger::Command);
    assert_eq!(c.status, Status::Fail);
    assert!(c.detail.contains("min ago") && c.remedy.is_some());
    assert_eq!(link.skew, None);
    let (c, _) = checks_hub::hub(&a, &d.0, None, None, Trigger::Cli);
    assert_eq!(
        (c.status, c.detail.as_str()),
        (Status::Skip, "not enrolled")
    );
}

#[test]
fn version_check_needs_a_channel() {
    let a = app();
    assert_eq!(checks_hub::version(&a, "canary").status, Status::Pass);
    assert_eq!(checks_hub::version(&a, "").status, Status::Warn);
}

#[test]
fn an_unenrolled_agent_fails_identity_and_skips_the_hub() {
    let d = TestDir::new("st-unenrolled");
    let r = execute(&app(), &d.0, Trigger::Cli, Some(CORE), &Value::Null);
    let status = |id: &str| r.checks.iter().find(|c| c.id == id).map(|c| c.status);
    assert_eq!(status("identity"), Some(Status::Fail));
    assert_eq!(status("hub"), Some(Status::Skip));
    assert_eq!(status("update"), Some(Status::Skip));
    assert_eq!(status("clock"), Some(Status::Skip));
    assert_eq!(status("permissions"), Some(Status::Skip));
    assert_eq!(status("sql"), Some(Status::Skip));
    assert_eq!(status("spool"), Some(Status::Pass));
    assert_eq!(status("config"), Some(Status::Warn));
    assert!(r.checks.iter().all(|c| c.module == CORE));
    let table = cli::table(&r);
    assert!(table.starts_with("STATUS  MODULE"));
    assert!(table.contains("remedy: run `dl-agent-test enroll"));
}

#[test]
fn module_only_run_skips_core_and_follows_the_config() {
    let d = TestDir::new("st-only");
    let r = execute(&app(), &d.0, Trigger::Cli, Some("alpha"), &Value::Null);
    assert_eq!(r.checks.len(), 1);
    assert_eq!(
        (r.checks[0].module.as_str(), r.checks[0].status),
        ("alpha", Status::Skip)
    );
    crate::store::write_json(
        &d.0.join("config.json"),
        &json!({"current": {"version": 2, "body": {"modules": {"alpha": {"config_dir": "D:\\x"}}}}}),
    )
    .unwrap();
    let r = execute(&app(), &d.0, Trigger::Cli, Some("alpha"), &Value::Null);
    assert_eq!(
        (r.checks[0].status, r.checks[0].detail.as_str()),
        (Status::Pass, "dir D:\\x")
    );
}

#[test]
fn an_invalid_sql_file_is_one_fail() {
    let d = TestDir::new("st-sql");
    std::fs::write(d.0.join(crate::sql::SQL_FILE), b"{not json").unwrap();
    let site = crate::sql::Site::load(&d.0);
    let checks = checks_core::databases(&d.0, &site, &Value::Null);
    assert_eq!(checks.len(), 1);
    assert_eq!(
        (checks[0].id.as_str(), checks[0].status),
        ("sql", Status::Fail)
    );
}

#[cfg(unix)]
#[test]
fn open_private_files_fail_the_permission_check() {
    use std::os::unix::fs::PermissionsExt;
    let d = TestDir::new("st-perm");
    let key = d.0.join("identity.key");
    std::fs::write(&key, b"seed").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(checks_core::permissions(&d.0).status, Status::Pass);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let c = checks_core::permissions(&d.0);
    assert_eq!(c.status, Status::Fail);
    assert!(c.detail.contains("identity.key: mode 644"));
}

#[test]
fn the_no_update_file_adds_a_warn_row() {
    let d = TestDir::new("st-noupdate");
    assert!(checks_core::no_update(&d.0).is_none());
    std::fs::write(d.0.join("no-update"), b"").unwrap();
    let c = checks_core::no_update(&d.0).unwrap();
    assert_eq!(c.status, Status::Warn);
    assert!(c.detail.contains("no-update"));
    assert!(c.remedy.is_some());
}

#[test]
fn command_output_fits_and_keeps_failures_first() {
    let long = "x y ".repeat(150);
    let mut checks: Vec<Check> = (0..400)
        .map(|i| {
            Check::new(
                "m",
                &format!("c{i}"),
                "T",
                Outcome::pass(long.clone()),
                Duration::ZERO,
            )
        })
        .collect();
    checks.push(Check::new(
        "m",
        "bad",
        "T",
        Outcome::fail("broken"),
        Duration::ZERO,
    ));
    let out = command_output(&run(checks), "uploaded");
    assert!(out.len() <= crate::command::MAX_OUTPUT);
    let v = dl_json::parse(&out).unwrap();
    assert_eq!(v["checks"].as_array().map(Vec::len), Some(1));
    assert_eq!(v["summary"]["fail"].as_u64(), Some(1));
}
