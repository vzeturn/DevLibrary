//! The machine setup of `install --hub` (Windows): the steps the one-line installer used to run
//! in PowerShell, now in the signed binary so the script only downloads, verifies and hands off.
//!
//! Order: Administrator check, folders, Defender exclusion, older copies removed, binary
//! placed in Program Files, SQL login, enrolment (kept when enrolled; a code then attaches its
//! product to this agent), module setup, replaced agents, channel/pin, service + recovery +
//! watchdog, Kaspersky hint, first heartbeat.

use std::path::Path;
use std::time::{Duration, Instant};

use dl_winsvc::codes;
use dl_winsvc::scm::{Manager, ServiceConfig, StartType};

use crate::cli::{App, EXIT_CONFIG, svc};
use crate::cli_install::{
    EXIT_FAILED, EXIT_NOT_ADMIN, Failure, MachineOpts, Target, install_exe, running_from,
};
use crate::cli_install_sql::{self as sqlstep, Secret};
use crate::identity::Identity;
use crate::{Error, now, optout};

const DESCRIPTION: &str = "DevLibrary shared agent: runs product modules, self-updates from the hub and reports problems.";
/// Generic hosts never ended by `--replace`: their image name says nothing about the agent.
const GENERIC_HOSTS: [&str; 9] = [
    "svchost",
    "java",
    "javaw",
    "node",
    "dotnet",
    "python",
    "pythonw",
    "powershell",
    "cmd",
];

/// Runs the machine setup; returns the exit code.
pub fn machine(app: &App, dir: &Path, o: &MachineOpts) -> i32 {
    match setup(app, dir, o) {
        Ok(code) => code,
        Err(f) => f.print(),
    }
}

fn config(e: impl std::fmt::Display, next: &str) -> Failure {
    Failure::new(e.to_string(), next, EXIT_CONFIG)
}

fn setup(app: &App, dir: &Path, o: &MachineOpts) -> Result<i32, Failure> {
    if !cfg!(windows) {
        return Err(config(
            "the machine setup runs on Windows",
            "on Linux: enroll --hub <url>, then install",
        ));
    }
    let manager = Manager::connect().map_err(|e| match e.code() {
        Some(codes::ACCESS_DENIED) => Failure::new(
            "not running as Administrator",
            "open PowerShell with Run as administrator and paste the line again",
            EXIT_NOT_ADMIN,
        ),
        _ => config(svc(e), "run again; if it repeats, restart Windows"),
    })?;
    let exe = install_exe(&app.service, crate::cli_install::env_var);
    let install_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    for d in [install_dir.as_path(), dir] {
        std::fs::create_dir_all(d).map_err(|e| {
            config(
                format!("cannot create {}: {e}", d.display()),
                "check the disk",
            )
        })?;
    }
    if o.defender {
        defender_exclusion(&[&install_dir, dir]);
    }
    let enrolled = Identity::exists(dir);
    remove_old_copies(app, &manager, &exe);
    let sha = place_binary(&exe)?;
    println!(
        "Installed {} {} ({})",
        app.product,
        app.version,
        exe.display()
    );
    let mut sql = sqlstep::run(&exe, dir, &o.sql, o.interactive, enrolled, &app.sql_sources)?;
    let attach_failed = enroll_step(app, dir, o, enrolled)?;
    setup_step(&exe, dir, o, &mut sql)?;
    replace_step(app, &manager, &o.replace, &exe)?;
    apply_target(app, dir, o.target.as_ref())?;
    let since = now().saturating_sub(5);
    let (exe_s, data_s) = (exe.display().to_string(), dir.display().to_string());
    register_service(app, &exe_s, &data_s)
        .map_err(|e| config(e, "close the Services console and run again"))?;
    println!("Service {} started; watchdog task registered", app.service);
    kaspersky_hint(&manager, &exe, &sha, dir);
    wait_link(&app.service, dir, since, o.link_timeout);
    if sql.failed {
        return Err(Failure::new(
            "SQL login test (see above); the agent runs with the previous SQL settings",
            format!(
                "{} sql-setup --auth sql --user <login> (or run the installer again)",
                app.service
            ),
            EXIT_FAILED,
        ));
    }
    if let Some(f) = attach_failed {
        return Err(f);
    }
    Ok(0)
}

/// Adds the Windows Defender exclusion for the agent's folders; a failure only warns.
fn defender_exclusion(paths: &[&Path]) {
    let quoted: Vec<String> = paths
        .iter()
        .map(|p| format!("'{}'", p.display().to_string().replace('\'', "''")))
        .collect();
    let script = format!(
        "if (Get-Command Add-MpPreference -ErrorAction SilentlyContinue) {{ Add-MpPreference -ExclusionPath @({}) }}",
        quoted.join(", ")
    );
    if let Err(e) = crate::service_watch::run_tool(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    ) {
        eprintln!("Warning: Windows Defender exclusion not added: {e}");
    }
}

/// The program a service command line runs: the quoted part, else the first word.
pub fn service_program(cmdline: &str) -> String {
    let t = cmdline.trim();
    match t.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or("").to_string(),
        None => t.split(' ').next().unwrap_or("").to_string(),
    }
}

/// The file name of a Windows path.
pub fn base_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Stops the agent's own service (kept, re-pointed later) and deletes older copies installed
/// under another service name; ends leftover processes of the binary.
fn remove_old_copies(app: &App, manager: &Manager, exe: &Path) {
    let image = format!("{}.exe", app.service).to_ascii_lowercase();
    for entry in manager.list_services().unwrap_or_else(|e| {
        eprintln!("Warning: services not listed: {e}");
        Vec::new()
    }) {
        let Ok(s) = manager.open(&entry.name) else {
            continue;
        };
        let Ok(cfg) = s.config() else { continue };
        let program = service_program(&cfg.binary_path_name);
        let own = entry.name.eq_ignore_ascii_case(&app.service);
        if !own && !base_name(&program).eq_ignore_ascii_case(&image) {
            continue;
        }
        if own {
            // First, so the watchdog does not start it again while the binary is replaced.
            crate::service_watch::unregister(app);
            if !crate::cli_install::same_path(Path::new(&program), exe) {
                println!(
                    "Service {} pointed at {program}; repairing the path",
                    entry.name
                );
            }
            if let Err(e) = s.stop_and_wait(Duration::from_secs(30)) {
                eprintln!("Warning: service {} did not stop: {e}", entry.name);
            }
            continue;
        }
        println!("Removing older copy: service {} ({program})", entry.name);
        if let Err(e) = s.stop_and_wait(Duration::from_secs(30)) {
            eprintln!("Warning: service {} did not stop: {e}", entry.name);
        }
        if let Err(e) = s.delete() {
            eprintln!("Warning: service {} not deleted: {e}", entry.name);
        }
    }
    kill_named(&image, |_| true);
}

/// Ends every process running `image` (but this one) that `keep` accepts.
fn kill_named(image: &str, keep: impl Fn(&dl_winsvc::process::Process) -> bool) {
    let me = std::process::id();
    let procs = match dl_winsvc::process::named(image) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Warning: processes not listed: {e}");
            return;
        }
    };
    for p in procs.into_iter().filter(|p| p.pid != me && keep(p)) {
        println!("Ending leftover process {} ({})", p.pid, p.name);
        if let Err(e) =
            crate::service_watch::run_tool("taskkill", &["/F", "/PID", &p.pid.to_string()])
        {
            eprintln!("Warning: {e}");
        }
    }
}

/// Copies this binary to `exe` (unless it runs from there) and returns its SHA-256 (hex).
fn place_binary(exe: &Path) -> Result<String, Failure> {
    let me = std::env::current_exe()
        .map_err(|e| config(format!("cannot locate this executable: {e}"), "run again"))?;
    let bytes = std::fs::read(&me)
        .map_err(|e| config(format!("cannot read {}: {e}", me.display()), "run again"))?;
    let sha = dl_encoding::hex::encode(<dl_hash::Sha256 as dl_hash::Digest>::digest(&bytes));
    if running_from(exe) {
        return Ok(sha);
    }
    let new = exe.with_extension("exe.new");
    if let Err(e) = crate::store::write_bytes(&new, &bytes) {
        return Err(av_failure(&new, &e.to_string()));
    }
    let mut last = String::new();
    for _ in 0..10 {
        match std::fs::rename(&new, exe) {
            Ok(()) => return Ok(sha),
            Err(e) => last = e.to_string(),
        }
        // A running copy cannot be overwritten but can be moved aside.
        let old = exe.with_extension("exe.old");
        if exe.exists() && std::fs::rename(exe, &old).is_ok() {
            continue;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if !new.exists() {
        return Err(av_failure(
            &new,
            "the file vanished right after it was written",
        ));
    }
    Err(config(
        format!("cannot replace {}: {last}", exe.display()),
        "end the running agent (Task Manager) and run again",
    ))
}

fn av_failure(path: &Path, why: &str) -> Failure {
    Failure::new(
        format!(
            "{} could not be written: {why} (antivirus?)",
            path.display()
        ),
        format!(
            "allow {} in the antivirus, then run again",
            path.parent()
                .map_or_else(String::new, |p| p.display().to_string())
        ),
        EXIT_FAILED,
    )
}

/// Enrols (not enrolled, or `--reenroll`), or on an enrolled machine given a code adds that
/// code's product to this agent (RT-0298). `Ok(Some)`: the attach failed; reported at the end so
/// the service still comes back with the products it had.
fn enroll_step(
    app: &App,
    dir: &Path,
    o: &MachineOpts,
    enrolled: bool,
) -> Result<Option<Failure>, Failure> {
    if enrolled && !o.reenroll {
        let code = o.code.as_deref().map_or("", str::trim);
        if code.is_empty() {
            println!("Already enrolled: keeping this machine's identity");
            return Ok(None);
        }
        let transport = Box::new(crate::hub::HttpTransport::new());
        return Ok(crate::cli_attach::install_step(dir, code, transport));
    }
    let code = match &o.code {
        Some(c) => c.clone(),
        None if o.interactive => sqlstep::ask("Enrolment code", ""),
        None => String::new(),
    };
    let next =
        "ask the hub admin for an enrolment code; set DL_AGENT_ENROLL_CODE or type it when asked";
    if code.trim().is_empty() {
        return Err(Failure::new(
            "an enrolment code is needed",
            next,
            EXIT_FAILED,
        ));
    }
    let hubs = if o.hubs.is_empty() {
        Identity::load(dir).map(|i| i.hub_urls).unwrap_or_default()
    } else {
        o.hubs.clone()
    };
    if hubs.is_empty() {
        return Err(config(
            "no hub to enrol with",
            "pass --hub <url> (the one-line installer does)",
        ));
    }
    crate::cli::enroll_with(app, dir, &hubs, code.trim(), enrolled)
        .map(|()| None)
        .map_err(|e| {
            Failure::new(
                format!("enrolment failed: {e}"),
                "the code may be used, expired or revoked: ask the hub admin for a new one",
                EXIT_FAILED,
            )
        })
}

/// Runs the module setup (`--setup`); `arrival-enroll` takes the same SQL sign-in through env.
fn setup_step(
    exe: &Path,
    dir: &Path,
    o: &MachineOpts,
    sql: &mut sqlstep::SqlDone,
) -> Result<(), Failure> {
    let password: Secret = std::mem::take(&mut sql.password);
    let Some(setup) = &o.setup else {
        return Ok(());
    };
    println!("Running setup: {setup}");
    let data = dir.display().to_string();
    let mut c = sqlstep::agent(exe);
    c.args([setup.as_str(), "--data", &data]);
    if setup == "arrival-enroll" {
        if let Some(a) = o.sql.auth.as_ref().or(sql.auth.as_ref()) {
            c.env("DL_AGENT_ARRIVAL_SQL_AUTH", a);
        }
        if let Some(u) = o.sql.user.as_ref().or(sql.user.as_ref()) {
            c.env("DL_AGENT_ARRIVAL_SQL_USER", u);
        }
        if !password.is_empty() {
            c.env(crate::cli_sql::PASSWORD_ENV, password.expose());
        }
    }
    let status = c.status();
    drop(c);
    drop(password);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Failure::new(
            format!("setup {setup} failed ({s}, see above)"),
            "fix what it says and run again; the previous agent, if any, still runs",
            EXIT_FAILED,
        )),
        Err(e) => Err(Failure::new(
            format!("cannot run setup {setup}: {e}"),
            "run again",
            EXIT_FAILED,
        )),
    }
}

/// Case-insensitive glob with `*` and `?` (PowerShell `-like`).
pub fn like(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

/// Whether a process of a replaced service may be ended: a service-session process whose image
/// is not a generic host.
pub fn may_end(image: &str, session: u32) -> bool {
    let stem = image
        .strip_suffix(".exe")
        .or_else(|| image.strip_suffix(".EXE"))
        .unwrap_or(image);
    session == 0 && !GENERIC_HOSTS.iter().any(|g| g.eq_ignore_ascii_case(stem))
}

/// Stops and deletes the services of older agents (`--replace` patterns); their files stay.
fn replace_step(
    app: &App,
    manager: &Manager,
    patterns: &[String],
    exe: &Path,
) -> Result<(), Failure> {
    if patterns.is_empty() {
        return Ok(());
    }
    let entries = manager.list_services().map_err(|e| {
        config(
            format!("services not listed: {e}"),
            "run again as Administrator",
        )
    })?;
    for entry in entries {
        if entry.name.eq_ignore_ascii_case(&app.service)
            || !patterns
                .iter()
                .any(|p| like(p, &entry.name) || like(p, &entry.display_name))
        {
            continue;
        }
        let s = manager
            .open(&entry.name)
            .map_err(|e| config(svc(e), "run again as Administrator"))?;
        let program = s
            .config()
            .map(|c| service_program(&c.binary_path_name))
            .unwrap_or_default();
        if crate::cli_install::same_path(Path::new(&program), exe) {
            continue;
        }
        println!("Replacing {} ({program}); its files are kept", entry.name);
        if let Err(e) = s.stop_and_wait(Duration::from_secs(30)) {
            eprintln!("Warning: service {} did not stop: {e}", entry.name);
        }
        s.delete().map_err(|e| {
            Failure::new(
                format!("could not delete service {}: {e}", entry.name),
                "close the Services console and run again",
                EXIT_FAILED,
            )
        })?;
        let image = base_name(&program).to_string();
        if may_end(&image, 0) {
            kill_named(&image, |p| may_end(&p.name, p.session_id));
        }
    }
    Ok(())
}

/// Records the channel to follow or the version to hold (PROTOCOL.md §6.5).
fn apply_target(app: &App, dir: &Path, target: Option<&Target>) -> Result<(), Failure> {
    let fail = |e: Error| config(e, "run again as Administrator");
    match target {
        Some(t @ (Target::Stable | Target::Canary)) => {
            let c = t.channel().unwrap_or("stable");
            optout::set_channel(dir, Some(c)).map_err(fail)?;
            optout::set_pin(dir, None).map_err(fail)?;
            println!("Following the {c} channel");
        }
        Some(Target::Version(v)) => {
            if *v != app.version {
                eprintln!(
                    "Warning: this build is {}, not {v}: pinning {}",
                    app.version, app.version
                );
            }
            optout::set_pin(dir, Some(&app.version)).map_err(fail)?;
            println!(
                "Pinned at {}: `{} install stable` follows a channel again",
                app.version, app.service
            );
        }
        Some(Target::Auto) => {
            optout::set_channel(dir, None).map_err(fail)?;
            optout::set_pin(dir, None).map_err(fail)?;
            println!("Following the channel the hub assigns");
        }
        None => {
            if let Some(p) = optout::pin(dir).filter(|p| *p != app.version) {
                optout::set_pin(dir, None).map_err(fail)?;
                println!("Pin {p} removed: this run installed {}", app.version);
            }
        }
    }
    Ok(())
}

/// Creates or updates the service for `exe run --data <data>`, sets recovery and delayed
/// start, starts it and registers the watchdog task. Safe to run again.
pub(crate) fn register_service(app: &App, exe: &str, data: &str) -> Result<(), Error> {
    let bin = dl_winsvc::command_line(exe, &["run", "--data", data]);
    let manager = Manager::connect().map_err(svc)?;
    let cfg =
        ServiceConfig::new(&app.display, StartType::Automatic, &bin).with_description(DESCRIPTION);
    let service = match manager.open(&app.service) {
        Ok(s) => {
            s.update_config(&cfg).map_err(svc)?;
            s
        }
        Err(e) if e.code() == Some(codes::SERVICE_DOES_NOT_EXIST) => {
            manager.create(&app.service, &cfg).map_err(svc)?
        }
        Err(e) => return Err(svc(e)),
    };
    // Exit 75 (update installed, restart requested) and crashes both restart the service.
    crate::service_watch::set_recovery(&service)?;
    service.set_delayed_auto_start(true).map_err(svc)?;
    match service.start(&[]) {
        Ok(()) => {}
        Err(e) if e.code() == Some(codes::SERVICE_ALREADY_RUNNING) => {}
        Err(e) => return Err(svc(e)),
    }
    crate::service_watch::register(app, exe, data)
}

/// Kaspersky deletes unknown binaries quietly: name what to allow.
fn kaspersky_hint(manager: &Manager, exe: &Path, sha: &str, dir: &Path) {
    let found = manager.list_services().is_ok_and(|l| {
        l.iter().any(|e| {
            e.name.to_ascii_lowercase().starts_with("avp")
                || e.name.eq_ignore_ascii_case("klnagent")
                || e.display_name.to_ascii_lowercase().contains("kaspersky")
        })
    });
    if found {
        eprintln!(
            "Warning: Kaspersky is installed. Add {} as a Trusted application (SHA-256 {sha}) and exclude {} from scanning.",
            exe.display(),
            dir.display()
        );
    }
}

/// Waits for a heartbeat answered by the hub after `since`; prints the last log lines if none.
fn wait_link(service: &str, dir: &Path, since: u64, timeout: u64) -> bool {
    println!("Waiting up to {timeout} s for the agent to reach the hub...");
    let end = Instant::now() + Duration::from_secs(timeout);
    while Instant::now() < end {
        if crate::watchdog::read_beat(dir).is_some_and(|b| b.hub_at >= since) {
            println!("Connected to the hub.");
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    eprintln!(
        "Warning: NOT connected after {timeout} s (the service keeps trying). Last log lines:"
    );
    for line in last_lines(&dir.join("logs").join("agent.log"), 15) {
        eprintln!("  {line}");
    }
    eprintln!("Next: check the proxy or firewall towards the hub, then: {service} status");
    false
}

fn last_lines(path: &Path, n: usize) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => {
            let lines: Vec<&str> = t.lines().collect();
            lines[lines.len().saturating_sub(n)..]
                .iter()
                .map(|s| s.to_string())
                .collect()
        }
        Err(e) => vec![format!("({} not readable: {e})", path.display())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_matches_powershell_wildcards_without_case() {
        assert!(like("MM.Arrival*", "mm.arrival.agent"));
        assert!(like("*Check-In*", "Azolo Check-In Agent"));
        assert!(like("dl-?gent", "DL-AGENT"));
        assert!(!like("MM.Arrival*", "dl-agent"));
        assert!(!like("a?", "a"));
    }

    #[test]
    fn a_service_command_line_names_its_program() {
        assert_eq!(
            service_program("\"C:\\Program Files\\x\\a.exe\" run --data C:\\d"),
            "C:\\Program Files\\x\\a.exe"
        );
        assert_eq!(service_program("C:\\x\\a.exe -k"), "C:\\x\\a.exe");
        assert_eq!(
            base_name("C:\\x\\MM.Arrival.Agent.exe"),
            "MM.Arrival.Agent.exe"
        );
    }

    #[test]
    fn replace_never_ends_generic_hosts_or_desktop_processes() {
        assert!(may_end("MM.Arrival.Agent.exe", 0));
        assert!(!may_end("MM.Arrival.Agent.exe", 1));
        assert!(!may_end("java.exe", 0));
        assert!(!may_end("SVCHOST.EXE", 0));
    }

    #[test]
    fn the_target_writes_the_channel_choice_or_the_pin() {
        let d = crate::store::testdir::TestDir::new("install-target");
        let app = App::new("dl-agent", "DevLibrary Agent");
        apply_target(&app, &d.0, Some(&Target::Canary)).unwrap();
        assert_eq!(optout::channel(&d.0).as_deref(), Some("canary"));
        apply_target(&app, &d.0, Some(&Target::Version("0.0.1".parse().unwrap()))).unwrap();
        assert_eq!(optout::pin(&d.0), Some(app.version.clone()));
        apply_target(&app, &d.0, None).unwrap(); // same version: the pin stays
        assert_eq!(optout::pin(&d.0), Some(app.version.clone()));
        optout::set_pin(&d.0, Some(&"0.0.1".parse().unwrap())).unwrap();
        apply_target(&app, &d.0, None).unwrap(); // another version was installed: unpinned
        assert_eq!(optout::pin(&d.0), None);
        apply_target(&app, &d.0, Some(&Target::Stable)).unwrap();
        assert_eq!(optout::channel(&d.0).as_deref(), Some("stable"));
        apply_target(&app, &d.0, Some(&Target::Auto)).unwrap();
        assert_eq!((optout::channel(&d.0), optout::pin(&d.0)), (None, None));
    }
}
