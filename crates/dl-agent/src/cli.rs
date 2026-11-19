//! The `dl-agent` command line, reusable by product binaries that add their own modules:
//!
//! ```no_run
//! let app = dl_agent::cli::App::new("dl-agent", "DevLibrary Agent");
//! std::process::exit(dl_agent::cli::main(app));
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use dl_cli::{Arg, Command, Matches};
use dl_json::Value;
use dl_winsvc::StopSignal;

use crate::agent::{Agent, Options};
use crate::config::ConfigStore;
use crate::hub::{self, HttpTransport};
use crate::identity::Identity;
use crate::supervisor::Factory;
use crate::tool::{SETUP_ARGS_ENV, Tool, ToolContext, with_setup_args};
use crate::{Error, Version, agent_version, now};

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
    /// Sub-commands the modules add (setup steps such as `einvoice-enroll`).
    pub tools: Vec<Tool>,
    /// The modules' self-tests (`selftest`).
    pub selftests: Vec<crate::selftest::SelfTest>,
    /// Release line on the hub (default `dl-agent`).
    pub product: String,
    /// Version of this binary on that line (default: the `dl-agent` crate version).
    pub version: Version,
}

impl App {
    /// An app with no modules beyond `sys`.
    pub fn new(service: &str, display: &str) -> App {
        App {
            service: service.into(),
            display: display.into(),
            modules: Vec::new(),
            tools: Vec::new(),
            selftests: Vec::new(),
            product: crate::agent::PRODUCT.into(),
            version: agent_version(),
        }
    }

    /// Sets the release line and version a product binary follows, e.g.
    /// `.release("azolo-agent", env!("CARGO_PKG_VERSION").parse().unwrap())`.
    pub fn release(mut self, product: &str, version: Version) -> App {
        self.product = product.into();
        self.version = version;
        self
    }

    /// Adds a module.
    pub fn module(mut self, factory: Factory) -> App {
        self.modules.push(factory);
        self
    }

    /// Adds a module sub-command. Its name must not clash with a built-in one.
    pub fn tool(mut self, tool: Tool) -> App {
        self.tools.push(tool);
        self
    }

    /// Adds a module's self-test (run by `selftest` and the `selftest` command).
    pub fn selftest(mut self, test: crate::selftest::SelfTest) -> App {
        self.selftests.push(test);
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
    let cmd = Command::new(app.service.clone())
        .version(app.version.to_string())
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
            Command::new("watchdog")
                .about("Checks the service once and starts, kills or restarts it when stuck (run by the scheduler)")
                .arg(data()),
        )
        .subcommand(
            Command::new("status")
                .about("Prints the local state as JSON")
                .arg(data()),
        )
        .subcommand(
            Command::new("sql-add")
                .about("Declares an on-prem SQL Server database in sql.json (restart to apply)")
                .arg(
                    Arg::positional("name")
                        .required()
                        .help("Database key, e.g. SMILE_FO"),
                )
                .arg(
                    Arg::option("connection")
                        .required()
                        .env("DL_AGENT_SQL_CONNECTION")
                        .help("Connection string (ADO or sqlserver:// URL)"),
                )
                .arg(Arg::option("max").help("Most connections at once (default 4)"))
                .arg(data()),
        )
        .subcommand(
            Command::new("sql-test")
                .about("Connects to every declared database (or one) and prints the result")
                .arg(Arg::positional("name").help("Only this database"))
                .arg(data()),
        )
        .subcommand(crate::cli_sql::grammar(data()))
        .subcommand(crate::selftest::cli::grammar(data()));
    app.tools.iter().fold(cmd, |cmd, t| {
        cmd.subcommand((t.grammar)(Command::new(t.name)).arg(data()))
    })
}

/// Parses the process arguments and runs the chosen command; returns the exit code.
pub fn main(app: App) -> i32 {
    let setup = std::env::var(SETUP_ARGS_ENV).ok();
    let args = with_setup_args(std::env::args_os().collect(), &app.tools, setup.as_deref());
    let m = command(&app).parse_from(args).unwrap_or_else(|e| e.exit());
    let Some((name, sub)) = m.subcommand() else {
        return EXIT_CONFIG;
    };
    let dir = data_dir(&app.service, sub);
    let result = match name {
        "enroll" => enroll(&app, &dir, sub),
        "run" => return run(&app, &dir),
        "install" => install(&app, &dir),
        "uninstall" => uninstall(&app, &dir, sub.flag("purge")),
        "watchdog" => {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!("cannot create {}: {e}", dir.display());
                return EXIT_CONFIG;
            }
            init_logging(&app, &dir, true, "watchdog.log");
            let code = crate::service_watch::run(&app, &dir);
            dl_log::shutdown();
            return code;
        }
        "sql-add" => sql_add(&dir, sub),
        "sql-test" => return sql_test(&dir, sub.value("name")),
        "sql-setup" => return crate::cli_sql::main(&app.service, &dir, sub),
        crate::selftest::COMMAND => return crate::selftest::cli::main(&app, &dir, sub),
        "status" => {
            println!(
                "{}",
                dl_json::to_string_pretty(&crate::agent::local_status(
                    &dir,
                    &app.product,
                    &app.version
                ))
            );
            Ok(())
        }
        other => match app.tools.iter().find(|t| t.name == other) {
            Some(t) => (t.run)(
                &ToolContext {
                    data_dir: &dir,
                    product: &app.product,
                    version: &app.version,
                },
                sub,
            ),
            None => return EXIT_CONFIG,
        },
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}: {e}", app.service);
            EXIT_CONFIG
        }
    }
}

fn sql_add(dir: &Path, m: &Matches) -> Result<(), Error> {
    let name = m.value("name").unwrap_or_default();
    let connection = m.value("connection").unwrap_or_default();
    let max = match m.value("max") {
        None => 4,
        Some(v) => v
            .parse()
            .map_err(|_| Error::Config("--max must be a number".into()))?,
    };
    crate::sql::declare(dir, name, connection, max)?;
    println!("declared {name}; restart the agent to apply");
    Ok(())
}

/// Checks the declared databases; exit 0 only when every checked one answers.
fn sql_test(dir: &Path, only: Option<&str>) -> i32 {
    let site = crate::sql::Site::load(dir);
    if let Some(e) = site.load_error {
        eprintln!("{e}");
        return EXIT_CONFIG;
    }
    let mut all_ok = true;
    let mut checked = 0;
    for pool in site.pools.all() {
        if only.is_some_and(|o| o != pool.name()) {
            continue;
        }
        checked += 1;
        match pool.check() {
            Ok(d) => {
                let t = pool
                    .stats()
                    .get("transport")
                    .cloned()
                    .unwrap_or(Value::Null);
                let rung = t.get("rung").and_then(Value::as_str).unwrap_or("-");
                let auth = t.get("auth").and_then(Value::as_str).unwrap_or("-");
                println!(
                    "{}: ok ({} ms, transport {rung}, auth {auth})",
                    pool.name(),
                    d.as_millis()
                );
            }
            Err(e) => {
                all_ok = false;
                println!("{}: FAILED {e}", pool.name());
            }
        }
    }
    if checked == 0 {
        eprintln!(
            "no database {} in {}",
            only.unwrap_or("declared"),
            crate::sql::SQL_FILE
        );
        return EXIT_CONFIG;
    }
    if all_ok { 0 } else { 1 }
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

fn init_logging(app: &App, dir: &Path, console: bool, file: &str) {
    let mut b = dl_log::Logger::builder()
        .level(dl_log::LevelFilter::Info)
        .field("product", app.product.clone())
        .field("version", app.version.to_string());
    if let Some(level) = ConfigStore::load(dir)
        .ok()
        .map(|c| c.current.log_level().to_string())
        && let Ok(l) = level.parse::<dl_log::LevelFilter>()
    {
        b = b.level(l);
    }
    match dl_log::FileSink::builder(dir.join("logs").join(file))
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
    init_logging(app, dir, !as_service, "agent.log");
    if as_service {
        let (app, dir) = (app.clone(), dir.to_path_buf());
        crate::service_watch::ensure(&app, &dir);
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
    let mut opts = Options::new(dir.to_path_buf(), exe);
    opts.product = app.product.clone();
    opts.version = app.version.clone();
    opts.selftest = Some(crate::selftest::hook(app, dir));
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

pub(crate) fn svc(e: dl_winsvc::Error) -> Error {
    Error::Config(format!("service manager: {e}"))
}

fn install_windows(app: &App, exe: &str, data: &str) -> Result<(), Error> {
    use dl_winsvc::scm::{Manager, ServiceConfig, StartType};
    let bin = dl_winsvc::command_line(exe, &["run", "--data", data]);
    let manager = Manager::connect().map_err(svc)?;
    let cfg = ServiceConfig::new(&app.display, StartType::Automatic, &bin)
        .with_description("DevLibrary shared agent: runs product modules, self-updates from the hub and reports problems.");
    let service = manager.create(&app.service, &cfg).map_err(svc)?;
    // Exit 75 (update installed, restart requested) and crashes both restart the service.
    crate::service_watch::set_recovery(&service)?;
    service.set_delayed_auto_start(true).map_err(svc)?;
    service.start(&[]).map_err(svc)?;
    crate::service_watch::register(app, exe, data)?;
    println!(
        "service {} installed and started, watchdog task registered",
        app.service
    );
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

pub(crate) fn unit_path(app: &App) -> PathBuf {
    PathBuf::from("/etc/systemd/system").join(format!("{}.service", app.service))
}

fn install_systemd(app: &App, exe: &str, data: &str) -> Result<(), Error> {
    crate::store::write_bytes(&unit_path(app), systemd_unit(app, exe, data).as_bytes())?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", &app.service])?;
    crate::service_watch::register(app, exe, data)?;
    println!(
        "unit {} installed and started, watchdog timer enabled",
        app.service
    );
    Ok(())
}

fn uninstall(app: &App, dir: &Path, purge: bool) -> Result<(), Error> {
    // First, so the watchdog does not start the service again while it is removed.
    crate::service_watch::unregister(app);
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
    fn module_tools_become_sub_commands_with_data() {
        let app = App::new("x", "X").tool(Tool {
            name: "demo-setup",
            grammar: |c| {
                c.about("Demo")
                    .arg(Arg::option("token").required().help("Token"))
            },
            run: |_, _| Ok(()),
        });
        let cmd = command(&app);
        cmd.debug_assert();
        let m = cmd
            .parse_from_with_env(["x", "demo-setup", "--token", "t", "--data", "/d"], |_| {
                None
            })
            .unwrap();
        let (name, sub) = m.subcommand().unwrap();
        assert_eq!(name, "demo-setup");
        assert_eq!(sub.value("token"), Some("t"));
        assert_eq!(data_dir("x", sub), PathBuf::from("/d"));
        assert!(
            cmd.parse_from_with_env(["x", "demo-setup"], |_| None)
                .is_err()
        );
    }

    #[test]
    fn module_names_start_with_sys() {
        let app = App::new("x", "X").module(crate::sys_module::factory());
        assert_eq!(app.module_names(), ["sys", "sys"]);
    }
}
