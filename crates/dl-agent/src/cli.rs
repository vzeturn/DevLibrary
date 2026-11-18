//! The `dl-agent` command line, reusable by product binaries that add their own modules:
//!
//! ```no_run
//! let app = dl_agent::cli::App::new("dl-agent", "DevLibrary Agent");
//! std::process::exit(dl_agent::cli::main(app));
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_cli::{Arg, Command, Matches};
use dl_winsvc::StopSignal;

use crate::agent::{Agent, Options};
use crate::config::ConfigStore;
use crate::hub::{self, HttpTransport};
use crate::identity::Identity;
use crate::supervisor::Factory;
use crate::{Error, agent_version, now};

/// Exit code for usage and setup errors (`EX_CONFIG`).
pub const EXIT_CONFIG: i32 = 78;
/// Largest log file before it rolls over.
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Rolled log files kept.
pub const LOG_KEEP: usize = 5;

/// A product binary built on the shared runtime.
#[derive(Clone)]
pub struct App {
    /// Service name (Windows SCM / systemd unit) and default data directory name.
    pub service: String,
    /// Human name shown by the service manager.
    pub display: String,
    /// The product's modules (the built-in `sys` module is always added).
    pub modules: Vec<Factory>,
}

impl App {
    /// An app with no modules beyond `sys`.
    pub fn new(service: &str, display: &str) -> App {
        App {
            service: service.into(),
            display: display.into(),
            modules: Vec::new(),
        }
    }

    /// Adds a module.
    pub fn module(mut self, factory: Factory) -> App {
        self.modules.push(factory);
        self
    }

    fn module_names(&self) -> Vec<String> {
        let mut names = vec!["sys".to_string()];
        names.extend(self.modules.iter().map(|f| f().name().to_string()));
        names
    }
}

/// The command-line grammar.
pub fn command(app: &App) -> Command {
    let data = || {
        Arg::option("data")
            .env("DL_AGENT_DATA")
            .help("State directory (default: %ProgramData%\\<service> or /var/lib/<service>)")
    };
    Command::new(app.service.clone())
        .version(agent_version().to_string())
        .about(format!(
            "{}: shared on-prem agent (modules, signed self-update, reports)",
            app.display
        ))
        .subcommand_required()
        .subcommand(
            Command::new("enroll")
                .about("Registers this machine with the hub using a one-time code")
                .arg(
                    Arg::option("hub")
                        .required()
                        .help("Hub URL(s), comma separated, first preferred"),
                )
                .arg(
                    Arg::option("code")
                        .required()
                        .env("DL_AGENT_ENROLL_CODE")
                        .help("One-time enrolment code"),
                )
                .arg(Arg::flag("force").help("Replace an existing identity"))
                .arg(data()),
        )
        .subcommand(
            Command::new("run")
                .about("Runs the agent (console, or as the service)")
                .arg(data()),
        )
        .subcommand(
            Command::new("install")
                .about("Installs and starts the system service")
                .arg(data()),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Stops and removes the system service")
                .arg(
                    Arg::flag("purge")
                        .help("Also delete the state directory (identity, spool, logs)"),
                )
                .arg(data()),
        )
        .subcommand(
            Command::new("status")
                .about("Prints the local state as JSON")
                .arg(data()),
        )
}

/// Parses the process arguments and runs the chosen command; returns the exit code.
pub fn main(app: App) -> i32 {
    let m = command(&app).parse_env_or_exit();
    let Some((name, sub)) = m.subcommand() else {
        return EXIT_CONFIG;
    };
    let dir = data_dir(&app.service, sub);
    let result = match name {
        "enroll" => enroll(&app, &dir, sub),
        "run" => return run(&app, &dir),
        "install" => install(&app, &dir),
        "uninstall" => uninstall(&app, &dir, sub.flag("purge")),
        "status" => {
            println!(
                "{}",
                dl_json::to_string_pretty(&crate::agent::local_status(&dir))
            );
            Ok(())
        }
        _ => return EXIT_CONFIG,
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}: {e}", app.service);
            EXIT_CONFIG
        }
    }
}

/// The state directory: `--data`, `DL_AGENT_DATA`, or the platform default.
pub fn data_dir(service: &str, m: &Matches) -> PathBuf {
    if let Some(d) = m.value("data") {
        return PathBuf::from(d);
    }
    default_data_dir(service)
}

/// `%ProgramData%\<service>`, `/var/lib/<service>`, or `./<service>-data` elsewhere.
pub fn default_data_dir(service: &str) -> PathBuf {
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData")
            .map_or_else(|| PathBuf::from(r"C:\ProgramData"), PathBuf::from);
        base.join(service)
    } else if cfg!(target_os = "linux") {
        PathBuf::from("/var/lib").join(service)
    } else {
        PathBuf::from(format!("{service}-data"))
    }
}

fn enroll(app: &App, dir: &Path, m: &Matches) -> Result<(), Error> {
    if Identity::exists(dir) && !m.flag("force") {
        return Err(Error::Config(format!(
            "{} is already enrolled; pass --force to replace it",
            dir.display()
        )));
    }
    let hubs: Vec<String> = m
        .value("hub")
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let code = m.value("code").unwrap_or("");
    let enrolled = hub::enroll(&HttpTransport::new(), &hubs, code, &app.module_names())?;
    crate::error::io("create data dir", std::fs::create_dir_all(dir))?;
    enrolled.identity.save(dir)?;
    if let Some(config) = enrolled.config.clone() {
        ConfigStore::load(dir)?.apply(config, now())?;
    }
    let id = &enrolled.identity;
    println!(
        "enrolled {} for {}/{} on channel {} ({})",
        id.agent_id,
        id.tenant,
        id.site,
        enrolled.channel,
        dir.display()
    );
    Ok(())
}

fn init_logging(dir: &Path, console: bool) {
    let mut b = dl_log::Logger::builder()
        .level(dl_log::LevelFilter::Info)
        .field("agent_version", agent_version().to_string());
    if let Some(level) = ConfigStore::load(dir)
        .ok()
        .map(|c| c.current.log_level().to_string())
        && let Ok(l) = level.parse::<dl_log::LevelFilter>()
    {
        b = b.level(l);
    }
    match dl_log::FileSink::builder(dir.join("logs").join("agent.log"))
        .max_size(LOG_MAX_BYTES)
        .keep(LOG_KEEP)
        .build()
    {
        Ok(sink) => b = b.sink(sink),
        Err(e) => eprintln!("cannot open log file: {e}"),
    }
    if console {
        b = b.sink(dl_log::ConsoleSink::stderr());
    }
    dl_log::init(b.build());
}

fn run(app: &App, dir: &Path) -> i32 {
    let as_service = dl_winsvc::is_service_process().unwrap_or(false);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("cannot create {}: {e}", dir.display());
        return EXIT_CONFIG;
    }
    init_logging(dir, !as_service);
    if as_service {
        let (app, dir) = (app.clone(), dir.to_path_buf());
        let result = dl_winsvc::run_service_with_exit_code(&app.service.clone(), move |stop| {
            run_agent(&app, &dir, &stop) as u32
        });
        dl_log::shutdown();
        return match result {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("service dispatcher failed: {e}");
                1
            }
        };
    }
    let code = run_agent(app, dir, &StopSignal::new());
    dl_log::shutdown();
    code
}

/// Runs the agent until it stops; returns the process exit code.
fn run_agent(app: &App, dir: &Path, stop: &StopSignal) -> i32 {
    let identity = match Identity::load(dir) {
        Ok(id) => id,
        Err(e) => {
            dl_log::error!("not enrolled, run `enroll` first"; error = %e, data_dir = %dir.display());
            return EXIT_CONFIG;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            dl_log::error!("cannot locate the agent executable"; error = %e);
            return EXIT_CONFIG;
        }
    };
    let opts = Options::new(dir.to_path_buf(), exe);
    match Agent::new(
        opts,
        identity,
        Box::new(HttpTransport::new()),
        app.modules.clone(),
    ) {
        Ok(mut agent) => {
            dl_log::info!("agent started"; data_dir = %dir.display());
            agent.run(stop).code() as i32
        }
        Err(e) => {
            dl_log::error!("agent cannot start"; error = %e);
            EXIT_CONFIG
        }
    }
}

fn exe_path() -> Result<String, Error> {
    let exe = crate::error::io("locate executable", std::env::current_exe())?;
    Ok(exe.display().to_string())
}

fn install(app: &App, dir: &Path) -> Result<(), Error> {
    if !Identity::exists(dir) {
        return Err(Error::NotEnrolled);
    }
    let exe = exe_path()?;
    let data = dir.display().to_string();
    if cfg!(windows) {
        install_windows(app, &exe, &data)
    } else if cfg!(target_os = "linux") {
        install_systemd(app, &exe, &data)
    } else {
        Err(Error::Config(
            "service install is supported on Windows and Linux (systemd)".into(),
        ))
    }
}

fn svc(e: dl_winsvc::Error) -> Error {
    Error::Config(format!("service manager: {e}"))
}

fn install_windows(app: &App, exe: &str, data: &str) -> Result<(), Error> {
    use dl_winsvc::scm::{Manager, RecoveryAction, ServiceConfig, StartType};
    let bin = dl_winsvc::command_line(exe, &["run", "--data", data]);
    let manager = Manager::connect().map_err(svc)?;
    let cfg = ServiceConfig::new(&app.display, StartType::Automatic, &bin)
        .with_description("DevLibrary shared agent: runs product modules, self-updates from the hub and reports problems.");
    let service = manager.create(&app.service, &cfg).map_err(svc)?;
    // Exit 75 (update installed, restart requested) and crashes both restart the service.
    let restart = [
        RecoveryAction::Restart(Duration::from_secs(5)),
        RecoveryAction::Restart(Duration::from_secs(10)),
        RecoveryAction::Restart(Duration::from_secs(60)),
    ];
    service
        .set_recovery_actions(&restart, Duration::from_secs(86_400))
        .map_err(svc)?;
    service
        .set_recovery_on_non_crash_failures(true)
        .map_err(svc)?;
    service.set_delayed_auto_start(true).map_err(svc)?;
    service.start(&[]).map_err(svc)?;
    println!("service {} installed and started", app.service);
    Ok(())
}

/// The systemd unit for `exe run --data <data>`.
pub fn systemd_unit(app: &App, exe: &str, data: &str) -> String {
    format!(
        "[Unit]\nDescription={display}\nWants=network-online.target\nAfter=network-online.target\n\n\
         [Service]\nType=simple\nExecStart=\"{exe}\" run --data \"{data}\"\nRestart=always\nRestartSec=5\n\
         TimeoutStopSec=30\nKillMode=mixed\n\n[Install]\nWantedBy=multi-user.target\n",
        display = app.display
    )
}

fn systemctl(args: &[&str]) -> Result<(), Error> {
    let status = crate::error::io(
        "run systemctl",
        std::process::Command::new("systemctl").args(args).status(),
    )?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "systemctl {} failed: {status}",
            args.join(" ")
        )))
    }
}

fn unit_path(app: &App) -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{}.service", app.service))
}

fn install_systemd(app: &App, exe: &str, data: &str) -> Result<(), Error> {
    crate::store::write_bytes(&unit_path(app), systemd_unit(app, exe, data).as_bytes())?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", &app.service])?;
    println!("unit {} installed and started", app.service);
    Ok(())
}

fn uninstall(app: &App, dir: &Path, purge: bool) -> Result<(), Error> {
    if cfg!(windows) {
        let manager = dl_winsvc::scm::Manager::connect().map_err(svc)?;
        let service = manager.open(&app.service).map_err(svc)?;
        if let Err(e) = service.stop_and_wait(Duration::from_secs(30)) {
            eprintln!("stop: {e}");
        }
        service.delete().map_err(svc)?;
    } else if cfg!(target_os = "linux") {
        systemctl(&["disable", "--now", &app.service])?;
        crate::error::io("remove unit", std::fs::remove_file(unit_path(app)))?;
        systemctl(&["daemon-reload"])?;
    } else {
        return Err(Error::Config(
            "service uninstall is supported on Windows and Linux (systemd)".into(),
        ));
    }
    if purge && dir.exists() {
        crate::error::io("remove data dir", std::fs::remove_dir_all(dir))?;
    }
    println!("service {} removed", app.service);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_is_valid_and_parses() {
        let app = App::new("dl-agent", "DevLibrary Agent");
        let cmd = command(&app);
        cmd.debug_assert();
        let m = cmd
            .parse_from_with_env(
                [
                    "dl-agent",
                    "enroll",
                    "--hub",
                    "https://a,https://b/",
                    "--code",
                    "X",
                ],
                |_| None,
            )
            .unwrap();
        let (name, sub) = m.subcommand().unwrap();
        assert_eq!(name, "enroll");
        assert_eq!(sub.value("hub"), Some("https://a,https://b/"));
        assert!(!sub.flag("force"));
        let m = cmd
            .parse_from_with_env(["dl-agent", "status"], |k| {
                (k == "DL_AGENT_DATA").then(|| "/x".to_string())
            })
            .unwrap();
        assert_eq!(
            data_dir("dl-agent", m.subcommand().unwrap().1),
            PathBuf::from("/x")
        );
        assert!(cmd.parse_from_with_env(["dl-agent"], |_| None).is_err());
    }

    #[test]
    fn systemd_unit_restarts_always() {
        let unit = systemd_unit(
            &App::new("dl-agent", "DevLibrary Agent"),
            "/opt/dl-agent",
            "/var/lib/dl-agent",
        );
        assert!(unit.contains("ExecStart=\"/opt/dl-agent\" run --data \"/var/lib/dl-agent\""));
        assert!(unit.contains("Restart=always"));
    }

    #[test]
    fn module_names_start_with_sys() {
        let app = App::new("x", "X").module(crate::sys_module::factory());
        assert_eq!(app.module_names(), ["sys", "sys"]);
    }
}
