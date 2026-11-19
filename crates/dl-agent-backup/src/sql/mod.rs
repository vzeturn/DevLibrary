//! SQL Server access: connections on `dl-tds` through dl-agent's transport ladder (old TLS,
//! then login without TLS for SQL Server 2005–2012), the edition probe that decides
//! `COMPRESSION` / `CHECKSUM`, and the few queries the backup needs. [`SqlConn`] is the seam
//! the pipeline, restore and discovery are tested through.

pub mod options;
pub mod quote;

use std::time::Duration;

use dl_agent::sql::transport;
use dl_tds::{CancelToken, Client, Param};

use crate::Error;
pub use options::{BackupOptions, ServerInfo};

/// How long a login may take (Go used 10 s).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one BACKUP or RESTORE may run (Go used 4 h).
pub const LONG_TIMEOUT: Duration = Duration::from_secs(4 * 3600);
/// How long the small probe queries may take.
const SHORT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where and how to log in.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Target {
    /// `host`, `host\instance`, empty for localhost.
    pub host: String,
    /// TCP port; 0 means 1433. With an instance name, 0 or 1433 asks the SQL Browser.
    pub port: u16,
    /// `Windows` for SSPI, anything else for a SQL login.
    pub auth_type: String,
    /// SQL login name (the hub's servers use `sa`).
    pub username: String,
    /// SQL login password (a secret).
    pub password: String,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("auth_type", &self.auth_type)
            .finish_non_exhaustive()
    }
}

impl Target {
    /// Whether this target logs in with Windows authentication.
    pub fn windows_auth(&self) -> bool {
        self.auth_type.eq_ignore_ascii_case("windows")
    }

    /// The host part without the instance name, `localhost` when empty.
    pub fn machine(&self) -> &str {
        let h = self.host.trim();
        let h = h.split('\\').next().unwrap_or(h);
        if h.is_empty() { "localhost" } else { h }
    }

    /// Whether the server runs on this machine (then its paths are the agent's paths).
    pub fn is_local(&self) -> bool {
        let m = self.machine();
        matches!(
            m.to_ascii_lowercase().as_str(),
            "localhost" | "127.0.0.1" | "::1" | "." | "(local)"
        ) || m.eq_ignore_ascii_case(&dl_agent::hub::hostname())
    }

    /// The `dl-tds` configuration, as the Go DSN: an instance name wins over the default
    /// port, Windows authentication uses SSPI.
    pub fn config(&self) -> dl_tds::Config {
        let host = match self.machine() {
            "." | "(local)" => "localhost",
            m => m,
        };
        let instance = self.host.trim().split_once('\\').map(|(_, i)| i.trim());
        let mut cfg = dl_tds::Config::new(host)
            .with_app_name("dl-agent-backup")
            .with_connect_timeout(CONNECT_TIMEOUT)
            .with_timeout(Some(SHORT_TIMEOUT));
        cfg = match instance.filter(|i| !i.is_empty()) {
            Some(i) if self.port == 0 || self.port == 1433 => cfg.with_instance(i),
            _ => cfg.with_port(if self.port == 0 { 1433 } else { self.port }),
        };
        if self.windows_auth() {
            cfg.with_integrated_auth()
        } else {
            cfg.credentials(&self.username, &self.password)
        }
    }
}

/// One file inside a backup (`RESTORE FILELISTONLY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFile {
    /// Logical name.
    pub logical: String,
    /// `D` data, `L` log, `F` full-text catalog, `S` FILESTREAM.
    pub kind: String,
}

/// An open connection, as the pipeline sees it.
pub trait SqlConn: Send {
    /// What the edition probe found.
    fn info(&self) -> &ServerInfo;
    /// The instance's default backup directory (`xp_instance_regread`), if readable.
    fn backup_dir(&mut self) -> Result<Option<String>, Error>;
    /// The database's `recovery_model_desc`; `None` when there is no such database.
    fn recovery_model(&mut self, database: &str) -> Result<Option<String>, Error>;
    /// Runs one long statement (BACKUP / RESTORE) with named string parameters.
    fn run(&mut self, statement: &str, params: &[(String, String)]) -> Result<(), Error>;
    /// The files inside a backup file.
    fn file_list(&mut self, path: &str) -> Result<Vec<BackupFile>, Error>;
    /// The default data and log directories.
    fn data_dirs(&mut self) -> Result<(String, String), Error>;
    /// Every database name on the server.
    fn list_databases(&mut self) -> Result<Vec<String>, Error>;
    /// Arms (or clears) the cancel token for the next statement.
    fn set_cancel(&mut self, token: Option<CancelToken>);
    /// Whether this login may `BACKUP DATABASE` `database`; `None` when there is no such
    /// database. Connections that do not model permissions answer `Some(true)`.
    fn can_backup(&mut self, database: &str) -> Result<Option<bool>, Error> {
        let _ = database;
        Ok(Some(true))
    }
}

/// Opens connections; the real one is [`Tds`].
pub trait Connector: Send + Sync {
    /// Logs in to `target`.
    fn connect(&self, target: &Target) -> Result<Box<dyn SqlConn>, Error>;
}

/// The `dl-tds` connector.
#[derive(Debug, Default, Clone, Copy)]
pub struct Tds;

impl Connector for Tds {
    fn connect(&self, target: &Target) -> Result<Box<dyn SqlConn>, Error> {
        open(target, &target.config())
    }
}

/// Logs in to `target` with `cfg` (its [`Target::config`], possibly adjusted), down the
/// transport ladder, and probes the edition.
pub(crate) fn open(target: &Target, cfg: &dl_tds::Config) -> Result<Box<dyn SqlConn>, Error> {
    let rungs = transport::rungs(cfg, None);
    let (res, i) = transport::walk(&rungs, 0, |rung| Client::connect(&rung.apply(cfg)));
    let mut client = res.map_err(|e| sql_err(&format!("connect to {}", target.host), &e))?;
    let transport = rungs[i].name();
    let info = options::probe(&mut client, transport)?;
    Ok(Box::new(TdsConn { client, info }))
}

/// A `dl-tds` connection with its probe result.
pub struct TdsConn {
    client: Client,
    info: ServerInfo,
}

fn sql_err(what: &str, e: &dl_tds::Error) -> Error {
    match e {
        dl_tds::Error::Cancelled => Error::Cancelled,
        e => Error::Sql(format!("{what}: {e}")),
    }
}

fn params_of(list: &[(String, String)]) -> Vec<Param> {
    list.iter()
        .map(|(n, v)| Param::new(n, v.as_str()))
        .collect()
}

impl SqlConn for TdsConn {
    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn backup_dir(&mut self) -> Result<Option<String>, Error> {
        let sql = "DECLARE @dir NVARCHAR(512); \
                   EXEC master.dbo.xp_instance_regread N'HKEY_LOCAL_MACHINE', \
                   N'SOFTWARE\\Microsoft\\MSSQLServer\\MSSQLServer', N'BackupDirectory', @dir OUTPUT; \
                   SELECT @dir AS dir;";
        let r = self
            .client
            .query(sql, &[])
            .map_err(|e| sql_err("read the backup directory", &e))?;
        let dir = r
            .rows()
            .last()
            .and_then(|row| row.try_get::<Option<String>>("dir").flatten())
            .filter(|d| !d.trim().is_empty());
        Ok(dir)
    }

    fn recovery_model(&mut self, database: &str) -> Result<Option<String>, Error> {
        let r = self
            .client
            .query(
                "SELECT recovery_model_desc FROM sys.databases WHERE name = @name",
                &[Param::new("name", database)],
            )
            .map_err(|e| sql_err("read the recovery model", &e))?;
        Ok(r.rows().first().and_then(|row| row.try_get::<String>(0)))
    }

    fn run(&mut self, statement: &str, params: &[(String, String)]) -> Result<(), Error> {
        self.client
            .set_timeout(Some(LONG_TIMEOUT))
            .map_err(|e| sql_err("set the statement timeout", &e))?;
        let res = self.client.query(statement, &params_of(params));
        let reset = self.client.set_timeout(Some(SHORT_TIMEOUT));
        res.map_err(|e| sql_err("statement failed", &e))?;
        reset.map_err(|e| sql_err("reset the statement timeout", &e))
    }

    fn file_list(&mut self, path: &str) -> Result<Vec<BackupFile>, Error> {
        let r = self
            .client
            .query(
                "RESTORE FILELISTONLY FROM DISK = @path",
                &[Param::new("path", path)],
            )
            .map_err(|e| sql_err("read the backup's file list", &e))?;
        let mut out = Vec::new();
        for row in r.rows() {
            let logical: String = row
                .get("LogicalName")
                .map_err(|e| sql_err("read the backup's file list", &e))?;
            let kind: String = row
                .get("Type")
                .map_err(|e| sql_err("read the backup's file list", &e))?;
            out.push(BackupFile { logical, kind });
        }
        Ok(out)
    }

    fn data_dirs(&mut self) -> Result<(String, String), Error> {
        let sql = "SELECT CAST(SERVERPROPERTY('InstanceDefaultDataPath') AS nvarchar(512)) AS d, \
                   CAST(SERVERPROPERTY('InstanceDefaultLogPath') AS nvarchar(512)) AS l, \
                   (SELECT physical_name FROM sys.master_files WHERE database_id = 1 AND file_id = 1) AS m";
        let r = self
            .client
            .query(sql, &[])
            .map_err(|e| sql_err("read the data directories", &e))?;
        let row = r
            .rows()
            .first()
            .ok_or_else(|| Error::Sql("no data directory row".into()))?;
        let text = |k: &str| {
            row.try_get::<Option<String>>(k)
                .flatten()
                .map(|s| s.trim().trim_end_matches(['\\', '/']).to_owned())
                .filter(|s| !s.is_empty())
        };
        let master = text("m")
            .map(|m| quote::parent(&m).to_owned())
            .unwrap_or_default();
        let data = text("d").unwrap_or_else(|| master.clone());
        let log = text("l").unwrap_or_else(|| data.clone());
        if data.is_empty() {
            return Err(Error::Sql("cannot find the server's data directory".into()));
        }
        Ok((data, log))
    }

    fn list_databases(&mut self) -> Result<Vec<String>, Error> {
        let r = self
            .client
            .query("SELECT name FROM sys.databases ORDER BY name", &[])
            .map_err(|e| sql_err("list databases", &e))?;
        r.rows()
            .iter()
            .map(|row| {
                row.get::<String>(0)
                    .map_err(|e| sql_err("list databases", &e))
            })
            .collect()
    }

    fn set_cancel(&mut self, token: Option<CancelToken>) {
        self.client.set_cancel(token);
    }

    fn can_backup(&mut self, database: &str) -> Result<Option<bool>, Error> {
        let r = self
            .client
            .query(
                "SELECT HAS_PERMS_BY_NAME(@name, 'DATABASE', 'BACKUP DATABASE') AS can",
                &[Param::new("name", database)],
            )
            .map_err(|e| sql_err("read the BACKUP permission", &e))?;
        Ok(r.rows()
            .first()
            .and_then(|row| row.try_get::<Option<i32>>(0))
            .flatten()
            .map(|v| v == 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(host: &str, port: u16, auth: &str) -> Target {
        Target {
            host: host.into(),
            port,
            auth_type: auth.into(),
            username: "sa".into(),
            password: "pw".into(),
        }
    }

    #[test]
    fn instance_name_wins_over_the_default_port() {
        // Go TestDSNWithInstanceName: host\instance, the port ignored.
        let c = t(r"SRV\SQLEXPRESS", 1433, "SQL").config();
        assert_eq!(c.host(), "SRV");
        assert_eq!(c.instance(), Some("SQLEXPRESS"));
        let c = t(r"SRV\SQLEXPRESS", 14330, "SQL").config();
        assert_eq!(c.port(), Some(14330));
        let c = t("", 0, "SQL").config();
        assert_eq!((c.host(), c.port()), ("localhost", Some(1433)));
    }

    #[test]
    fn windows_auth_uses_sspi_and_hides_the_password() {
        // Go TestDSNWindowsAuth.
        assert!(t("srv", 1433, "Windows").config().integrated_auth());
        assert!(!t("srv", 1433, "SQL").config().integrated_auth());
        let shown = format!("{:?}", t("srv", 1433, "SQL"));
        assert!(!shown.contains("pw"), "{shown}");
    }

    #[test]
    fn local_targets() {
        assert!(t(r".\SQLEXPRESS", 0, "").is_local());
        assert!(t("localhost", 0, "").is_local());
        assert!(!t("db.example.com", 0, "").is_local());
    }
}
