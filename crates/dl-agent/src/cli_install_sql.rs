//! The SQL login step of the machine setup (`install --hub`): what the one-line installer's
//! `Read-SqlLogin` / `Set-SqlLoginUnattended` did, now in the signed binary.
//!
//! The password never reaches argv, a log or the console: it is typed with echo off
//! ([`read_secret`]), comes from `DL_AGENT_SQL_PASSWORD`, or is the old Check-In agent's
//! (`DL_AGENT_SQL_LEGACY`, handed over by the installer script), and travels to the
//! `sql-setup` child in that child's environment only.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Command;

use crate::cli_install::Failure;
use crate::cli_sql::PASSWORD_ENV;
use crate::sql::secret::AuthMode;

/// The old Check-In agent's connection string, from the installer script (`Get-ArrivalLegacySql`).
pub const LEGACY_ENV: &str = "DL_AGENT_SQL_LEGACY";
/// Where [`LEGACY_ENV`] came from, in words.
pub const LEGACY_FROM_ENV: &str = "DL_AGENT_SQL_LEGACY_FROM";
/// One database's connection for `--sql-name` (`sql-add`).
pub const CONNECTION_ENV: &str = "DL_AGENT_SQL_CONNECTION";
/// Secrets the installer's children never inherit unless handed one on purpose.
pub const CHILD_SECRETS: [&str; 5] = [
    LEGACY_ENV,
    CONNECTION_ENV,
    PASSWORD_ENV,
    "DL_AGENT_ENROLL_CODE",
    "DL_AGENT_SQL_LEGACY_FROM",
];

/// The SQL options of `install`.
#[derive(Clone, Debug, Default)]
pub struct SqlOpts {
    /// `--sql-name`: declare one database from `DL_AGENT_SQL_CONNECTION`.
    pub name: Option<String>,
    /// `--sql-auth windows|sql`: unattended.
    pub auth: Option<String>,
    /// `--sql-server`.
    pub server: Option<String>,
    /// `--sql-user`.
    pub user: Option<String>,
    /// `--sql-prompt`: ask even with no local SQL Server.
    pub prompt: bool,
    /// `--skip-sql`.
    pub skip: bool,
}

/// A password in memory: its bytes are overwritten when it is dropped.
#[derive(Default)]
pub struct Secret(String);

impl Secret {
    /// Wraps `s`.
    pub fn new(s: String) -> Secret {
        Secret(s)
    }

    /// The text.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Nothing typed or given.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// How the SQL step ended.
#[derive(Debug, Default)]
pub struct SqlDone {
    /// The test failed: nothing new was saved.
    pub failed: bool,
    /// The SQL password, kept for `arrival-enroll` (it takes the same sign-in).
    pub password: Secret,
    /// The login kind reused from an old agent (`windows`/`sql`), for `arrival-enroll`.
    pub auth: Option<String>,
    /// The SQL login name reused from an old agent, for `arrival-enroll`.
    pub user: Option<String>,
}

/// What this machine saved, else what the old Check-In agent used.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Saved {
    /// `windows`, `sql` or empty.
    pub auth: String,
    /// SQL Server.
    pub server: String,
    /// SQL login name.
    pub user: String,
    /// Databases, comma separated.
    pub databases: String,
    /// Set when this came from the old agent: where from.
    pub from: String,
    /// The old agent's password (only then).
    pub password: Option<String>,
}

/// Parses the old agent's ADO connection into a saved answer; `None` when it is not one.
pub fn legacy_saved(conn: &str, from: &str) -> Option<Saved> {
    let c: dl_tds::Config = conn.parse().ok()?;
    let mut server = c.host().to_string();
    if let Some(i) = c.instance().filter(|i| !i.is_empty()) {
        server = format!("{server}\\{i}");
    } else if let Some(p) = c.port() {
        server = format!("{server},{p}");
    }
    let windows = c.integrated_auth();
    Some(Saved {
        auth: if windows { "windows" } else { "sql" }.into(),
        server,
        user: if windows {
            String::new()
        } else {
            c.user().to_string()
        },
        databases: String::new(),
        from: if from.is_empty() {
            "The old Check-In agent".into()
        } else {
            from.into()
        },
        password: (!windows && !c.password().is_empty()).then(|| c.password().to_string()),
    })
}

fn saved(dir: &Path) -> Saved {
    match crate::sql::setup::saved(dir) {
        Ok(Some(s)) => {
            return Saved {
                auth: s.auth.as_str().into(),
                server: s.server,
                user: s.user,
                databases: s.databases.join(","),
                ..Saved::default()
            };
        }
        Ok(None) => {}
        Err(e) => eprintln!("Warning: the saved SQL login is not readable: {e}"),
    }
    let Some(conn) = crate::cli_install::env_var(LEGACY_ENV) else {
        return Saved::default();
    };
    let from = crate::cli_install::env_var(LEGACY_FROM_ENV).unwrap_or_default();
    legacy_saved(&conn, &from).unwrap_or_else(|| {
        eprintln!("Warning: the old Check-In agent's SQL connection was not understood");
        Saved::default()
    })
}

/// The local SQL Server: `.` for the default instance, `.\NAME` for a named one.
pub fn local_server(services: &[String]) -> Option<String> {
    if services
        .iter()
        .any(|s| s.eq_ignore_ascii_case("MSSQLSERVER"))
    {
        return Some(".".into());
    }
    services.iter().find_map(|s| {
        s.get(..6)
            .filter(|p| p.eq_ignore_ascii_case("MSSQL$"))
            .map(|_| format!(".\\{}", &s[6..]))
    })
}

pub(crate) fn local_sql_server() -> Option<String> {
    let names: Vec<String> = dl_winsvc::scm::Manager::connect_read_only()
        .and_then(|m| m.list_services())
        .map(|l| l.into_iter().map(|e| e.name).collect())
        .unwrap_or_default();
    local_server(&names)
}

/// Prints `label [default]: ` and reads a visible answer; Enter keeps `default`.
pub fn ask(label: &str, default: &str) -> String {
    eprint!("{label} [{default}]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(_) if !line.trim().is_empty() => line.trim().to_string(),
        Ok(_) => default.to_string(),
        Err(e) => {
            eprintln!("cannot read the answer: {e}");
            default.to_string()
        }
    }
}

/// Reads a password typed at the console with echo off (Windows: `SetConsoleMode` through
/// dl-winos). Nothing typed is shown; elsewhere, or when input is not a console, it refuses.
pub fn read_secret(label: &str) -> Result<Secret, Failure> {
    let refuse = |why: String| {
        Failure::new(
            why,
            format!("set {PASSWORD_ENV} for the run instead (it is never saved in clear)"),
            crate::cli::EXIT_CONFIG,
        )
    };
    if !std::io::stdin().is_terminal() {
        return Err(refuse("no console to type the password at".into()));
    }
    read_hidden(label).map_err(refuse)
}

#[cfg(windows)]
fn read_hidden(label: &str) -> Result<Secret, String> {
    let echo = dl_winos::console::hide_input()
        .map_err(|e| format!("cannot hide the password while it is typed: {e}"))?;
    eprint!("{label} (typing is hidden): ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    let restored = echo.restore();
    eprintln!();
    let secret = Secret::new(line.trim_end_matches(['\r', '\n']).to_string());
    drop(Secret::new(line));
    if let Err(e) = restored {
        eprintln!("Warning: console echo not restored: {e}");
    }
    read.map_err(|e| format!("cannot read the password: {e}"))?;
    Ok(secret)
}

#[cfg(not(windows))]
fn read_hidden(_label: &str) -> Result<Secret, String> {
    Err("a hidden password prompt is only available on Windows".into())
}

/// Runs the SQL step; `exe` is the installed binary (its `sql-setup --as-service` tests as
/// LocalSystem through a scheduled task that runs `exe`). With no answer given, the logins
/// this machine already uses (`sources`, see [`crate::cli_install_sql_auto`]) are tried first.
pub fn run(
    exe: &Path,
    dir: &Path,
    o: &SqlOpts,
    interactive: bool,
    enrolled: bool,
    sources: &[crate::legacy_sql::Source],
) -> Result<SqlDone, Failure> {
    if o.skip {
        return Ok(SqlDone::default());
    }
    if let Some(name) = &o.name {
        return add_one(exe, dir, name).map(|()| SqlDone::default());
    }
    if let Some(auth) = &o.auth {
        let done = unattended(exe, dir, o, auth, interactive)?;
        if done.failed && !enrolled {
            return Err(Failure::new(
                "the SQL login test failed (see above); nothing was saved or enrolled",
                "fix the login, server or password and run again",
                crate::cli_install::EXIT_FAILED,
            ));
        }
        if done.failed {
            eprintln!(
                "Warning: the SQL login test failed (see above): the previous SQL settings are kept"
            );
        }
        return Ok(done);
    }
    if o.prompt {
        menu(exe, dir)?;
        return Ok(SqlDone::default());
    }
    if let Some(done) = crate::cli_install_sql_auto::reuse(exe, dir, sources, interactive)? {
        if done.failed {
            eprintln!(
                "Warning: no SQL login found on this machine works (see above); the agent is installed without a new SQL login and reports it to the hub"
            );
        }
        return Ok(done);
    }
    if interactive && dir.join("sql.json").exists() {
        menu(exe, dir)?;
    }
    Ok(SqlDone::default())
}

/// A command for the installed binary that inherits none of the installer's secrets.
pub fn agent(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    for k in CHILD_SECRETS {
        c.env_remove(k);
    }
    c
}

fn status_of(mut c: Command, what: &str) -> Result<i32, Failure> {
    c.status()
        .map(|s| s.code().unwrap_or(crate::cli_install::EXIT_FAILED))
        .map_err(|e| {
            Failure::new(
                format!("cannot run {what}: {e}"),
                "the antivirus may block the agent; allow it and run again",
                crate::cli_install::EXIT_FAILED,
            )
        })
}

fn add_one(exe: &Path, dir: &Path, name: &str) -> Result<(), Failure> {
    let Some(conn) = crate::cli_install::env_var(CONNECTION_ENV) else {
        return Err(Failure::new(
            format!("--sql-name {name} needs the connection in {CONNECTION_ENV}"),
            format!("set {CONNECTION_ENV} for the run"),
            crate::cli::EXIT_CONFIG,
        ));
    };
    let data = dir.display().to_string();
    let mut c = agent(exe);
    c.args(["sql-add", name, "--data", &data])
        .env(CONNECTION_ENV, conn);
    let mut test = agent(exe);
    test.args(["sql-test", name, "--data", &data]);
    if status_of(c, "sql-add")? != 0 || status_of(test, "sql-test")? != 0 {
        return Err(Failure::new(
            format!("SQL {name} did not connect; nothing was enrolled"),
            "check the connection string and run again",
            crate::cli_install::EXIT_FAILED,
        ));
    }
    Ok(())
}

/// `sql-setup` for the login, plus `extra` arguments; the password only in the child's
/// environment.
pub(crate) fn sql_setup(
    exe: &Path,
    dir: &Path,
    auth: &str,
    server: &str,
    user: &str,
    password: &Secret,
    extra: &[&str],
) -> Result<bool, Failure> {
    let data = dir.display().to_string();
    let result = dir.join("sql-setup.last.json").display().to_string();
    let mut c = agent(exe);
    c.args([
        "sql-setup",
        "--auth",
        auth,
        "--server",
        server,
        "--data",
        &data,
        "--result",
        &result,
    ]);
    if auth == "sql" {
        c.args(["--user", user]);
    } else {
        c.arg("--as-service");
    }
    c.args(extra);
    if !password.is_empty() {
        c.env(PASSWORD_ENV, password.expose());
    }
    Ok(status_of(c, "sql-setup")? == 0)
}

fn unattended(
    exe: &Path,
    dir: &Path,
    o: &SqlOpts,
    auth: &str,
    interactive: bool,
) -> Result<SqlDone, Failure> {
    AuthMode::parse(auth).map_err(|e| {
        Failure::new(
            e.to_string(),
            "use --sql-auth windows or sql",
            crate::cli::EXIT_CONFIG,
        )
    })?;
    let s = saved(dir);
    let server = o
        .server
        .clone()
        .or_else(|| (!s.server.is_empty()).then(|| s.server.clone()))
        .or_else(local_sql_server)
        .unwrap_or_else(|| ".".into());
    let user = o.user.clone().unwrap_or_else(|| s.user.clone());
    let mut password = Secret::new(crate::cli_install::env_var(PASSWORD_ENV).unwrap_or_default());
    if auth == "sql" && password.is_empty() {
        if let Some(p) = s.password.as_ref().filter(|_| user == s.user) {
            println!("No {PASSWORD_ENV}: using the old Check-In agent's password for this login.");
            password = Secret::new(p.clone());
        } else if interactive {
            password = read_secret(&format!("Password of SQL login {user}"))?;
        }
    }
    let ok = sql_setup(exe, dir, auth, &server, &user, &password, &[])?;
    if ok {
        println!("SQL login {auth} tested and saved.");
    } else {
        match std::fs::read_to_string(dir.join("sql-setup.last.json")) {
            Ok(t) => println!("SQL_SETUP_RESULT {}", t.trim()),
            Err(e) => eprintln!("Warning: no SQL test result to show: {e}"),
        }
    }
    Ok(SqlDone {
        failed: !ok,
        password,
        ..SqlDone::default()
    })
}

/// The interactive SQL login menu: `[1] Windows  [2] SQL login  [0] skip`, Enter keeps the
/// previous answer; it asks again until the test passes or the operator skips.
pub(crate) fn menu(exe: &Path, dir: &Path) -> Result<(), Failure> {
    let s = saved(dir);
    let mut mode = if s.auth == "sql" { "2" } else { "1" }.to_string();
    let mut server = if s.server.is_empty() {
        local_sql_server().unwrap_or_else(|| ".".into())
    } else {
        s.server.clone()
    };
    let mut user = if s.user.is_empty() {
        "sa".to_string()
    } else {
        s.user.clone()
    };
    if !s.from.is_empty() {
        println!(
            "{}: {} login on {}. Enter keeps it.",
            s.from, s.auth, s.server
        );
    } else if !s.auth.is_empty() {
        println!(
            "Saved SQL login: {} on {} ({}). Enter keeps it.",
            s.auth, s.server, s.databases
        );
    }
    loop {
        mode = ask("SQL login: [1] Windows  [2] SQL login  [0] skip", &mode);
        match mode.as_str() {
            "0" => {
                println!("SQL login not changed.");
                return Ok(());
            }
            "1" | "2" => {}
            _ => {
                eprintln!("Type 1, 2 or 0.");
                mode = "1".into();
                continue;
            }
        }
        server = ask(
            "SQL Server (., .\\SQLEXPRESS, host\\instance or host,port)",
            &server,
        );
        let (auth, password) = if mode == "2" {
            user = ask("SQL login name", &user);
            let keeps = s.auth == "sql" && user == s.user;
            let label = if keeps {
                "Password (Enter keeps the saved one)"
            } else {
                "Password"
            };
            let mut pw = read_secret(label)?;
            if pw.is_empty()
                && user == s.user
                && let Some(p) = &s.password
            {
                pw = Secret::new(p.clone());
            }
            ("sql", pw)
        } else {
            ("windows", Secret::default())
        };
        let ok = sql_setup(exe, dir, auth, &server, &user, &password, &[])?;
        drop(password);
        if ok {
            println!("SQL login tested and saved.");
            return Ok(());
        }
        eprintln!(
            "The SQL login test failed (exact error above). Choose again; Enter keeps the previous answer."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_old_agents_sql_login_becomes_the_saved_answer() {
        let s = legacy_saved(
            "Data Source=srv\\SMILE;User ID=smile;Password=p@ss;Initial Catalog=SMILE_FO",
            "Old agent",
        )
        .unwrap();
        assert_eq!(s.auth, "sql");
        assert_eq!(s.server, "srv\\SMILE");
        assert_eq!(s.user, "smile");
        assert_eq!(s.password.as_deref(), Some("p@ss"));
        assert_eq!(s.from, "Old agent");
        let w = legacy_saved("Server=db,1444;Integrated Security=SSPI", "").unwrap();
        assert_eq!(
            (w.auth.as_str(), w.server.as_str(), w.password),
            ("windows", "db,1444", None)
        );
        assert!(w.from.contains("Check-In"));
    }

    #[test]
    fn the_local_sql_server_prefers_the_default_instance() {
        let names = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            local_server(&names(&["Spooler", "MSSQL$SQLEXPRESS", "MSSQLSERVER"])).as_deref(),
            Some(".")
        );
        assert_eq!(
            local_server(&names(&["mssql$smile"])).as_deref(),
            Some(".\\smile")
        );
        assert_eq!(local_server(&names(&["SQLWriter"])), None);
    }

    #[test]
    fn a_secret_never_prints_and_children_lose_the_installers_secrets() {
        assert_eq!(format!("{:?}", Secret::new("hunter2".into())), "Secret(..)");
        let c = agent(Path::new("dl-agent.exe"));
        let removed: Vec<_> = c
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for k in CHILD_SECRETS {
            assert!(removed.contains(&k.to_string()), "{k}");
        }
    }

    #[test]
    fn unattended_sql_refuses_an_unknown_auth_mode() {
        let d = crate::store::testdir::TestDir::new("install-sql-auth");
        let o = SqlOpts {
            auth: Some("kerberos".into()),
            ..SqlOpts::default()
        };
        let e = run(Path::new("missing.exe"), &d.0, &o, false, true, &[]).unwrap_err();
        assert_eq!(e.code, crate::cli::EXIT_CONFIG);
    }
}
