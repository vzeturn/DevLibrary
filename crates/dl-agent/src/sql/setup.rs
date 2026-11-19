//! `sql-setup`: choose the SQL login mode at install time, test it against every SMILE
//! database on the server, and only then write `sql.json` (PROTOCOL.md §9.8).
//!
//! Windows mode tests as the service account (LocalSystem → `NT AUTHORITY\SYSTEM` on the
//! local server, `DOMAIN\HOST$` on another machine); when that login is missing the
//! result carries the exact T-SQL a DBA can run. Nothing is ever granted by the agent.

use std::path::Path;

use dl_json::{Map, Value, json};

use super::secret::{self, AuthMode};
use super::transport::Rung;
use super::{SQL_FILE, TdsConnector};
use crate::Error;

/// The databases a SMILE site may have.
pub const SMILE_DATABASES: [&str; 3] = ["SMILE_FO", "SMILE_POS", "SMILE_BO"];

/// SQL errors that mean "this login cannot get in": login failed (18456), untrusted
/// domain (18452), cannot open database (4060, or 4063 when the server would fall back to the
/// login's default database), principal not a user (916), denied (229).
const ACCESS_ERRORS: [i32; 6] = [18456, 18452, 4060, 4063, 916, 229];

/// What the operator chose.
#[derive(Clone)]
pub struct Request {
    /// `.`, `.\SQLEXPRESS`, `host\inst`, `host,port`.
    pub server: String,
    /// Login mode.
    pub auth: AuthMode,
    /// SQL login name (SQL mode).
    pub user: String,
    /// SQL login password (SQL mode); never printed.
    pub password: String,
    /// Databases to set up; empty = every SMILE database present on the server.
    pub databases: Vec<String>,
    /// `--allow-unencrypted`: a SQL login to another computer may end on the unencrypted
    /// rung (an old server without TLS 1.2). Off, such a server fails as
    /// `sql_tls_unavailable`.
    pub allow_unencrypted: bool,
}

impl core::fmt::Debug for Request {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Request")
            .field("server", &self.server)
            .field("auth", &self.auth)
            .field("user", &self.user)
            .field("databases", &self.databases)
            .field("allow_unencrypted", &self.allow_unencrypted)
            .finish_non_exhaustive()
    }
}

/// A failed SQL call: the server's message and error number when it sent one.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// Exact error text.
    pub message: String,
    /// SQL Server error number.
    pub number: Option<i32>,
}

impl From<dl_tds::Error> for Failure {
    fn from(e: dl_tds::Error) -> Failure {
        let number = match &e {
            dl_tds::Error::Server { number, .. } => Some(*number),
            _ => None,
        };
        Failure {
            message: e.to_string(),
            number,
        }
    }
}

/// The SQL calls setup makes (a fake in tests). `clear` lets the transport ladder end
/// unencrypted for a SQL login to another computer ([`Request::allow_unencrypted`]).
pub trait Sql {
    /// The databases of `wanted` that exist on the server `connection` points at.
    fn present(
        &self,
        connection: &str,
        wanted: &[&str],
        clear: bool,
    ) -> Result<Vec<String>, Failure>;
    /// Logs in with `connection`, runs `SELECT 1` and answers the transport rung that worked
    /// (saved as `transport` in `sql.json`).
    fn test(&self, connection: &str, clear: bool) -> Result<Rung, Failure>;
}

/// The real server, through the agent's transport ladder.
pub struct Live;

impl Sql for Live {
    fn present(
        &self,
        connection: &str,
        wanted: &[&str],
        clear: bool,
    ) -> Result<Vec<String>, Failure> {
        let (mut c, _) = open(connection, clear)?;
        let list = wanted
            .iter()
            .map(|w| format!("N'{}'", w.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        let r = c.query(
            &format!("SELECT name FROM sys.databases WHERE name IN ({list}) ORDER BY name"),
            &[],
        )?;
        Ok(r.rows()
            .iter()
            .filter_map(|row| row.get::<String>(0).ok())
            .collect())
    }

    fn test(&self, connection: &str, clear: bool) -> Result<Rung, Failure> {
        let (mut c, rung) = open(connection, clear)?;
        c.query("SELECT 1", &[])?;
        Ok(rung)
    }
}

/// A session through the ladder and the rung it settled on; a guarded ladder's handshake
/// failure is `sql_tls_unavailable` ([`TdsConnector::tls_refusal`]).
fn open(connection: &str, clear: bool) -> Result<(dl_tds::Client, Rung), Failure> {
    let mut c = TdsConnector::parse(connection).map_err(|e| Failure {
        message: e.to_string(),
        number: None,
    })?;
    if clear {
        c = c.allow_unencrypted();
    }
    match c.open() {
        Ok(client) => Ok((client, c.rung().unwrap_or(Rung::AsConfigured))),
        Err(e) => Err(match c.tls_refusal(&e) {
            Some(message) => Failure {
                message,
                number: None,
            },
            None => e.into(),
        }),
    }
}

/// One database's test.
#[derive(Clone, Debug, PartialEq)]
pub struct DbResult {
    /// Database name.
    pub name: String,
    /// `None` when the login worked.
    pub error: Option<Failure>,
    /// The transport rung that worked (`None` on failure).
    pub transport: Option<Rung>,
}

/// The result of a setup run, printed as JSON and kept for the installer.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// Every database answered.
    pub ok: bool,
    /// Mode tested.
    pub auth: AuthMode,
    /// Server as given.
    pub server: String,
    /// The login SQL Server sees.
    pub login: String,
    /// Per database.
    pub databases: Vec<DbResult>,
    /// Why the server could not be listed (discovery).
    pub error: Option<Failure>,
    /// T-SQL for a DBA when a Windows login lacks access.
    pub hint: Option<String>,
}

impl Outcome {
    /// Wire form (no password).
    pub fn to_json(&self) -> Value {
        let fail = |f: &Option<Failure>| match f {
            None => Value::Null,
            Some(f) => json!({"message": f.message, "number": f.number.map(i64::from)}),
        };
        let dbs: Vec<Value> = self
            .databases
            .iter()
            .map(|d| {
                json!({"name": d.name, "ok": d.error.is_none(), "error": fail(&d.error),
                       "transport": d.transport.map(Rung::name)})
            })
            .collect();
        json!({"ok": self.ok, "auth": self.auth.as_str(), "server": self.server, "login": self.login,
               "databases": dbs, "error": fail(&self.error), "hint": self.hint})
    }

    /// Reads [`Outcome::to_json`] back (the result a LocalSystem child run wrote).
    ///
    /// # Errors
    /// [`Error::Config`] for a document that is not an outcome.
    pub fn from_json(v: &Value) -> Result<Outcome, Error> {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let fail = |v: Option<&Value>| match v {
            Some(f) if f.is_object() => Some(Failure {
                message: s(f, "message"),
                number: f
                    .get("number")
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok()),
            }),
            _ => None,
        };
        let databases = v
            .get("databases")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|d| DbResult {
                        name: s(d, "name"),
                        error: fail(d.get("error")),
                        transport: d
                            .get("transport")
                            .and_then(Value::as_str)
                            .and_then(|t| Rung::parse(t).ok().flatten()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Outcome {
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            auth: AuthMode::parse(&s(v, "auth"))?,
            server: s(v, "server"),
            login: s(v, "login"),
            databases,
            error: fail(v.get("error")),
            hint: v.get("hint").and_then(Value::as_str).map(str::to_string),
        })
    }
}

/// An ADO connection string for `database` with the chosen login.
pub fn connection(req: &Request, database: &str) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let login = match req.auth {
        AuthMode::Windows => "Integrated Security=SSPI".to_string(),
        AuthMode::Sql => format!("User Id={};Password={}", q(&req.user), q(&req.password)),
    };
    format!(
        "Server={};Database={};{login};TrustServerCertificate=true",
        q(&req.server),
        q(database)
    )
}

/// The login SQL Server sees for this request (Windows mode: the LocalSystem service).
pub fn login(req: &Request) -> String {
    match req.auth {
        AuthMode::Sql => req.user.clone(),
        AuthMode::Windows => service_login(
            &req.server,
            &host_env("COMPUTERNAME"),
            &host_env("USERDOMAIN"),
        ),
    }
}

fn host_env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// LocalSystem's login on `server`: itself on this machine, the computer account elsewhere.
pub fn service_login(server: &str, computer: &str, domain: &str) -> String {
    let host = server
        .split(['\\', ','])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches("tcp:");
    let local = ["", ".", "(local)", "localhost", "127.0.0.1", "::1"]
        .iter()
        .any(|l| host.eq_ignore_ascii_case(l))
        || (!computer.is_empty() && host.eq_ignore_ascii_case(computer));
    if local || computer.is_empty() {
        "NT AUTHORITY\\SYSTEM".into()
    } else if domain.is_empty() || domain.eq_ignore_ascii_case(computer) {
        format!("{computer}$")
    } else {
        format!("{domain}\\{computer}$")
    }
}

/// T-SQL a DBA runs to let `login` read and write `databases` (SQL 2008 compatible).
pub fn grant_script(login: &str, databases: &[String]) -> String {
    let n = login.replace('\'', "''");
    let b = login.replace(']', "]]");
    let mut s = format!(
        "-- Run as a sysadmin. Lets the dl-agent service ({login}) use the SMILE databases.\n\
         IF SUSER_ID(N'{n}') IS NULL CREATE LOGIN [{b}] FROM WINDOWS;\n"
    );
    for db in databases {
        let d = db.replace(']', "]]");
        s.push_str(&format!(
            "USE [{d}];\n\
             IF USER_ID(N'{n}') IS NULL CREATE USER [{b}] FOR LOGIN [{b}];\n\
             EXEC sp_addrolemember N'db_datareader', N'{n}';\n\
             EXEC sp_addrolemember N'db_datawriter', N'{n}';\n\
             GRANT EXECUTE TO [{b}];\n\
             -- EXEC sp_addrolemember N'db_ddladmin', N'{n}';  -- only if the hub installs SQL bundles here\n"
        ));
    }
    s
}

/// Lists (when asked) and tests every database; never writes anything.
pub fn run(req: &Request, sql: &dyn Sql) -> Outcome {
    let login = login(req);
    let mut out = Outcome {
        ok: false,
        auth: req.auth,
        server: req.server.clone(),
        login: login.clone(),
        databases: Vec::new(),
        error: None,
        hint: None,
    };
    let names = if req.databases.is_empty() {
        match sql.present(
            &connection(req, "master"),
            &SMILE_DATABASES,
            req.allow_unencrypted,
        ) {
            Ok(n) if n.is_empty() => {
                out.error = Some(Failure {
                    message: format!(
                        "no SMILE database (SMILE_FO, SMILE_POS, SMILE_BO) on {}",
                        req.server
                    ),
                    number: None,
                });
                return out;
            }
            Ok(n) => n,
            Err(f) => {
                out.hint = hint(req, &login, &[&f], &SMILE_DATABASES.map(String::from));
                out.error = Some(f);
                return out;
            }
        }
    } else {
        req.databases.clone()
    };
    for name in names {
        let (transport, error) = match sql.test(&connection(req, &name), req.allow_unencrypted) {
            Ok(rung) => (Some(rung), None),
            Err(f) => (None, Some(f)),
        };
        out.databases.push(DbResult {
            name,
            error,
            transport,
        });
    }
    let failed: Vec<&Failure> = out
        .databases
        .iter()
        .filter_map(|d| d.error.as_ref())
        .collect();
    let failed_dbs: Vec<String> = out
        .databases
        .iter()
        .filter(|d| d.error.is_some())
        .map(|d| d.name.clone())
        .collect();
    out.hint = hint(req, &login, &failed, &failed_dbs);
    out.ok = failed.is_empty();
    out
}

fn hint(req: &Request, login: &str, failures: &[&Failure], dbs: &[String]) -> Option<String> {
    let access = failures
        .iter()
        .any(|f| f.number.is_some_and(|n| ACCESS_ERRORS.contains(&n)));
    (req.auth == AuthMode::Windows && access && !dbs.is_empty()).then(|| grant_script(login, dbs))
}

/// Writes the tested databases to `sql.json`: connection without password, `auth`, the
/// sealed password in SQL mode, and the rung that worked as `transport` (a pin: the
/// runtime never steps below what setup proved, so nobody on the path can force a weaker
/// rung later). Other keys of an existing entry (limits) stay.
///
/// # Errors
/// [`Error::Config`] when the outcome is not ok; I/O and DPAPI errors.
pub fn save(data_dir: &Path, req: &Request, outcome: &Outcome) -> Result<(), Error> {
    if !outcome.ok {
        return Err(Error::Config("refusing to save a failed SQL setup".into()));
    }
    let path = data_dir.join(SQL_FILE);
    let mut doc = crate::store::read_json(&path)?.unwrap_or_else(|| json!({"databases": {}}));
    let root = doc
        .as_object_mut()
        .ok_or_else(|| Error::Config(format!("{SQL_FILE} is not an object")))?;
    if !root.contains_key("databases") {
        root.insert("databases", Value::Object(Map::new()));
    }
    let dbs = root
        .get_mut("databases")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| Error::Config(format!("{SQL_FILE}: databases is not an object")))?;
    for db in &outcome.databases {
        let mut req_db = req.clone();
        req_db.password = String::new();
        let mut conn = connection(&req_db, &db.name);
        if req.auth == AuthMode::Sql {
            conn = conn.replace(";Password=\"\"", "");
        }
        let mut entry = dbs
            .get(&db.name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(|| {
                let mut m = Map::new();
                m.insert("max_connections", Value::from(4u64));
                m
            });
        entry.insert("connection", Value::String(conn));
        entry.insert(secret::AUTH_KEY, Value::String(req.auth.as_str().into()));
        entry.remove(secret::PASSWORD_KEY);
        if let Some(rung) = db.transport {
            entry.insert("transport", Value::String(rung.name().into()));
        }
        if req.auth == AuthMode::Sql {
            entry.insert(
                secret::PASSWORD_KEY,
                Value::String(secret::seal(&req.password)?),
            );
        }
        dbs.insert(&db.name, Value::Object(entry));
    }
    super::SqlPools::from_json(&doc)?;
    crate::store::write_json(&path, &doc)?;
    crate::identity::restrict_file(&path)
}

/// The saved login choice: the installer's default on a reinstall. Never the password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    /// Login mode.
    pub auth: AuthMode,
    /// Server as typed at install (`host`, `host\instance` or `host,port`).
    pub server: String,
    /// SQL login name; empty in Windows mode.
    pub user: String,
    /// Database names in `sql.json`.
    pub databases: Vec<String>,
}

/// What is saved now, for the installer's default.
///
/// # Errors
/// I/O, a bad file or a connection string that does not parse.
pub fn saved(data_dir: &Path) -> Result<Option<Saved>, Error> {
    let Some(doc) = crate::store::read_json(&data_dir.join(SQL_FILE))? else {
        return Ok(None);
    };
    let Some(dbs) = doc.get("databases").and_then(Value::as_object) else {
        return Ok(None);
    };
    let mut first = None;
    let mut names = Vec::new();
    for (name, entry) in dbs.iter() {
        names.push(name.to_string());
        if first.is_none() {
            first = Some(entry.clone());
        }
    }
    let Some(entry) = first else { return Ok(None) };
    let conn = entry
        .get("connection")
        .and_then(Value::as_str)
        .unwrap_or("");
    let config: dl_tds::Config = conn
        .parse()
        .map_err(|e: dl_tds::Error| Error::Config(format!("connection string: {e}")))?;
    let mut server = config.host().to_string();
    if let Some(i) = config.instance() {
        server = format!("{server}\\{i}");
    }
    if let Some(p) = config.port() {
        server = format!("{server},{p}");
    }
    let auth = match entry.get(secret::AUTH_KEY).and_then(Value::as_str) {
        Some(a) => AuthMode::parse(a)?,
        None => AuthMode::of(&config),
    };
    Ok(Some(Saved {
        auth,
        server,
        user: config.user().to_string(),
        databases: names,
    }))
}

/// The saved password of `user`, if one is sealed in `sql.json` (reinstall without change).
///
/// # Errors
/// I/O, a bad file or a blob that cannot be unsealed.
pub fn saved_password(data_dir: &Path, user: &str) -> Result<Option<String>, Error> {
    let Some(doc) = crate::store::read_json(&data_dir.join(SQL_FILE))? else {
        return Ok(None);
    };
    for (_, entry) in doc
        .get("databases")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    {
        let (full, mode) = secret::resolve(&entry)?;
        let c: dl_tds::Config = full
            .parse()
            .map_err(|e: dl_tds::Error| Error::Config(format!("connection string: {e}")))?;
        if mode == AuthMode::Sql && c.user().eq_ignore_ascii_case(user) && !c.password().is_empty()
        {
            return Ok(Some(c.password().to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
