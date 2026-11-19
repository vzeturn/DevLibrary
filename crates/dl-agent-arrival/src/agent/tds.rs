//! `SqlPmsExecutor` over `dl-tds`: stored procedures by catalog name with typed parameters,
//! through a `dl-agent` session pool (which also walks the TLS ladder for old servers when the
//! connection string leaves encryption at login-only).

use std::time::Duration;

use dl_agent::sql::{Pool, PoolOptions, TdsConnector};
use dl_tds::{Param, ResultSet};

use crate::agent::executor::{self, ExecError, Executor, Sets};
use crate::agent::sqlvalue;
use crate::contracts::catalog::catalog;
use crate::contracts::errors::COMMAND_TIMEOUT;
use crate::contracts::projection::Projection;
use crate::contracts::{Operation, Params, validator};
use crate::error::Error;

const FINGERPRINT_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_HOLDERS_TIMEOUT: Duration = Duration::from_secs(5);

const LOCK_HOLDERS_SQL: &str = "SELECT TOP 3 s.session_id, s.program_name, s.host_name,
       DATEDIFF(SECOND, t.transaction_begin_time, GETDATE())
FROM sys.dm_tran_session_transactions st
JOIN sys.dm_tran_active_transactions t ON t.transaction_id = st.transaction_id
JOIN sys.dm_exec_sessions s ON s.session_id = st.session_id
WHERE st.session_id <> @@SPID AND s.database_id = DB_ID()
ORDER BY t.transaction_begin_time";

/// Sizing and timeouts of the executor.
#[derive(Debug, Clone, Copy)]
pub struct TdsOptions {
    /// Sessions the pool may open.
    pub max_connections: usize,
    /// `CommandTimeoutSeconds`.
    pub command_timeout: Duration,
    /// `LockTimeoutSeconds` (at least one second).
    pub lock_timeout: Duration,
}

/// The executor of a running agent.
pub struct TdsExecutor {
    pool: Pool<TdsConnector>,
    opts: TdsOptions,
}

impl std::fmt::Debug for TdsExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TdsExecutor")
            .field("opts", &self.opts)
            .finish_non_exhaustive()
    }
}

/// The connection string as `Microsoft.Data.SqlClient` 4+ reads it: an ADO string without an
/// `Encrypt` key gets `Encrypt=true` (its default, certificate checked). `Encrypt=False`
/// encrypts the login only and SqlClient never checks the certificate then, so it gets
/// `TrustServerCertificate=true` unless the string sets that key (dl-tds checks it otherwise,
/// and a SQL Server 2022 self-signed certificate fails: RT-0164 test PC). Everything else is
/// left as written.
pub fn with_sqlclient_defaults(connection: &str) -> String {
    let trimmed = connection.trim();
    if trimmed.contains("://") {
        return trimmed.to_string();
    }
    let add = match key_value(trimmed, "encrypt") {
        None => "Encrypt=true",
        Some(v) if is_login_only(&v) && !has_key(trimmed, "trustservercertificate") => {
            "TrustServerCertificate=true"
        }
        Some(_) => return trimmed.to_string(),
    };
    let sep = if trimmed.is_empty() || trimmed.ends_with(';') {
        ""
    } else {
        ";"
    };
    format!("{trimmed}{sep}{add}")
}

/// SqlClient's `Encrypt=False` / `Optional` spellings.
fn is_login_only(value: &str) -> bool {
    ["false", "no", "0", "optional"]
        .iter()
        .any(|v| value.eq_ignore_ascii_case(v))
}

/// The trimmed value of the last `key` in an ADO connection string (case-insensitive key).
fn key_value(connection: &str, key: &str) -> Option<String> {
    connection
        .split(';')
        .filter_map(|part| part.split_once('='))
        .rfind(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        .map(|(_, v)| v.trim().to_string())
}

/// Opens one session outside the pool (installer, probe), walking the same TLS ladder;
/// `database` replaces the connection string's.
///
/// # Errors
/// The `dl-tds` error (a connection string that does not parse is [`dl_tds::Error::Config`],
/// whose message never echoes the string).
pub fn connect(connection: &str, database: Option<&str>) -> Result<dl_tds::Client, dl_tds::Error> {
    let mut config: dl_tds::Config = with_sqlclient_defaults(connection).parse()?;
    if config.app_name() == "dl-tds" {
        config = config.with_app_name("dl-agent");
    }
    if let Some(db) = database {
        config = config.with_database(db);
    }
    let rungs = dl_agent::sql::transport::rungs(&config, None);
    dl_agent::sql::transport::walk(&rungs, 0, |rung| {
        dl_tds::Client::connect(&rung.apply(&config))
    })
    .0
}

/// What `PmsConnectionProbe` reads from the PMS server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// `SERVERPROPERTY('ProductVersion')`.
    pub version: String,
    /// `DB_NAME()`.
    pub database: String,
}

impl Probe {
    /// The line `test-pms` prints: `OK SQL Server 16.0.1000.6 (2022), database SMILE_FO`.
    pub fn line(&self) -> String {
        format!(
            "OK SQL Server {} ({}), database {}",
            self.version,
            executor::engine_name(&self.version),
            self.database
        )
    }
}

/// `PmsConnectionProbe.RunAsync`: opens `connection` the way the agent will and reads the
/// server version and database.
///
/// # Errors
/// The `dl-tds` error; [`dl_tds::Error::is_tls`] tells a failed TLS handshake apart.
pub fn probe(connection: &str) -> Result<Probe, dl_tds::Error> {
    let mut client = connect(connection, None)?;
    let results = client.query(
        "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(32)), DB_NAME()",
        &[],
    )?;
    let row = results.rows().first();
    let col = |i: usize| row.map(|r| text(r, i).to_string()).unwrap_or_default();
    Ok(Probe {
        version: col(0),
        database: col(1),
    })
}

/// `true` when the ADO connection string sets `key` (case-insensitive).
pub fn has_key(connection: &str, key: &str) -> bool {
    connection
        .split(';')
        .filter_map(|part| part.split_once('='))
        .any(|(k, _)| k.trim().eq_ignore_ascii_case(key))
}

impl TdsExecutor {
    /// An executor for `connection` (never echoed in errors).
    ///
    /// # Errors
    /// [`Error::Config`] when the connection string does not parse.
    pub fn new(connection: &str, opts: TdsOptions) -> Result<TdsExecutor, Error> {
        let connector = TdsConnector::parse(&with_sqlclient_defaults(connection))
            .map_err(|e| Error::Config(format!("PmsConnectionString: {e}")))?
            .with_lock_timeout(Some(opts.lock_timeout));
        let pool = Pool::new(
            "arrival",
            connector,
            PoolOptions {
                max_connections: opts.max_connections.max(1),
                acquire_timeout: Duration::from_secs(15),
                query_timeout: Some(opts.command_timeout),
                ..PoolOptions::default()
            },
        );
        Ok(TdsExecutor { pool, opts })
    }

    fn session(&self) -> Result<dl_agent::sql::Pooled<'_, TdsConnector>, ExecError> {
        self.pool.get().map_err(|e| {
            let code = dl_agent::sql::problem_code(&e, "sql_unavailable");
            dl_log::warn!("cannot open a PMS session"; error = %e);
            ExecError::Unavailable(format!("cannot open a session ({code})"))
        })
    }

    fn params(op: &Operation, params: &Params) -> Result<Vec<Param>, ExecError> {
        let mut out = Vec::with_capacity(op.parameters.len());
        for p in &op.parameters {
            // Absent = the procedure default applies; present with null = explicit SQL NULL.
            let Some(raw) = params.get(&p.name) else {
                continue;
            };
            let value = match raw {
                None => sqlvalue::null_of(p.ty),
                Some(text) => validator::convert_value(p, text)
                    .and_then(sqlvalue::to_param)
                    .ok_or_else(|| {
                        ExecError::Other(format!("parameter {} does not convert", p.name))
                    })?,
            };
            out.push(Param::new(&p.name, value));
        }
        Ok(out)
    }

    fn read_sets(
        op: &Operation,
        projection: Option<&Projection>,
        sets: Vec<ResultSet>,
    ) -> Result<Sets, ExecError> {
        let mut out: Sets = Vec::new();
        for set in sets {
            // A write procedure that selects nothing leaves a set without columns.
            if set.columns().is_empty() {
                continue;
            }
            let index = out.len();
            // Result sets the catalog does not describe are dropped: unknown data never
            // leaves the hotel.
            if index >= op.result_sets.len() {
                break;
            }
            let columns = executor::effective_columns(op, projection, index);
            let total = set.len();
            let mut rows = Vec::with_capacity(total.min(op.max_rows));
            for row in set.rows().iter().take(op.max_rows) {
                let mut raw = Vec::with_capacity(row.len());
                for (col, value) in set.columns().iter().zip(row.values()) {
                    let json =
                        sqlvalue::to_json(value, col.type_name()).map_err(ExecError::Other)?;
                    raw.push((col.name().to_string(), json));
                }
                rows.push(executor::project(raw, columns.as_deref()));
            }
            if total > op.max_rows {
                dl_log::warn!(
                    "{} set {} exceeded {} rows; truncated.",
                    op.name,
                    index,
                    op.max_rows
                );
            }
            out.push(rows);
        }
        Ok(out)
    }
}

/// A `dl-tds` failure as the cloud sees it.
pub fn exec_error(e: &dl_tds::Error) -> ExecError {
    match e {
        dl_tds::Error::Server { number, .. } => ExecError::Sql { number: *number },
        dl_tds::Error::Timeout => ExecError::Sql {
            number: COMMAND_TIMEOUT,
        },
        dl_tds::Error::Io(_) | dl_tds::Error::Tls(_) | dl_tds::Error::Broken => {
            ExecError::Unavailable("the connection was lost".into())
        }
        other => ExecError::Other(format!("PMS reply not understood: {other}")),
    }
}

impl Executor for TdsExecutor {
    fn execute(
        &self,
        op: &Operation,
        params: &Params,
        projection: Option<&Projection>,
    ) -> Result<Sets, ExecError> {
        let sql_params = Self::params(op, params)?;
        let mut session = self.session()?;
        session
            .set_timeout(Some(self.opts.command_timeout))
            .map_err(|e| exec_error(&e))?;
        // Set on every call: a pooled session may come back with whatever the last user left.
        let lock_ms = self.opts.lock_timeout.as_millis().max(1000);
        session
            .execute(&format!("SET LOCK_TIMEOUT {lock_ms}"), &[])
            .map_err(|e| exec_error(&e))?;
        let results = session
            .call(&op.procedure, &sql_params)
            .map_err(|e| exec_error(&e))?;
        Self::read_sets(op, projection, results.into_sets())
    }

    fn schema_fingerprint(&self) -> Option<String> {
        let cat = catalog().ok()?;
        let mut names: Vec<&str> = cat
            .operations()
            .iter()
            .map(|o| o.procedure.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        let placeholders: Vec<String> = (0..names.len()).map(|i| format!("@n{i}")).collect();
        let sql = format!(
            "SELECT o.name, p.name, TYPE_NAME(p.user_type_id)
FROM sys.objects o LEFT JOIN sys.parameters p ON p.object_id = o.object_id
WHERE o.type = 'P' AND o.name IN ({})
ORDER BY o.name, p.parameter_id",
            placeholders.join(", ")
        );
        let params: Vec<Param> = names
            .iter()
            .enumerate()
            .map(|(i, n)| Param::new(&format!("n{i}"), *n))
            .collect();
        let run = || -> Result<String, ExecError> {
            let mut session = self.session()?;
            session
                .set_timeout(Some(FINGERPRINT_TIMEOUT))
                .map_err(|e| exec_error(&e))?;
            let results = session.query(&sql, &params).map_err(|e| exec_error(&e))?;
            let lines: Vec<String> = results
                .rows()
                .iter()
                .map(|r| format!("{}|{}|{}", text(r, 0), text(r, 1), text(r, 2)))
                .collect();
            let digest = crate::contracts::vault_ops::sha256_hex(lines.join("\n").as_bytes());
            Ok(digest[..16].to_string())
        };
        match run() {
            Ok(f) => Some(f),
            Err(e) => {
                dl_log::warn!("Could not read PMS schema fingerprint."; error = %e);
                None
            }
        }
    }

    fn describe_lock_holders(&self) -> Option<String> {
        let run = || -> Result<String, ExecError> {
            let mut session = self.session()?;
            session
                .set_timeout(Some(LOCK_HOLDERS_TIMEOUT))
                .map_err(|e| exec_error(&e))?;
            let results = session
                .query(LOCK_HOLDERS_SQL, &[])
                .map_err(|e| exec_error(&e))?;
            let parts: Vec<String> = results
                .rows()
                .iter()
                .map(|r| {
                    format!(
                        "session {} {} on {} open {}s",
                        int(r, 0),
                        executor::printable(opt_text(r, 1)),
                        executor::printable(opt_text(r, 2)),
                        int(r, 3)
                    )
                })
                .collect();
            Ok(parts.join("; "))
        };
        match run() {
            Ok(t) if t.is_empty() => Some("no open transaction found".into()),
            Ok(t) => Some(t),
            Err(e) => {
                dl_log::warn!(
                    "Could not list PMS lock holders; grant VIEW SERVER STATE to see them.";
                    error = %e
                );
                None
            }
        }
    }
}

fn opt_text(row: &dl_tds::Row, i: usize) -> Option<&str> {
    match row.values().get(i) {
        Some(dl_tds::Value::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn text(row: &dl_tds::Row, i: usize) -> &str {
    opt_text(row, i).unwrap_or("")
}

fn int(row: &dl_tds::Row, i: usize) -> i64 {
    match row.values().get(i) {
        Some(dl_tds::Value::I16(n)) => i64::from(*n),
        Some(dl_tds::Value::I32(n)) => i64::from(*n),
        Some(dl_tds::Value::I64(n)) => *n,
        Some(dl_tds::Value::I8(n)) => i64::from(*n),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_defaults_follow_sqlclient() {
        assert_eq!(
            with_sqlclient_defaults("Server=.;Database=PMS;User Id=u;Password=p"),
            "Server=.;Database=PMS;User Id=u;Password=p;Encrypt=true"
        );
        // Login-only encryption never checks the certificate in SqlClient (RT-0164 test PC).
        assert_eq!(
            with_sqlclient_defaults("Server=.;Encrypt=False;"),
            "Server=.;Encrypt=False;TrustServerCertificate=true"
        );
        assert_eq!(
            with_sqlclient_defaults("Server=.; encrypt = no"),
            "Server=.; encrypt = no;TrustServerCertificate=true"
        );
        assert_eq!(
            with_sqlclient_defaults("Server=.;Encrypt=False;TrustServerCertificate=False"),
            "Server=.;Encrypt=False;TrustServerCertificate=False"
        );
        assert_eq!(
            with_sqlclient_defaults("Server=.;Encrypt=True"),
            "Server=.;Encrypt=True"
        );
        let cfg: dl_tds::Config = with_sqlclient_defaults(
            "Server=.;Integrated Security=true;Database=SMILE_FO;Encrypt=False",
        )
        .parse()
        .unwrap();
        assert!(cfg.trust_server_certificate());
        assert!(
            !with_sqlclient_defaults("Server=.;TrustServerCertificate=True")
                .contains("Trust=True;")
        );
        assert_eq!(
            exec_error(&dl_tds::Error::Timeout),
            ExecError::Sql {
                number: COMMAND_TIMEOUT
            }
        );
        assert_eq!(exec_error(&dl_tds::Error::Broken).wire_number(), Some(-1));
    }
}
