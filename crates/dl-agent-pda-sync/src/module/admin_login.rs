//! Where `pda-enroll` / `pda-grants` take the SQL administrator from when no one is at the
//! machine (remote attach, PROTOCOL.md §4.2, RT-0261).
//!
//! The service runs as LocalSystem, which is often not a SQL Server administrator, so the
//! default Windows login fails with error 15247. Two ways around it, both without a console:
//!
//! - `--sql-admin-user <login>` with the password in [`ADMIN_PASSWORD_ENV`] (the hub's
//!   attach command may only set `DL_AGENT_*` variables) or the classic
//!   `PDA_SQL_ADMIN_PASSWORD`;
//! - `--sql-admin-saved`: the SQL login dl-agent already keeps in `<data>/sql.json` (sealed
//!   password), for the POS database when it is declared, else the first SQL login there.
//!   Its server becomes `--sql-instance` unless one is given.

use std::path::Path;

use dl_agent::Error;
use dl_agent::sql::SQL_FILE;
use dl_agent::sql::secret::{AuthMode, resolve};
use dl_json::Value;

use crate::bootstrap::admin::PASSWORD_ENV_VAR;
use crate::hosting::secret_prompt::{ConsoleSecretPrompt, SecretPrompt};

/// The `DL_AGENT_*` name of the administrator password, the one remote attach can set.
pub const ADMIN_PASSWORD_ENV: &str = "DL_AGENT_PDA_SQL_ADMIN_PASSWORD";
/// The flag that picks the login saved in `sql.json`.
pub const SQL_ADMIN_SAVED_FLAG: &str = "sql-admin-saved";
/// The POS database `pda-enroll` sets up when `--database` is absent.
pub const DEFAULT_DATABASE: &str = "SMILE_POS";

/// A SQL administrator login found in `sql.json`.
#[derive(Clone, PartialEq, Eq)]
pub struct SavedLogin {
    /// The `sql.json` entry it came from.
    pub database: String,
    /// `host`, `host\instance` or `host,port`, as `--sql-instance` takes it.
    pub data_source: String,
    /// The SQL login.
    pub user: String,
    /// Its password (unsealed).
    pub password: String,
}

impl std::fmt::Debug for SavedLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedLogin")
            .field("database", &self.database)
            .field("data_source", &self.data_source)
            .field("user", &self.user)
            .field("password", &"***")
            .finish()
    }
}

fn data_source(c: &dl_tds::Config) -> String {
    let mut s = c.host().to_string();
    if let Some(i) = c.instance().filter(|i| !i.is_empty()) {
        s.push('\\');
        s.push_str(i);
    }
    if let Some(p) = c.port() {
        s.push(',');
        s.push_str(&p.to_string());
    }
    s
}

fn login_of(name: &str, entry: &Value) -> Result<Option<SavedLogin>, Error> {
    let (full, mode) =
        resolve(entry).map_err(|e| Error::Config(format!("{SQL_FILE}: database {name}: {e}")))?;
    if mode != AuthMode::Sql {
        return Ok(None);
    }
    let c: dl_tds::Config = full.parse().map_err(|e: dl_tds::Error| {
        Error::Config(format!(
            "{SQL_FILE}: database {name}: connection string: {e}"
        ))
    })?;
    if c.user().is_empty() || c.password().is_empty() {
        return Ok(None);
    }
    Ok(Some(SavedLogin {
        database: name.to_string(),
        data_source: data_source(&c),
        user: c.user().to_string(),
        password: c.password().to_string(),
    }))
}

/// The SQL login saved in the `sql.json` document: the entry named `database` when it uses
/// a SQL login, else the first entry that does.
///
/// # Errors
/// [`Error::Config`] when no entry holds a SQL login, or an entry cannot be read.
pub fn saved_login_in(doc: &Value, database: &str) -> Result<SavedLogin, Error> {
    let none = || {
        Error::Config(format!(
            "--{SQL_ADMIN_SAVED_FLAG}: {SQL_FILE} holds no SQL Server login (only Windows logins or no databases); \
             pass --sql-admin-user with {ADMIN_PASSWORD_ENV} instead"
        ))
    };
    let dbs = doc
        .get("databases")
        .and_then(Value::as_object)
        .ok_or_else(none)?;
    if let Some(entry) = dbs
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(database))
        .map(|(_, e)| e)
        && let Some(found) = login_of(database, entry)?
    {
        return Ok(found);
    }
    for (name, entry) in dbs.iter() {
        if name.eq_ignore_ascii_case(database) {
            continue;
        }
        if let Some(found) = login_of(name, entry)? {
            return Ok(found);
        }
    }
    Err(none())
}

/// [`saved_login_in`] over `<data_dir>/sql.json`.
///
/// # Errors
/// As [`saved_login_in`]; I/O errors reading the file.
pub fn saved_login(data_dir: &Path, database: &str) -> Result<SavedLogin, Error> {
    let doc = dl_agent::store::read_json(&data_dir.join(SQL_FILE))?.unwrap_or(Value::Null);
    saved_login_in(&doc, database)
}

/// The administrator password: a fixed one (the saved login), else [`ADMIN_PASSWORD_ENV`],
/// else the classic variable or the console prompt.
pub struct AdminPassword {
    fixed: Option<String>,
    from_env: fn(&str) -> Option<String>,
}

fn real_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl AdminPassword {
    /// The environment, then the console.
    pub fn from_environment() -> AdminPassword {
        AdminPassword {
            fixed: None,
            from_env: real_env,
        }
    }

    /// Always `password` (the login saved in `sql.json`).
    pub fn fixed(password: String) -> AdminPassword {
        AdminPassword {
            fixed: Some(password),
            from_env: real_env,
        }
    }

    #[cfg(test)]
    fn with_env(from_env: fn(&str) -> Option<String>) -> AdminPassword {
        AdminPassword {
            fixed: None,
            from_env,
        }
    }
}

impl SecretPrompt for AdminPassword {
    fn read(&self, env_var: &str, prompt: &str) -> Option<String> {
        if env_var == PASSWORD_ENV_VAR {
            if let Some(p) = &self.fixed {
                return Some(p.clone());
            }
            if let Some(p) = (self.from_env)(ADMIN_PASSWORD_ENV).filter(|p| !p.is_empty()) {
                return Some(p);
            }
        }
        ConsoleSecretPrompt.read(env_var, prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Value {
        dl_json::parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn the_pos_entry_wins_and_its_server_becomes_the_instance() {
        let d = doc(r#"{"databases":{
                "SMILE_FO":{"connection":"Server=SRV\\FO;Database=SMILE_FO;User Id=fo;Password=fo-pass"},
                "SMILE_POS":{"connection":"Server=.\\SQL2019,1444;Database=SMILE_POS;User Id=sa;Password=pos-pass"}}}"#);
        let l = saved_login_in(&d, DEFAULT_DATABASE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(l.database, "SMILE_POS");
        assert_eq!(l.user, "sa");
        assert_eq!(l.password, "pos-pass");
        assert!(l.data_source.ends_with(",1444"), "{}", l.data_source);
        assert!(l.data_source.contains("SQL2019"), "{}", l.data_source);
        assert!(!format!("{l:?}").contains("pos-pass"));
    }

    #[test]
    fn a_windows_pos_entry_falls_back_to_another_sql_login() {
        let d = doc(r#"{"databases":{
                "SMILE_POS":{"connection":"Server=.;Database=SMILE_POS;Integrated Security=SSPI"},
                "SMILE_FO":{"connection":"Server=SRV;Database=SMILE_FO;User Id=fo;Password=fo-pass"}}}"#);
        let l = saved_login_in(&d, "smile_pos").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((l.database.as_str(), l.user.as_str()), ("SMILE_FO", "fo"));
        assert_eq!(l.data_source, "SRV");
    }

    #[test]
    fn only_windows_logins_or_no_file_is_a_clear_error() {
        let d = doc(
            r#"{"databases":{"SMILE_POS":{"connection":"Server=.;Integrated Security=SSPI"}}}"#,
        );
        let e = saved_login_in(&d, DEFAULT_DATABASE)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("no SQL Server login"), "{e}");
        assert!(saved_login_in(&Value::Null, DEFAULT_DATABASE).is_err());
        let dir = std::env::temp_dir().join(format!("pda-admin-login-{}", std::process::id()));
        assert!(saved_login(&dir, DEFAULT_DATABASE).is_err());
    }

    #[test]
    fn the_password_comes_fixed_then_from_the_dl_agent_variable() {
        let p = AdminPassword::fixed("saved".into());
        assert_eq!(p.read(PASSWORD_ENV_VAR, "").as_deref(), Some("saved"));
        let p = AdminPassword::with_env(|k| (k == ADMIN_PASSWORD_ENV).then(|| "from-hub".into()));
        assert_eq!(p.read(PASSWORD_ENV_VAR, "").as_deref(), Some("from-hub"));
        let p = AdminPassword::with_env(|_| Some(String::new()));
        // Empty counts as unset; under cargo test there is no console, so nothing.
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            assert_eq!(
                p.read("PDA_SYNC_UNSET_VAR_FOR_TEST_91", "").as_deref(),
                None
            );
        }
    }
}
