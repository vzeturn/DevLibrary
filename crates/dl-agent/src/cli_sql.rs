//! `sql-setup`: the install-time SQL login choice (PROTOCOL.md §9.8).
//!
//! The password comes only from `DL_AGENT_SQL_PASSWORD` (never the command line) and is
//! never printed; an empty one reuses the password already sealed for the same user.

use std::path::Path;
use std::time::{Duration, Instant};

use dl_cli::{Arg, Command, Matches};
use dl_json::{Value, json};

use crate::Error;
use crate::cli::EXIT_CONFIG;
use crate::report::{ProblemReport, Severity, Spool};
use crate::sql::secret::AuthMode;
use crate::sql::setup::{self, Live, Outcome, Request};

/// Environment variable holding the SQL password.
pub const PASSWORD_ENV: &str = "DL_AGENT_SQL_PASSWORD";
/// Request file a LocalSystem child run reads (no secrets: Windows mode only).
const REQUEST_FILE: &str = "sql-setup.request.json";
/// Result file the child writes.
const RESULT_FILE: &str = "sql-setup.result.json";
/// How long the LocalSystem test may take.
const CHILD_WAIT: Duration = Duration::from_secs(150);

/// The `sql-setup` grammar.
pub fn grammar(data: Arg) -> Command {
    Command::new("sql-setup")
        .about("Chooses the SQL login (windows or sql), tests it on every SMILE database, then saves sql.json")
        .arg(Arg::option("server").env("DL_AGENT_SQL_SERVER").help("SQL Server: ., .\\SQLEXPRESS, host\\instance or host,port (default .)"))
        .arg(Arg::option("auth").env("DL_AGENT_SQL_AUTH").help("windows (the service account) or sql (a SQL login)"))
        .arg(Arg::option("user").env("DL_AGENT_SQL_USER").help("SQL login name (auth sql); the password comes from DL_AGENT_SQL_PASSWORD"))
        .arg(Arg::option("databases").help("Comma list (default: every SMILE_FO, SMILE_POS, SMILE_BO present)"))
        .arg(Arg::flag("test-only").help("Test only; do not save"))
        .arg(Arg::flag("allow-unencrypted").help("SQL login to another computer whose SQL Server has no TLS 1.2: accept an unencrypted login (saved as transport unencrypted)"))
        .arg(Arg::flag("as-service").help("Windows auth: test as LocalSystem, the service account, via a one-shot scheduled task"))
        .arg(Arg::flag("show").help("Print the saved auth, server, user and databases (never the password)"))
        .arg(Arg::flag("json").help("Print the result as one JSON line"))
        .arg(Arg::option("source").help("Where this login came from (JSON, no secret), saved as sql.json source for the hub"))
        .arg(Arg::flag("no-report").help("Do not send a problem report when the test fails (the installer reports once)"))
        .arg(Arg::option("result").help("Also write the result JSON to this file"))
        .arg(Arg::flag("system-child").hidden())
        .arg(data)
}

/// Runs `sql-setup`; exit 0 = tested (and saved), 1 = a database failed, 78 = bad input.
pub fn main(service: &str, dir: &Path, m: &Matches) -> i32 {
    if m.flag("show") {
        return show(dir);
    }
    if m.flag("system-child") {
        return child(dir);
    }
    let req = match request(dir, m) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("sql-setup: {e}");
            return EXIT_CONFIG;
        }
    };
    let outcome = if req.auth == AuthMode::Windows && m.flag("as-service") && cfg!(windows) {
        match as_local_system(service, dir, &req) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("sql-setup: cannot test as LocalSystem: {e}");
                return EXIT_CONFIG;
            }
        }
    } else {
        setup::run(&req, &Live)
    };
    print(&outcome, m.flag("json"));
    if let Some(path) = m.value("result")
        && let Err(e) = crate::store::write_json(Path::new(path), &outcome.to_json())
    {
        eprintln!("sql-setup: cannot write {path}: {e}");
    }
    if !outcome.ok {
        if !m.flag("no-report") {
            spool_failure(dir, &outcome);
        }
        return 1;
    }
    if m.flag("test-only") {
        return 0;
    }
    let source = match m.value("source").map(dl_json::parse).transpose() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("sql-setup: --source is not JSON: {e}");
            return EXIT_CONFIG;
        }
    };
    match setup::save(dir, &req, &outcome).and_then(|()| setup::set_source(dir, source)) {
        Ok(()) => {
            if !m.flag("json") {
                println!(
                    "saved {} database(s) with {} login; restart the agent to apply",
                    outcome.databases.len(),
                    req.auth.as_str()
                );
            }
            0
        }
        Err(e) => {
            eprintln!("sql-setup: {e}");
            EXIT_CONFIG
        }
    }
}

/// Builds the request; an empty SQL password reuses the one sealed for that user.
pub fn request(dir: &Path, m: &Matches) -> Result<Request, Error> {
    let auth = AuthMode::parse(
        m.value("auth")
            .ok_or_else(|| Error::Config("--auth windows|sql is required".into()))?,
    )?;
    let server = m.value("server").unwrap_or(".").trim().to_string();
    let databases = m
        .value("databases")
        .map(|d| {
            d.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut req = Request {
        server: if server.is_empty() {
            ".".into()
        } else {
            server
        },
        auth,
        user: String::new(),
        password: String::new(),
        databases,
        allow_unencrypted: m.flag("allow-unencrypted"),
    };
    if auth == AuthMode::Sql {
        req.user = m.value("user").unwrap_or("").trim().to_string();
        if req.user.is_empty() {
            return Err(Error::Config("--user is required with --auth sql".into()));
        }
        req.password = std::env::var(PASSWORD_ENV).unwrap_or_default();
        if req.password.is_empty() {
            req.password = setup::saved_password(dir, &req.user)?.ok_or_else(|| {
                Error::Config(format!(
                    "set {PASSWORD_ENV}: no password is saved for {}",
                    req.user
                ))
            })?;
        }
    }
    Ok(req)
}

fn show(dir: &Path) -> i32 {
    match setup::saved(dir) {
        Ok(Some(s)) => {
            println!("auth={}", s.auth.as_str());
            println!("server={}", s.server);
            println!("user={}", s.user);
            println!("databases={}", s.databases.join(","));
            0
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("sql-setup: {e}");
            EXIT_CONFIG
        }
    }
}

fn print(o: &Outcome, as_json: bool) {
    if as_json {
        println!("{}", dl_json::to_string(&o.to_json()));
        return;
    }
    println!(
        "SQL login: {} as {} on {}",
        o.auth.as_str(),
        o.login,
        o.server
    );
    if let Some(e) = &o.error {
        println!("  FAILED{}: {}", number(e.number), e.message);
    }
    for d in &o.databases {
        match &d.error {
            None => println!(
                "  {}: ok ({})",
                d.name,
                d.transport.map_or("as_configured", |t| t.name())
            ),
            Some(e) => println!("  {}: FAILED{}: {}", d.name, number(e.number), e.message),
        }
    }
    if let Some(h) = &o.hint {
        println!("This login has no access yet. A DBA can grant it with:\n{h}");
    }
}

fn number(n: Option<i32>) -> String {
    n.map(|n| format!(" (SQL error {n})")).unwrap_or_default()
}

/// A failed setup also goes to the hub as a problem report when this machine is enrolled.
fn spool_failure(dir: &Path, o: &Outcome) {
    if !crate::identity::Identity::exists(dir) {
        return;
    }
    let first = o
        .error
        .iter()
        .chain(o.databases.iter().filter_map(|d| d.error.as_ref()))
        .next()
        .map(|f| f.message.clone())
        .unwrap_or_default();
    let r = ProblemReport::new(
        Severity::Error,
        "agent",
        "sql_setup_failed",
        &format!(
            "SQL {} login as {} failed: {first}",
            o.auth.as_str(),
            o.login
        ),
        crate::now(),
    )
    .with("auth", o.auth.as_str())
    .with("server", o.server.clone())
    .with("login", o.login.clone())
    .with("result", o.to_json());
    if let Err(e) = Spool::open(dir.join("reports.json")).record(r) {
        eprintln!("sql-setup: cannot spool the report: {e}");
    }
}

/// The LocalSystem side: reads the request, tests, writes the result.
fn child(dir: &Path) -> i32 {
    let req = match crate::store::read_json(&dir.join(REQUEST_FILE)) {
        Ok(Some(v)) => Request {
            server: v
                .get("server")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .into(),
            auth: AuthMode::Windows,
            user: String::new(),
            password: String::new(),
            databases: v
                .get("databases")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            // Windows login: the whole ladder is allowed anyway.
            allow_unencrypted: false,
        },
        _ => return EXIT_CONFIG,
    };
    let o = setup::run(&req, &Live);
    match crate::store::write_json(&dir.join(RESULT_FILE), &o.to_json()) {
        Ok(()) if o.ok => 0,
        Ok(()) => 1,
        Err(_) => EXIT_CONFIG,
    }
}

/// The `schtasks` arguments that run the child once as LocalSystem.
pub fn task_args(task: &str, exe: &Path, dir: &Path) -> Vec<String> {
    let tr = format!(
        "\"{}\" sql-setup --system-child --data \"{}\"",
        exe.display(),
        dir.display()
    );
    [
        "/Create", "/F", "/TN", task, "/RU", "SYSTEM", "/SC", "ONCE", "/ST", "00:00", "/RL",
        "HIGHEST", "/TR", &tr,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Tests Windows login as LocalSystem: a one-shot scheduled task runs this binary as
/// SYSTEM, which writes the result; the task is always deleted afterwards.
fn as_local_system(service: &str, dir: &Path, req: &Request) -> Result<Outcome, Error> {
    let exe = crate::error::io("find this program", std::env::current_exe())?;
    let result = dir.join(RESULT_FILE);
    let request = dir.join(REQUEST_FILE);
    let _ = std::fs::remove_file(&result);
    crate::store::write_json(
        &request,
        &json!({"server": req.server, "databases": req.databases.clone()}),
    )?;
    let task = format!("{service}-sqltest");
    let args = task_args(&task, &exe, dir);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = crate::service_watch::run_tool("schtasks", &refs)
        .and_then(|_| crate::service_watch::run_tool("schtasks", &["/Run", "/TN", &task]))
        .and_then(|_| wait_for(&result));
    if let Err(e) = crate::service_watch::run_tool("schtasks", &["/Delete", "/F", "/TN", &task]) {
        eprintln!("sql-setup: cannot delete the task {task}: {e}");
    }
    let _ = std::fs::remove_file(&request);
    let v = run?;
    let _ = std::fs::remove_file(&result);
    Outcome::from_json(&v)
}

fn wait_for(path: &Path) -> Result<Value, Error> {
    let end = Instant::now() + CHILD_WAIT;
    while Instant::now() < end {
        if let Ok(Some(v)) = crate::store::read_json(path) {
            return Ok(v);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(Error::Config(format!(
        "no result from the LocalSystem test within {} s",
        CHILD_WAIT.as_secs()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testdir::TestDir;

    fn matches(args: &[&str]) -> Matches {
        let cmd = Command::new("t").subcommand(grammar(Arg::option("data")));
        let mut argv = vec!["t", "sql-setup"];
        argv.extend_from_slice(args);
        let m = cmd.parse_from_with_env(argv, |_| None).unwrap();
        m.subcommand().unwrap().1.clone()
    }

    #[test]
    fn request_defaults_to_the_local_server_and_needs_a_mode() {
        let d = TestDir::new("cli-sql-req");
        let r = request(&d.0, &matches(&["--auth", "windows"])).unwrap();
        assert_eq!((r.server.as_str(), r.auth), (".", AuthMode::Windows));
        assert!(r.databases.is_empty());
        let r = request(
            &d.0,
            &matches(&[
                "--auth",
                "windows",
                "--server",
                ".\\SQLEXPRESS",
                "--databases",
                "SMILE_FO, SMILE_BO",
            ]),
        )
        .unwrap();
        assert_eq!(r.databases, ["SMILE_FO", "SMILE_BO"]);
        assert!(request(&d.0, &matches(&[])).is_err());
        let e = request(&d.0, &matches(&["--auth", "sql"])).unwrap_err();
        assert!(e.to_string().contains("--user"));
    }

    #[test]
    fn task_runs_this_binary_as_system_with_the_data_dir() {
        let a = task_args(
            "dl-agent-sqltest",
            Path::new("C:\\Program Files\\dl-agent\\dl-agent.exe"),
            Path::new("C:\\ProgramData\\dl-agent"),
        );
        assert_eq!(a[5], "SYSTEM");
        let tr = a.last().unwrap();
        assert_eq!(
            tr,
            "\"C:\\Program Files\\dl-agent\\dl-agent.exe\" sql-setup --system-child --data \"C:\\ProgramData\\dl-agent\""
        );
        assert!(tr.len() < 261, "schtasks /TR limit");
    }

    #[test]
    fn child_writes_a_result_and_refuses_without_a_request() {
        let d = TestDir::new("cli-sql-child");
        assert_eq!(child(&d.0), EXIT_CONFIG);
        assert!(!d.0.join(RESULT_FILE).exists());
    }
}
