//! One full run (core checks, then every module's), its upload, and the hook the service's
//! `sys` module calls for the `selftest` command.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use dl_json::{Value, json};

use super::{
    CORE, Check, MODULE_TIMEOUT, Outcome, PROBE_TIMEOUT, Run, Status, Trigger, checks_core,
    checks_hub, checks_state, legacy, run_id, runner,
};
use crate::cli::App;
use crate::command::MAX_OUTPUT;
use crate::config::{ConfigStore, RemoteConfig};
use crate::hub::{HttpTransport, HubClient};
use crate::identity::Identity;
use crate::sql::Site;
use crate::sys_module::SelfTestHook;
use crate::{Error, now};

/// Each registered module's name and `enabled_by_default`.
pub(crate) fn module_defaults(app: &App) -> Vec<(String, bool)> {
    app.modules
        .iter()
        .map(|f| {
            let m = f();
            (m.name().to_string(), m.enabled_by_default())
        })
        .collect()
}

fn config(dir: &Path) -> (Check, RemoteConfig) {
    let started = Instant::now();
    let (outcome, config) = match ConfigStore::load(dir) {
        Ok(s) if s.current.version == 0 => (
            Outcome::warn("no remote config received yet: every module runs with its defaults")
                .remedy("the hub sends it on the first accepted heartbeat"),
            s.current,
        ),
        Ok(s) => (
            Outcome::pass(format!("remote config version {}", s.current.version)),
            s.current,
        ),
        Err(e) => (
            Outcome::fail(format!("config.json unreadable: {e}"))
                .remedy("restart the service: it fetches the config again"),
            RemoteConfig::default(),
        ),
    };
    let check = Check::new(CORE, "config", "Remote config", outcome, started.elapsed());
    (check, config)
}

/// Runs the self-test. `only` limits it to `core` or one module; `args` may carry a schema
/// requirement for the databases (`{requirement, database?}`, as `sql.probe`).
pub fn execute(app: &App, dir: &Path, trigger: Trigger, only: Option<&str>, args: &Value) -> Run {
    let started_at = now();
    let (config_check, config) = config(dir);
    let site = Site::load(dir);
    let defaults = module_defaults(app);
    let enabled: Vec<String> = defaults
        .iter()
        .filter(|(n, d)| config.module_enabled_or(n, *d))
        .map(|(n, _)| n.clone())
        .collect();
    let mut checks = Vec::new();
    if only.is_none_or(|o| o == CORE) {
        let (identity, id) = checks_core::identity(app, dir);
        let hub_urls = checks_state::hub_urls(id.as_ref());
        let (service, state) = checks_core::service(app);
        let (hub, link) = checks_hub::hub(app, dir, id, state, trigger);
        checks.push(checks_hub::version(app, &link.channel));
        checks.push(identity);
        checks.push(config_check);
        checks.push(checks_core::permissions(dir));
        checks.push(service);
        checks.push(checks_core::watchdog(app));
        checks.push(hub);
        checks.push(hub_urls);
        checks.push(checks_state::proxy());
        checks.push(checks_hub::clock(link.skew));
        checks.push(checks_hub::update(app, &link));
        checks.push(checks_state::update_state(
            dir,
            &app.version,
            crate::fault::COMPILED,
        ));
        checks.extend(checks_core::no_update(dir));
        checks.push(checks_hub::spool(dir, &link));
        checks.extend(checks_core::databases(dir, &site, args));
        checks.push(legacy::check(&enabled));
    }
    if only != Some(CORE) {
        let plans = runner::plan(&defaults, &app.selftests, &config, only);
        checks.extend(runner::run_modules(
            &plans,
            dir,
            &config,
            &site.pools,
            MODULE_TIMEOUT,
        ));
    }
    Run {
        run_id: run_id(),
        trigger,
        started_at,
        finished_at: now(),
        agent_version: app.version.to_string(),
        os: std::env::consts::OS.into(),
        hostname: crate::hub::hostname(),
        checks,
    }
}

/// Keeps `run` as [`super::LAST_RUN_FILE`] in `dir` for the diagnostics bundle. A file that
/// cannot be written is logged: the run itself is still answered and uploaded.
pub fn save_last(run: &Run, dir: &Path) {
    let text = dl_json::to_string_pretty(&run.to_json());
    if let Err(e) = std::fs::write(dir.join(super::LAST_RUN_FILE), text) {
        dl_log::warn!("selftest: cannot keep the last run"; error = %e);
    }
}

/// Sends the run to the hub (`Ok(false)`: the hub is too old to take it).
pub fn upload(run: &Run, dir: &Path) -> Result<bool, Error> {
    let identity = Identity::load(dir)?;
    let transport = HttpTransport::with_timeout(PROBE_TIMEOUT * 2);
    HubClient::new(Box::new(transport), identity).selftest(&run.to_json())
}

/// Upload result in a few words, logged when it fails (a self-test still reports locally).
pub fn upload_note(run: &Run, dir: &Path) -> String {
    match upload(run, dir) {
        Ok(true) => "uploaded".into(),
        Ok(false) => {
            dl_log::info!("selftest: the hub has no self-test endpoint yet"; run_id = %run.run_id);
            "not uploaded: the hub has no self-test endpoint yet".into()
        }
        Err(Error::NotEnrolled) => "not uploaded: not enrolled".into(),
        Err(e) => {
            dl_log::warn!("selftest: upload failed"; run_id = %run.run_id, error = %e);
            format!("upload failed: {e}")
        }
    }
}

/// The command answer: summary and checks, cut to fit [`MAX_OUTPUT`] (only the checks
/// that did not pass, then only their ids).
pub fn command_output(run: &Run, upload: &str) -> String {
    let head = |checks: Vec<Value>| {
        dl_json::to_string(&json!({
            "run_id": run.run_id, "summary": run.summary(), "upload": upload, "checks": checks,
        }))
    };
    let all = head(run.checks.iter().map(super::Check::to_json).collect());
    if all.len() <= MAX_OUTPUT {
        return all;
    }
    let not_pass = run.checks.iter().filter(|c| c.status != Status::Pass);
    let short = head(not_pass.clone().map(super::Check::to_json).collect());
    if short.len() <= MAX_OUTPUT {
        return short;
    }
    head(
        not_pass
            .map(|c| json!({"module": c.module, "id": c.id, "status": c.status.as_str()}))
            .collect(),
    )
}

/// The hook the agent's `sys` module runs for the `selftest` command. `args.module` limits
/// the run like `--module`.
pub fn hook(app: &App, dir: &Path) -> SelfTestHook {
    let app = app.clone();
    let dir = dir.to_path_buf();
    SelfTestHook(Arc::new(move |args: &Value| {
        let only = args.get("module").and_then(Value::as_str);
        let run = execute(&app, &dir, Trigger::Command, only, args);
        save_last(&run, &dir);
        let note = upload_note(&run, &dir);
        dl_log::info!("selftest finished"; run_id = %run.run_id, summary = %dl_json::to_string(&run.summary()), upload = %note);
        (!run.failed(), command_output(&run, &note))
    }))
}
