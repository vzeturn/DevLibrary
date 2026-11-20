//! The automatic SQL login step of `install` (PROTOCOL.md §9.8.1): gather the logins the
//! older agents on this machine used ([`crate::sql::legacy`]), the old Check-In agent's
//! connection handed over by the installer script, and the Windows service login when this
//! machine runs SQL Server; drop duplicates; test each in order on every SMILE database with
//! `sql-setup` and keep the first that works. The operator is asked only when none works;
//! an unattended run then fails with a clear error and a problem report for the hub.

use std::path::Path;

use dl_json::{Value, json};

use crate::cli_install::Failure;
use crate::cli_install_sql::{Saved, Secret, legacy_saved};
use crate::report::{ProblemReport, Severity, Spool};
use crate::sql::legacy::Scan;

/// One login to try.
#[derive(Debug)]
pub struct Candidate {
    /// `windows` or `sql`.
    pub auth: String,
    /// SQL Server as `sql-setup --server` takes it.
    pub server: String,
    /// SQL login name (`sql`).
    pub user: String,
    /// SQL login password (`sql`); never printed.
    pub password: Secret,
    /// Where it came from, for people.
    pub from: String,
    /// Where it came from, for the hub (`sql.json` `source`): product, location, file. No secret.
    pub source: Value,
}

impl Candidate {
    fn key(&self) -> (String, String, String, String) {
        (
            self.auth.clone(),
            normal_server(&self.server),
            self.user.to_ascii_lowercase(),
            self.password.expose().to_string(),
        )
    }

    /// `sql login smile on srv\SMILE (the old Check-In agent, in place)`.
    pub fn describe(&self) -> String {
        match self.auth.as_str() {
            "sql" => format!("SQL login {} on {} ({})", self.user, self.server, self.from),
            _ => format!(
                "Windows login (the service account) on {} ({})",
                self.server, self.from
            ),
        }
    }
}

/// `.`, `(local)`, `localhost`, `127.0.0.1` and this computer's name are one server.
fn normal_server(s: &str) -> String {
    let s = s.trim().trim_start_matches("tcp:").to_ascii_lowercase();
    let (host, rest) = match s.find(['\\', ',']) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    let me = std::env::var("COMPUTERNAME")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let local = ["", ".", "(local)", "localhost", "127.0.0.1", "::1"].contains(&host)
        || (!me.is_empty() && host == me);
    format!("{}{rest}", if local { "." } else { host })
}

fn candidate(s: Saved, source: Value) -> Candidate {
    Candidate {
        auth: s.auth,
        server: s.server,
        user: s.user,
        password: Secret::new(s.password.unwrap_or_default()),
        from: s.from,
        source,
    }
}

/// The candidates in the order they are tried: older agents (in place first, then what a bench
/// moved aside), the installer script's hand-over, then the Windows service login on
/// `local_server`. Duplicates (same mode, server, login and password) are tried once.
pub fn candidates(
    scan: &Scan,
    script: Option<(&str, &str)>,
    local_server: Option<&str>,
) -> Vec<Candidate> {
    let mut all = Vec::new();
    for f in &scan.found {
        let from = format!("{}, {}: {}", f.label, f.location.name(), f.file.display());
        match legacy_saved(&f.connection, &from) {
            Some(s) => all.push(candidate(
                s,
                json!({"kind": "legacy_agent", "product": f.product,
                       "location": f.location.name(), "file": f.file.display().to_string()}),
            )),
            None => eprintln!(
                "Note: {} has a SQL connection that is not understood; skipped",
                f.file.display()
            ),
        }
    }
    if let Some((conn, from)) = script
        && let Some(s) = legacy_saved(conn, from)
    {
        let file = from.to_string();
        all.push(candidate(
            s,
            json!({"kind": "installer_script", "product": "checkin", "file": file}),
        ));
    }
    if let Some(server) = local_server {
        all.push(Candidate {
            auth: "windows".into(),
            server: server.to_string(),
            user: String::new(),
            password: Secret::default(),
            from: "this machine's SQL Server".into(),
            source: json!({"kind": "service_account"}),
        });
    }
    let mut out: Vec<Candidate> = Vec::new();
    for c in all {
        if c.server.trim().is_empty() || (c.auth == "sql" && c.user.trim().is_empty()) {
            continue;
        }
        if !out.iter().any(|o| o.key() == c.key()) {
            out.push(c);
        }
    }
    out
}

/// How one candidate is tested, and how the operator is asked (fakes in tests).
pub trait Trial {
    /// Tests `c` on every SMILE database and, when it works, saves it; `Err(message)` is the
    /// first SQL error.
    fn test(&mut self, c: &Candidate) -> Result<(), String>;
    /// The interactive menu.
    fn ask(&mut self) -> Result<(), Failure>;
}

/// How the automatic step ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Chosen {
    /// This candidate (index) works and was saved.
    Candidate(usize),
    /// None worked; the operator answered the menu.
    Asked,
    /// None worked and nobody can be asked: reported.
    Failed,
}

/// Tries `cands` in order; `interactive` decides what happens when none works.
pub fn choose(
    cands: &[Candidate],
    interactive: bool,
    trial: &mut dyn Trial,
    dir: &Path,
) -> Result<Chosen, Failure> {
    let mut tried = Vec::new();
    for (i, c) in cands.iter().enumerate() {
        println!("Trying {}", c.describe());
        match trial.test(c) {
            Ok(()) => {
                println!("SQL login chosen: {}", c.describe());
                return Ok(Chosen::Candidate(i));
            }
            Err(e) => {
                println!("  did not work: {e}");
                tried.push(json!({"auth": c.auth, "server": c.server, "login": c.user,
                                  "source": c.source.clone(), "error": e}));
            }
        }
    }
    if interactive {
        println!("None of the SQL logins found on this machine works; choose one.");
        trial.ask()?;
        return Ok(Chosen::Asked);
    }
    let message = format!(
        "no SQL login found on this machine works ({} tried) and nobody can be asked",
        cands.len()
    );
    eprintln!("ERROR: {message}");
    let r = ProblemReport::new(
        Severity::Error,
        "agent",
        "sql_auto_failed",
        &message,
        crate::now(),
    )
    .with("tried", Value::Array(tried));
    if let Err(e) = Spool::open(dir.join("reports.json")).record(r) {
        eprintln!("Warning: cannot spool the report for the hub: {e}");
    }
    Ok(Chosen::Failed)
}

#[cfg(test)]
#[path = "cli_install_sql_auto_tests.rs"]
mod tests;
