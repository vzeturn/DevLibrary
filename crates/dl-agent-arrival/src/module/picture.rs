//! The `picture_probe` command and the self-test row for the PICTURE phase: whether the
//! `SMILE_PICTURE` database exists, what the service's SQL login may do there and in the
//! PMS, whether the bundle and its picture phase are installed, and what is still needed.
//!
//! What the phase needs (proved by `tests/sql_live.rs` against a restricted login):
//! - install, once: `SMILE_PICTURE` must exist (creating it needs `CREATE ANY DATABASE`, i.e.
//!   `dbcreator`), and the login needs a user there that may create and alter tables in
//!   `dbo` (`db_ddladmin`, or `db_owner` as the bench grants); without a user the batches
//!   fail with SQL error 916;
//! - every day: the signature and picture procedures of the PMS read and write
//!   `SMILE_PICTURE.dbo.*` across databases, where ownership chaining is off, so the login
//!   also needs `db_datareader` + `db_datawriter` there (`db_owner` covers both).
//!
//! Read only: nothing is created or granted.

use dl_json::{Map, Value};
use dl_tds::Client;

use crate::agent::installer::{self, PICTURE_PROPERTY_NAME, PROPERTY_NAME};
use crate::agent::tds;

/// The picture database.
pub const PICTURE_DB: &str = "SMILE_PICTURE";
/// The tables the picture phase creates.
pub const PICTURE_TABLES: [&str; 2] = ["FontDeskSignture", "ProfilePicture"];

/// What the probe read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `SUSER_SNAME()`.
    pub login: String,
    /// `DB_NAME()` of the PMS connection.
    pub pms_database: String,
    /// `SERVERPROPERTY('ProductVersion')`.
    pub server_version: String,
    /// Member of `sysadmin`.
    pub sysadmin: bool,
    /// May create databases.
    pub create_database: bool,
    /// May create procedures in the PMS database.
    pub pms_create_procedure: bool,
    /// May set the bundle marker (database `ALTER`).
    pub pms_alter: bool,
    /// `SMILE_PICTURE` exists.
    pub picture_exists: bool,
    /// The login can enter `SMILE_PICTURE`.
    pub picture_access: bool,
    /// May create tables in `SMILE_PICTURE.dbo`.
    pub picture_ddl: bool,
    /// May read `SMILE_PICTURE.dbo`.
    pub picture_read: bool,
    /// May write `SMILE_PICTURE.dbo`.
    pub picture_write: bool,
    /// The picture tables found.
    pub picture_tables: Vec<String>,
    /// The bundle marker (`None`: never installed).
    pub bundle_installed: Option<String>,
    /// The picture-phase marker.
    pub picture_installed: Option<String>,
}

/// What the facts mean: the phase's state and what is still needed (operator lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    /// `installed`, `pending` (the next install runs it and can), or `blocked`.
    pub state: &'static str,
    /// What to do, in order.
    pub needs: Vec<String>,
}

/// The phase state from the facts.
pub fn assess(f: &Facts, picture_hash: &str) -> Assessment {
    let mut needs = Vec::new();
    let login = if f.login.is_empty() {
        "the service login"
    } else {
        f.login.as_str()
    };
    let user_sql = format!(
        "USE [{PICTURE_DB}]; CREATE USER [{login}] FOR LOGIN [{login}]; EXEC sp_addrolemember N'db_owner', N'{login}';"
    );
    let installed = f.picture_installed.as_deref() == Some(picture_hash)
        && f.picture_tables.len() == PICTURE_TABLES.len();
    if !f.picture_exists {
        if !f.create_database {
            needs.push(format!(
                "{PICTURE_DB} does not exist and {login} may not create it: as an administrator run CREATE DATABASE [{PICTURE_DB}] then {user_sql} (the bench does both with sql_grant_service)"
            ));
        } else if !f.sysadmin {
            needs.push(format!(
                "{PICTURE_DB} will be created by the install; then {login} needs a user there: {user_sql}"
            ));
        }
    } else if !f.picture_access {
        needs.push(format!(
            "{login} has no user in {PICTURE_DB} (the install fails with SQL error 916): as an administrator run {user_sql}"
        ));
    } else {
        if !installed && !f.picture_ddl {
            needs.push(format!(
                "{login} may not create tables in {PICTURE_DB}: USE [{PICTURE_DB}]; EXEC sp_addrolemember N'db_ddladmin', N'{login}'; (or db_owner)"
            ));
        }
        if !(f.picture_read && f.picture_write) {
            needs.push(format!(
                "{login} may not read and write {PICTURE_DB}.dbo (signatures and ID pictures fail): USE [{PICTURE_DB}]; EXEC sp_addrolemember N'db_datareader', N'{login}'; EXEC sp_addrolemember N'db_datawriter', N'{login}';"
            ));
        }
    }
    if f.bundle_installed.is_none() && !(f.pms_create_procedure && f.pms_alter) {
        needs.push(format!(
            "{login} may not install the procedure bundle in {}: make it db_owner of that database",
            f.pms_database
        ));
    }
    let state = if installed && needs.is_empty() {
        "installed"
    } else if needs.is_empty() {
        "pending"
    } else {
        "blocked"
    };
    Assessment { state, needs }
}

fn flag(v: Option<&dl_tds::Value>) -> bool {
    match v {
        Some(dl_tds::Value::I32(n)) => *n == 1,
        Some(dl_tds::Value::I64(n)) => *n == 1,
        Some(dl_tds::Value::Bool(b)) => *b,
        Some(dl_tds::Value::I8(n)) => *n == 1,
        Some(dl_tds::Value::I16(n)) => *n == 1,
        _ => false,
    }
}

fn text(v: Option<&dl_tds::Value>) -> String {
    match v {
        Some(dl_tds::Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn sql(e: &dl_tds::Error) -> String {
    match e.number() {
        Some(n) => format!("probe query failed (SQL error {n})"),
        None => format!("probe failed: {}", tds::exec_error(e)),
    }
}

fn read_facts(pms: &mut Client, connection: &str) -> Result<Facts, String> {
    let r = pms.query(
        "SELECT SUSER_SNAME(), DB_NAME(), CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(32)),
  IS_SRVROLEMEMBER('sysadmin'),
  HAS_PERMS_BY_NAME(NULL, NULL, 'CREATE ANY DATABASE'),
  HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE PROCEDURE'),
  HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'ALTER'),
  CASE WHEN DB_ID(@db) IS NULL THEN 0 ELSE 1 END,
  CASE WHEN DB_ID(@db) IS NULL THEN 0 ELSE HAS_DBACCESS(@db) END",
        &[dl_tds::Param::new("@db", PICTURE_DB)],
    ).map_err(|e| sql(&e))?;
    let row = r
        .rows()
        .first()
        .map(|r| r.values().to_vec())
        .unwrap_or_default();
    let mut f = Facts {
        login: text(row.first()),
        pms_database: text(row.get(1)),
        server_version: text(row.get(2)),
        sysadmin: flag(row.get(3)),
        create_database: flag(row.get(4)),
        pms_create_procedure: flag(row.get(5)),
        pms_alter: flag(row.get(6)),
        picture_exists: flag(row.get(7)),
        picture_access: flag(row.get(8)),
        ..Facts::default()
    };
    f.bundle_installed = installer::read_marker(pms, PROPERTY_NAME).map_err(|e| e.0)?;
    f.picture_installed = installer::read_marker(pms, PICTURE_PROPERTY_NAME).map_err(|e| e.0)?;
    if f.picture_access {
        let mut pic = tds::connect(connection, Some(PICTURE_DB)).map_err(|e| sql(&e))?;
        let r = pic.query(
            "SELECT HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'CREATE TABLE') & HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'ALTER'),
  HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'SELECT'),
  HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'INSERT') & HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'UPDATE') & HAS_PERMS_BY_NAME('dbo', 'SCHEMA', 'DELETE')",
            &[],
        ).map_err(|e| sql(&e))?;
        let row = r
            .rows()
            .first()
            .map(|r| r.values().to_vec())
            .unwrap_or_default();
        f.picture_ddl = flag(row.first());
        f.picture_read = flag(row.get(1));
        f.picture_write = flag(row.get(2));
        let t = pic.query(
            "SELECT name FROM sys.tables WHERE schema_id = SCHEMA_ID('dbo') AND name IN ('FontDeskSignture', 'ProfilePicture') ORDER BY name",
            &[],
        ).map_err(|e| sql(&e))?;
        f.picture_tables = t.rows().iter().map(|r| text(r.values().first())).collect();
    }
    Ok(f)
}

/// The facts and the assessment as the command answers them.
pub fn to_json(f: &Facts, a: &Assessment) -> Value {
    let b = installer::bundle();
    let mut m = Map::new();
    m.insert("state", a.state);
    m.insert("login", f.login.as_str());
    m.insert("pms_database", f.pms_database.as_str());
    m.insert("server_version", f.server_version.as_str());
    m.insert("sysadmin", f.sysadmin);
    m.insert("create_database", f.create_database);
    m.insert("pms_create_procedure", f.pms_create_procedure);
    m.insert("pms_alter", f.pms_alter);
    let mut pic = Map::new();
    pic.insert("exists", f.picture_exists);
    pic.insert("access", f.picture_access);
    pic.insert("create_tables", f.picture_ddl);
    pic.insert("read", f.picture_read);
    pic.insert("write", f.picture_write);
    pic.insert(
        "tables",
        Value::Array(
            f.picture_tables
                .iter()
                .map(|t| Value::from(t.as_str()))
                .collect(),
        ),
    );
    m.insert(PICTURE_DB, Value::Object(pic));
    let mut bundle = Map::new();
    bundle.insert("ours", b.hash.as_str());
    bundle.insert(
        "installed",
        f.bundle_installed.clone().map_or(Value::Null, Value::from),
    );
    bundle.insert("picture_ours", b.picture_hash.as_str());
    bundle.insert(
        "picture_installed",
        f.picture_installed.clone().map_or(Value::Null, Value::from),
    );
    m.insert("bundle", Value::Object(bundle));
    m.insert(
        "needs",
        Value::Array(a.needs.iter().map(|n| Value::from(n.as_str())).collect()),
    );
    Value::Object(m)
}

/// Runs the probe on `connection`.
///
/// # Errors
/// The PMS cannot be reached or a query failed (no connection string in the message).
pub fn probe(connection: &str) -> Result<(Facts, Assessment), String> {
    let mut pms = tds::connect(connection, None)
        .map_err(|e| format!("cannot connect: {}", tds::exec_error(&e)))?;
    let f = read_facts(&mut pms, connection)?;
    let a = assess(&f, &installer::bundle().picture_hash);
    Ok((f, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Facts {
        Facts {
            login: "NT AUTHORITY\\SYSTEM".into(),
            pms_database: "SMILE_FO".into(),
            picture_exists: true,
            picture_access: true,
            picture_ddl: true,
            picture_read: true,
            picture_write: true,
            pms_create_procedure: true,
            pms_alter: true,
            picture_tables: PICTURE_TABLES.iter().map(|s| s.to_string()).collect(),
            bundle_installed: Some("b".into()),
            picture_installed: Some("p".into()),
            ..Facts::default()
        }
    }

    #[test]
    fn an_installed_phase_with_rights_needs_nothing() {
        let a = assess(&ready(), "p");
        assert_eq!(a.state, "installed");
        assert!(a.needs.is_empty());
        let a = assess(&ready(), "newer");
        assert_eq!(a.state, "pending");
    }

    #[test]
    fn a_login_without_a_user_in_smile_picture_gets_the_exact_grant() {
        let f = Facts {
            picture_access: false,
            ..ready()
        };
        let a = assess(&f, "p");
        assert_eq!(a.state, "blocked");
        assert!(a.needs[0].contains("916"), "{:?}", a.needs);
        assert!(a.needs[0].contains("CREATE USER [NT AUTHORITY\\SYSTEM]"));
    }

    #[test]
    fn a_missing_database_and_missing_rights_are_named() {
        let f = Facts {
            picture_exists: false,
            picture_access: false,
            ..ready()
        };
        assert!(assess(&f, "p").needs[0].contains("CREATE DATABASE [SMILE_PICTURE]"));
        let f = Facts {
            picture_write: false,
            ..ready()
        };
        assert!(assess(&f, "p").needs[0].contains("db_datawriter"));
        let f = Facts {
            picture_ddl: false,
            picture_installed: None,
            ..ready()
        };
        assert!(assess(&f, "p").needs[0].contains("db_ddladmin"));
        let f = Facts {
            bundle_installed: None,
            pms_alter: false,
            ..ready()
        };
        assert!(assess(&f, "p").needs[0].contains("db_owner of that database"));
    }
}
