//! The installer's SQL step with no answer given (RT-0303): reuse a login that already works
//! instead of asking.
//!
//! Tried in order, each with `sql-setup` on every SMILE database (the first that passes is
//! saved, with where it came from as `login_source`):
//! 1. the login this agent already saved (a reinstall; its sealed password is reused),
//! 2. the old Check-In agent's connection the installer script handed over,
//! 3. every old agent's login the modules found ([`crate::legacy_sql`]), in the app's order,
//! 4. Windows sign-in (LocalSystem) to the local SQL Server.
//!
//! Only when none passes does a person get involved: an interactive run shows the menu, an
//! unattended one carries on without SQL, ends with an error and leaves one problem report
//! that names every login tried and why it failed (never a password).

use std::path::Path;

use dl_json::{Value, json};

use crate::cli_install::Failure;
use crate::cli_install_sql::{
    LEGACY_ENV, LEGACY_FROM_ENV, Secret, SqlDone, local_sql_server, menu, sql_setup,
};
use crate::legacy_sql::{Candidate, Source, from_connection_string, gather};
use crate::report::{ProblemReport, Severity, Spool};
use crate::sql::secret::AuthMode;

/// The login this agent already saved, as the first candidate (its password stays sealed:
/// `sql-setup` reuses it when given none).
fn saved_login(dir: &Path) -> Option<Candidate> {
    let s = match crate::sql::setup::saved(dir) {
        Ok(s) => s?,
        Err(e) => {
            eprintln!("Warning: the saved SQL login is not readable: {e}");
            return None;
        }
    };
    Some(Candidate {
        source: crate::sql::setup::source(dir).unwrap_or_default(),
        auth: s.auth,
        server: s.server,
        user: s.user,
        password: Secret::default(),
    })
}

/// Every login worth trying, best first, without repeats.
pub fn candidates(dir: &Path, sources: &[Source], local: Option<String>) -> Vec<Candidate> {
    let mut all: Vec<Candidate> = saved_login(dir).into_iter().collect();
    if let Some(conn) = crate::cli_install::env_var(LEGACY_ENV) {
        let from = crate::cli_install::env_var(LEGACY_FROM_ENV)
            .unwrap_or_else(|| "the old Check-In agent".into());
        match from_connection_string(&conn, &from) {
            Some(c) => all.push(c),
            None => {
                eprintln!("Warning: the old Check-In agent's SQL connection was not understood")
            }
        }
    }
    for source in sources {
        all.extend(source());
    }
    if let Some(server) = local {
        all.push(Candidate {
            source: "Windows sign-in to the local SQL Server".into(),
            auth: AuthMode::Windows,
            server,
            user: String::new(),
            password: Secret::default(),
        });
    }
    gather(all)
}

/// Why the last `sql-setup` failed, from its result file (first error only).
fn last_error(dir: &Path) -> String {
    let doc = match crate::store::read_json(&dir.join("sql-setup.last.json")) {
        Ok(Some(d)) => d,
        Ok(None) => return "no result written".into(),
        Err(e) => return format!("result unreadable: {e}"),
    };
    let message = |f: &Value| f.get("message").and_then(Value::as_str).map(str::to_string);
    doc.get("error")
        .and_then(message)
        .or_else(|| {
            doc.get("databases")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|d| {
                    let name = d.get("name").and_then(Value::as_str).unwrap_or("?");
                    d.get("error")
                        .and_then(message)
                        .map(|m| format!("{name}: {m}"))
                })
                .next()
        })
        .unwrap_or_else(|| "failed".into())
}

/// Tries every candidate; `Ok(None)` when there is none to try (no old agent, no local SQL
/// Server): the caller does what it did before.
///
/// # Errors
/// The agent binary cannot be run, or the menu cannot read the console.
pub fn reuse(
    exe: &Path,
    dir: &Path,
    sources: &[Source],
    interactive: bool,
) -> Result<Option<SqlDone>, Failure> {
    let found = candidates(dir, sources, local_sql_server());
    if found.is_empty() {
        return Ok(None);
    }
    println!(
        "SQL login: trying the {} login(s) this machine already uses, best first.",
        found.len()
    );
    let mut tried: Vec<Value> = Vec::new();
    for c in found {
        let shown = if c.source.is_empty() {
            format!("{} (saved by this agent)", c.describe())
        } else {
            c.describe()
        };
        println!("Trying {shown}");
        let mut extra = vec!["--no-report"];
        if !c.source.is_empty() {
            extra.extend(["--source", c.source.as_str()]);
        }
        let auth = c.auth.as_str();
        if sql_setup(exe, dir, auth, &c.server, &c.user, &c.password, &extra)? {
            println!(
                "SQL login reused: {shown}; it signs in to every SMILE database and is saved."
            );
            return Ok(Some(SqlDone {
                failed: false,
                auth: Some(auth.to_string()),
                user: (c.auth == AuthMode::Sql).then(|| c.user.clone()),
                password: c.password,
            }));
        }
        let why = last_error(dir);
        println!("  not usable: {why}");
        tried.push(json!({"source": c.source, "auth": auth, "server": c.server,
                          "login": c.user, "error": why}));
    }
    report(dir, &tried);
    if interactive {
        eprintln!(
            "None of the SQL logins found on this machine signs in to every SMILE database; choose one."
        );
        menu(exe, dir)?;
        return Ok(Some(SqlDone::default()));
    }
    Ok(Some(SqlDone {
        failed: true,
        ..SqlDone::default()
    }))
}

/// One problem report for the whole search; sent once the agent is enrolled and running.
fn report(dir: &Path, tried: &[Value]) {
    let r = ProblemReport::new(
        Severity::Error,
        "agent",
        "sql_login_not_found",
        &format!(
            "none of the {} SQL login(s) found on this machine signs in to every SMILE database",
            tried.len()
        ),
        crate::now(),
    )
    .with("tried", Value::Array(tried.to_vec()));
    if let Err(e) = Spool::open(dir.join("reports.json")).record(r) {
        eprintln!("Warning: cannot spool the SQL problem report: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pms() -> Vec<Candidate> {
        vec![
            from_connection_string("Server=.;User ID=smile;Password=a;Database=SMILE_FO", "pms")
                .unwrap(),
            from_connection_string("Server=(local);User ID=SMILE;Password=a", "einvoice").unwrap(),
        ]
    }

    #[test]
    fn the_saved_login_comes_first_and_windows_last() {
        let d = crate::store::testdir::TestDir::new("install-sql-auto");
        std::fs::write(
            d.0.join("sql.json"),
            r#"{"login_source":"the old PMS site agent (x)","databases":{"SMILE_FO":{"connection":"Server=srv;Database=SMILE_FO;User ID=old","auth":"sql"}}}"#,
        )
        .unwrap();
        let got = candidates(&d.0, &[pms], Some(".".into()));
        let shown: Vec<_> = got
            .iter()
            .map(|c| (c.source.as_str(), c.server.as_str()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("the old PMS site agent (x)", "srv"),
                ("pms", "localhost"),
                ("Windows sign-in to the local SQL Server", "."),
            ]
        );
    }

    #[test]
    fn nothing_found_means_nothing_to_try() {
        let d = crate::store::testdir::TestDir::new("install-sql-auto-none");
        assert!(candidates(&d.0, &[], None).is_empty());
        let none = reuse(Path::new("missing.exe"), &d.0, &[], false);
        assert!(matches!(none, Ok(None)) || local_sql_server().is_some());
    }

    #[test]
    fn the_failure_names_the_first_database_error() {
        let d = crate::store::testdir::TestDir::new("install-sql-auto-err");
        std::fs::write(
            d.0.join("sql-setup.last.json"),
            r#"{"ok":false,"error":null,"databases":[{"name":"SMILE_FO","error":null},{"name":"SMILE_POS","error":{"message":"Login failed"}}]}"#,
        )
        .unwrap();
        assert_eq!(last_error(&d.0), "SMILE_POS: Login failed");
        report(&d.0, &[json!({"source": "pms", "error": "x"})]);
        let spooled = std::fs::read_to_string(d.0.join("reports.json")).unwrap();
        assert!(spooled.contains("sql_login_not_found"), "{spooled}");
    }
}
