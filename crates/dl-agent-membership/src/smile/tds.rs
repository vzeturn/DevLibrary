//! [`TdsSmile`]: one lazily opened SMILE connection (reopened after a broken session), the
//! .NET connection-string defaults (`SmileConnectionFactory`), the identity probe
//! (`SmileDbIdentityProbe`) and the health ping.
//!
//! The module opens two of these, one for the sync thread (ping, identity) and one for the
//! scan thread, so a slow scan never delays a heartbeat.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use dl_tds::{Client, Config, Encrypt, Param, ResultSet, Row, TlsVersion};

use super::{SmileError, SmileHealth, rows};
use crate::identity::{ForeignState, IdentityProbe, Snapshot};
use crate::wire::DbIdentity;

/// `SmileConnectionFactory.DefaultConnectTimeoutSeconds`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// `SmileConnectionFactory.DefaultCommandTimeoutSeconds`.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// The identity probe's command timeout.
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);

/// SQL errors that make one optional identity field unreadable (`UnreadableFieldErrors`).
const UNREADABLE_FIELD_ERRORS: [i32; 5] = [229, 297, 300, 207, 208];

const CORE_SQL: &str = "\
SELECT COALESCE(CAST(@@SERVERNAME AS nvarchar(256)), CAST(SERVERPROPERTY('ServerName') AS nvarchar(256))) AS Server,
       DB_NAME() AS DatabaseName, d.create_date AS CreateDate, GETDATE() AS PmsNow
FROM sys.databases d WHERE d.database_id = DB_ID()";

const RECOVERY_SQL: &str = "SELECT database_guid AS DatabaseGuid, family_guid AS FamilyGuid FROM sys.database_recovery_status WHERE database_id = DB_ID()";

const BROKER_SQL: &str =
    "SELECT service_broker_guid FROM sys.databases WHERE database_id = DB_ID()";

/// `ForeignSql` with `SmileClerk.App` = `APP` and a 30-day window.
const FOREIGN_SQL: &str = "\
SELECT TOP 1 LastChangeTime FROM Folio WITH (NOLOCK)
WHERE ISNULL(LastChangeClerkID, '') <> 'APP'
  AND LastChangeTime >= DATEADD(day, -30, GETDATE())
ORDER BY LastChangeTime DESC";

/// Where the connection string led, safe to log (no user, no password).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmileTarget {
    /// Host (and `\instance`).
    pub data_source: String,
    /// Database name.
    pub database: String,
}

/// Splits an ADO connection string on `;` outside quotes (both quote styles, as SqlClient).
fn split_parts(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in raw.chars() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (Some(q), _) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, ';') => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    v.to_owned()
}

fn seconds(key: &str, v: &str) -> Result<Duration, SmileError> {
    v.trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .map_err(|_| SmileError::Config(format!("connection string key '{key}' is not a number")))
}

/// Turns a .NET (`Microsoft.Data.SqlClient`) connection string into a dl-tds config with
/// the same defaults the .NET agent applies when a key is not declared: connect timeout
/// 15 s, command timeout 30 s, `Encrypt=Mandatory`, `TrustServerCertificate=true`.
///
/// Keys whose meaning differs between SqlClient and dl-tds are translated: SqlClient's
/// `Connect Timeout` is the connect timeout, its `Command Timeout` the statement timeout,
/// and `Trust Server Certificate` / `yes` / `no` spellings are accepted. When `tlsmin` is
/// not declared, TLS 1.0 is accepted as the minimum, because SqlClient on Windows takes
/// whatever SChannel negotiates and older SMILE servers offer no more.
pub fn normalize_connection_string(raw: &str) -> Result<Config, SmileError> {
    if raw.trim().is_empty() {
        return Err(SmileError::Config("empty SMILE connection string".into()));
    }
    let mut kept = Vec::new();
    let mut connect = None;
    let mut command = None;
    let (mut has_encrypt, mut has_trust, mut has_tls_min, mut has_app) =
        (false, false, false, false);
    for part in split_parts(raw) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let key: String = k
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase();
        let value = unquote(v);
        match key.as_str() {
            "connecttimeout" | "connectiontimeout" | "timeout" => {
                connect = Some(seconds(&key, &value)?);
            }
            "commandtimeout" => command = Some(seconds(&key, &value)?),
            "trustservercertificate" => {
                has_trust = true;
                let b = match value.to_ascii_lowercase().as_str() {
                    "yes" | "true" => "true",
                    "no" | "false" => "false",
                    other => {
                        return Err(SmileError::Config(format!(
                            "invalid TrustServerCertificate value ({} chars)",
                            other.len()
                        )));
                    }
                };
                kept.push(format!("TrustServerCertificate={b}"));
            }
            "encrypt" => {
                has_encrypt = true;
                kept.push(format!("Encrypt={value}"));
            }
            "tlsmin" => {
                has_tls_min = true;
                kept.push(format!("tlsmin={value}"));
            }
            "applicationintent" => kept.push(format!("ApplicationIntent={value}")),
            "applicationname" | "appname" => {
                has_app = true;
                kept.push(format!("App Name={value}"));
            }
            "hostnameincertificate" => kept.push(format!("HostNameInCertificate={value}")),
            "workstationid" => kept.push(format!("Workstation ID={value}")),
            "packetsize" => kept.push(format!("Packet Size={value}")),
            _ => {
                let quoted = if value.contains(';') || value.contains('"') {
                    format!("\"{}\"", value.replace('"', "\"\""))
                } else {
                    value
                };
                kept.push(format!("{}={quoted}", k.trim()));
            }
        }
    }
    let mut cfg: Config = kept.join(";").parse().map_err(|e: dl_tds::Error| match e {
        dl_tds::Error::Config { key, .. } => {
            SmileError::Config(format!("SMILE connection string rejected (key '{key}')"))
        }
        _ => SmileError::Config("SMILE connection string rejected".into()),
    })?;
    if !has_encrypt {
        cfg = cfg.with_encrypt(Encrypt::Required);
    }
    if !has_trust {
        cfg = cfg.with_trust_server_certificate(true);
    }
    if !has_tls_min {
        cfg = cfg.with_tls_min(TlsVersion::Tls10);
    }
    if !has_app {
        cfg = cfg.with_app_name("dl-agent membership");
    }
    Ok(cfg
        .with_connect_timeout(connect.unwrap_or(DEFAULT_CONNECT_TIMEOUT))
        .with_timeout(Some(command.unwrap_or(DEFAULT_COMMAND_TIMEOUT))))
}

/// SMILE over dl-tds; implements every source trait of [`crate::smile`].
pub struct TdsSmile {
    cfg: Config,
    command_timeout: Duration,
    client: Mutex<Option<Client>>,
    warned: Mutex<HashSet<&'static str>>,
}

impl std::fmt::Debug for TdsSmile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdsSmile")
            .field("target", &self.target())
            .finish_non_exhaustive()
    }
}

impl TdsSmile {
    /// A SMILE source for `connection_string` (not connected yet).
    pub fn new(connection_string: &str) -> Result<TdsSmile, SmileError> {
        let cfg = normalize_connection_string(connection_string)?;
        Ok(TdsSmile {
            command_timeout: cfg.timeout().unwrap_or(DEFAULT_COMMAND_TIMEOUT),
            cfg,
            client: Mutex::new(None),
            warned: Mutex::new(HashSet::new()),
        })
    }

    /// Host and database, for logs.
    pub fn target(&self) -> SmileTarget {
        let data_source = match self.cfg.instance() {
            Some(i) if !i.is_empty() => format!("{}\\{i}", self.cfg.host()),
            _ => self.cfg.host().to_owned(),
        };
        SmileTarget {
            data_source,
            database: self.cfg.database().to_owned(),
        }
    }

    /// Runs `f` on the connection, opening it first when needed and dropping it when the
    /// session broke. `timeout` overrides the command timeout for this call.
    pub(crate) fn with<T>(
        &self,
        timeout: Option<Duration>,
        f: impl FnOnce(&mut Client) -> Result<T, dl_tds::Error>,
    ) -> Result<T, SmileError> {
        let mut slot = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(Client::is_broken) {
            *slot = None;
        }
        if slot.is_none() {
            *slot = Some(Client::connect(&self.cfg).map_err(|e| SmileError::from_tds(&e))?);
        }
        let Some(client) = slot.as_mut() else {
            return Err(SmileError::Unavailable("connection not open".into()));
        };
        let wanted = timeout.unwrap_or(self.command_timeout);
        if let Err(e) = client.set_timeout(Some(wanted)) {
            *slot = None;
            return Err(SmileError::from_tds(&e));
        }
        let out = f(client);
        // A session whose timeout cannot be restored is dropped and reopened next time.
        let restore_failed =
            timeout.is_some() && client.set_timeout(Some(self.command_timeout)).is_err();
        if restore_failed || client.is_broken() {
            *slot = None;
        }
        out.map_err(|e| SmileError::from_tds(&e))
    }

    /// The rows of the first result set.
    pub(crate) fn rows(&self, sql: &str, params: &[Param]) -> Result<Vec<Row>, SmileError> {
        self.with(None, |c| {
            Ok(c.query(sql, params)?
                .into_sets()
                .into_iter()
                .next()
                .map(ResultSet::into_rows)
                .unwrap_or_default())
        })
    }

    /// Every result set.
    pub(crate) fn sets(&self, sql: &str, params: &[Param]) -> Result<Vec<Vec<Row>>, SmileError> {
        self.with(None, |c| {
            Ok(c.query(sql, params)?
                .into_sets()
                .into_iter()
                .map(ResultSet::into_rows)
                .collect())
        })
    }

    fn warn_once(&self, field: &'static str, number: Option<i32>) {
        let mut warned = self
            .warned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if warned.insert(field) {
            crate::log::warn(
                "SMILE identity field not readable with this login; reported without it",
                &[
                    ("field", crate::log::V::S(field)),
                    (
                        "sql_error",
                        crate::log::V::I(i64::from(number.unwrap_or(0))),
                    ),
                ],
            );
        }
    }

    /// Runs an optional identity read: the errors of a login that may not read the field
    /// make it `None`; any other error fails the probe.
    fn optional<T>(
        &self,
        field: &'static str,
        read: impl FnOnce(&TdsSmile) -> Result<Option<T>, SmileError>,
    ) -> Result<Option<T>, SmileError> {
        match read(self) {
            Ok(v) => Ok(v),
            Err(SmileError::Server(n)) if UNREADABLE_FIELD_ERRORS.contains(&n) => {
                self.warn_once(field, Some(n));
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    fn identity_rows(&self, sql: &str) -> Result<Vec<Row>, SmileError> {
        self.with(Some(IDENTITY_TIMEOUT), |c| {
            Ok(c.query(sql, &[])?
                .into_sets()
                .into_iter()
                .next()
                .map(ResultSet::into_rows)
                .unwrap_or_default())
        })
    }
}

impl SmileHealth for TdsSmile {
    fn ping(&self) -> Result<(), SmileError> {
        self.with(Some(IDENTITY_TIMEOUT), Client::ping)
    }
}

impl IdentityProbe for TdsSmile {
    fn read_identity(&self) -> Result<Snapshot, SmileError> {
        let core = self.identity_rows(CORE_SQL)?;
        let core = core
            .first()
            .ok_or_else(|| SmileError::Invalid("sys.databases has no row for DB_ID()".into()))?;
        let server = rows::text(core, "Server")?;
        let database = rows::text(core, "DatabaseName")?;
        if server.trim().is_empty() || database.trim().is_empty() {
            return Err(SmileError::Invalid(
                "SMILE server or database name not readable".into(),
            ));
        }
        let create_date = rows::datetime(core, "CreateDate")?;
        let pms_now = rows::datetime(core, "PmsNow")?;
        let recovery = self.optional("database_guid", |s| {
            let r = s.identity_rows(RECOVERY_SQL)?;
            match r.first() {
                Some(row) => Ok(Some((
                    rows::opt_uuid(row, "DatabaseGuid")?,
                    rows::opt_uuid(row, "FamilyGuid")?,
                ))),
                None => Ok(None),
            }
        })?;
        let (database_guid, family_guid) = recovery.unwrap_or((None, None));
        if database_guid.is_none() {
            self.warn_once("database_guid", None);
        }
        let broker = self.optional("service_broker_guid", |s| {
            let r = s.identity_rows(BROKER_SQL)?;
            r.first()
                .map_or(Ok(None), |row| rows::opt_uuid(row, "service_broker_guid"))
        })?;
        let foreign = self.optional("foreign_activity", |s| {
            let r = s.identity_rows(FOREIGN_SQL)?;
            match r.first() {
                Some(row) => Ok(Some(rows::opt_datetime(row, "LastChangeTime")?)),
                None => Ok(Some(None)),
            }
        })?;
        let (last_foreign_change, foreign_state) = match foreign {
            None => (None, ForeignState::Unreadable),
            Some(None) => (None, ForeignState::NoneInWindow),
            Some(Some(at)) => (Some(at), ForeignState::Known),
        };
        Ok(Snapshot {
            identity: DbIdentity {
                server: server.trim().to_owned(),
                database: database.trim().to_owned(),
                create_date,
                database_guid,
                family_guid,
                service_broker_guid: broker,
            },
            pms_now,
            last_foreign_change,
            foreign_state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotnet_defaults_apply_only_when_not_declared() {
        let c = normalize_connection_string(
            "Data Source=srv\\SMILE;Initial Catalog=SMILE_FO;User ID=u;Password='p;w''d';MultipleActiveResultSets=True",
        )
        .unwrap();
        assert_eq!(c.host(), "srv");
        assert_eq!(c.instance(), Some("SMILE"));
        assert_eq!(c.database(), "SMILE_FO");
        assert_eq!(c.password(), "p;w'd");
        assert_eq!(c.encrypt(), Encrypt::Required);
        assert!(c.trust_server_certificate());
        assert_eq!(c.tls_min(), TlsVersion::Tls10);
        assert_eq!(c.connect_timeout(), DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(c.timeout(), Some(DEFAULT_COMMAND_TIMEOUT));

        let c = normalize_connection_string(
            "Server=tcp:h,1444;Database=d;User Id=u;Password=p;Encrypt=False;Trust Server Certificate=no;Connect Timeout=5;Command Timeout=60",
        )
        .unwrap();
        assert_eq!(c.port(), Some(1444));
        assert_eq!(c.encrypt(), Encrypt::Off);
        assert!(!c.trust_server_certificate());
        assert_eq!(c.connect_timeout(), Duration::from_secs(5));
        assert_eq!(c.timeout(), Some(Duration::from_secs(60)));
    }

    #[test]
    fn errors_never_echo_the_connection_string() {
        let e = normalize_connection_string("Server=h;Password=secret;Encrypt=bogus").unwrap_err();
        assert!(!e.to_string().contains("secret") && !e.to_string().contains("bogus"));
        assert!(normalize_connection_string("  ").is_err());
        let e =
            normalize_connection_string("Server=h;Password=secret;Connect Timeout=x").unwrap_err();
        assert!(!e.to_string().contains("secret"));
    }
}
