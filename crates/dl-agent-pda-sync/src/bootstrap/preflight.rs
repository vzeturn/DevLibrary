//! What the enroll preflight checks before any write (port of `Bootstrap/SqlPreflight.cs`):
//! the server's version and authentication mode, as findings.

use super::error::{Error, Result, tds_failure};
use super::models::SqlInstance;
use crate::config::{AgentError, codes};
use crate::contracts::wire::site_event_levels;
use std::fmt;

/// `SERVERPROPERTY` values the preflight checks and prints (`SqlServerInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlServerInfo {
    /// `ProductVersion`, e.g. `16.0.4135.4` (`0` when NULL).
    pub product_version: String,
    /// `Edition` (`unknown` when NULL).
    pub edition: String,
    /// `ProductLevel`, e.g. `RTM` (empty when NULL).
    pub product_level: String,
    /// `IsIntegratedSecurityOnly = 1`: Windows logins only.
    pub integrated_security_only: bool,
}

/// `SqlServerInfo.Query` (the C# raw string literal, CRLF line ends as built on Windows).
pub const SERVER_INFO_QUERY: &str = "SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)),\r\n       CAST(SERVERPROPERTY('Edition') AS nvarchar(128)),\r\n       CAST(SERVERPROPERTY('ProductLevel') AS nvarchar(128)),\r\n       CAST(SERVERPROPERTY('IsIntegratedSecurityOnly') AS int)";

impl SqlServerInfo {
    /// A record.
    pub fn new(
        product_version: &str,
        edition: &str,
        product_level: &str,
        integrated_security_only: bool,
    ) -> SqlServerInfo {
        SqlServerInfo {
            product_version: product_version.to_owned(),
            edition: edition.to_owned(),
            product_level: product_level.to_owned(),
            integrated_security_only,
        }
    }

    /// Major version (11 = 2012, ..., 16 = 2022); 0 when unparseable
    /// (`int.TryParse(NumberStyles.None)` of the text before the first dot).
    pub fn major(&self) -> i32 {
        let head = self.product_version.split('.').next().unwrap_or("");
        if head.is_empty() || !head.bytes().all(|b| b.is_ascii_digit()) {
            return 0;
        }
        head.parse().unwrap_or(0)
    }

    /// Reads the properties on an open connection (read-only, `SqlServerInfo.ReadAsync`).
    pub fn read(client: &mut dl_tds::Client) -> Result<SqlServerInfo> {
        let results = client.query(SERVER_INFO_QUERY, &[]).map_err(|e| {
            let messages = client.take_messages();
            match tds_failure(&e, &messages) {
                Ok(f) => Error::sql_exception(&f),
                Err(ex) => Error::Unexpected(ex),
            }
        })?;
        client.take_messages();
        let Some(row) = results.rows().first() else {
            return Err(Error::unexpected(
                "System.InvalidOperationException",
                "SERVERPROPERTY returned no row.",
            ));
        };
        let text = |i: usize| -> Result<Option<String>> {
            row.get::<Option<String>>(i)
                .map_err(|e| Error::unexpected("System.InvalidCastException", e.to_string()))
        };
        let version = text(0)?.unwrap_or_else(|| "0".to_owned());
        let edition = text(1)?.unwrap_or_else(|| "unknown".to_owned());
        let level = text(2)?.unwrap_or_default();
        let integrated_only = row
            .get::<Option<i32>>(3)
            .map_err(|e| Error::unexpected("System.InvalidCastException", e.to_string()))?
            == Some(1);
        Ok(SqlServerInfo {
            product_version: version,
            edition,
            product_level: level,
            integrated_security_only: integrated_only,
        })
    }
}

impl fmt::Display for SqlServerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} ({})",
            self.product_version, self.product_level, self.edition
        )
    }
}

/// One preflight result line: `warn` lets the run continue, `error` stops it
/// (`PreflightFinding`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightFinding {
    /// `site_event_levels::WARN` or `ERROR`.
    pub level: String,
    /// Stable code.
    pub code: String,
    /// What to do about it.
    pub message: String,
}

impl PreflightFinding {
    /// A blocking finding.
    pub fn error(code: &str, message: impl Into<String>) -> PreflightFinding {
        PreflightFinding {
            level: site_event_levels::ERROR.to_owned(),
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// A warning.
    pub fn warn(code: &str, message: impl Into<String>) -> PreflightFinding {
        PreflightFinding {
            level: site_event_levels::WARN.to_owned(),
            code: code.to_owned(),
            message: message.into(),
        }
    }

    /// A blocking finding from a coded failure.
    pub fn from_agent(e: &AgentError) -> PreflightFinding {
        PreflightFinding::error(e.code(), e.message())
    }

    /// Whether it stops the run.
    pub fn is_error(&self) -> bool {
        self.level == site_event_levels::ERROR
    }
}

/// What the SQL preflight found: the located database, the server, and anything that
/// blocks the bootstrap (`SqlPreflight`).
#[derive(Debug, Clone, PartialEq)]
pub struct SqlPreflight {
    /// The instance.
    pub instance: SqlInstance,
    /// The POS database.
    pub database: String,
    /// The schema fingerprint score.
    pub fingerprint_score: f64,
    /// The server.
    pub server: SqlServerInfo,
    /// Findings of [`check_server`].
    pub findings: Vec<PreflightFinding>,
}

impl SqlPreflight {
    /// Whether a finding blocks the bootstrap.
    pub fn blocked(&self) -> bool {
        self.findings.iter().any(PreflightFinding::is_error)
    }

    /// `SQL Server {server}, database {db} on {instance}`.
    pub fn describe(&self) -> String {
        format!(
            "SQL Server {}, database {} on {}",
            self.server, self.database, self.instance
        )
    }
}

/// `SqlServerSupport.MinMajor`: the scripts need SQL Server 2012 (`ALTER ROLE ... ADD
/// MEMBER`).
pub const MIN_MAJOR: i32 = 11;
/// `SqlServerSupport.MinName`.
pub const MIN_NAME: &str = "SQL Server 2012 (11.x)";

/// `SqlServerSupport.Check`: Windows-only authentication and a server below 2012 block
/// the bootstrap.
pub fn check_server(instance: &SqlInstance, server: &SqlServerInfo) -> Vec<PreflightFinding> {
    let mut findings = Vec::new();
    if server.integrated_security_only {
        findings.push(PreflightFinding::error(
            codes::SQL_WINDOWS_AUTH_ONLY,
            format!(
                "SQL Server '{instance}' accepts Windows logins only, but the agent signs in to the POS database as the SQL login pda_sync. \
                 Enable 'SQL Server and Windows Authentication mode' (SQL Server Management Studio: server Properties > Security), \
                 restart the SQL Server service, then run enroll again."
            ),
        ));
    }
    if server.major() < MIN_MAJOR {
        findings.push(PreflightFinding::error(
            codes::SQL_SERVER_TOO_OLD,
            format!(
                "SQL Server '{instance}' is version {} ({}); the agent setup needs {MIN_NAME} or newer. \
                 Upgrade SQL Server before enrolling.",
                server.product_version, server.edition
            ),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_versions_parse_like_int_try_parse() {
        let v = |s: &str| SqlServerInfo::new(s, "e", "RTM", false).major();
        assert_eq!(v("16.0.4135.4"), 16);
        assert_eq!(v("10.50.6000.34"), 10);
        assert_eq!(v("x.1"), 0);
        assert_eq!(v(" 11.0"), 0);
        assert_eq!(v("+11.0"), 0);
        assert_eq!(v(""), 0);
        assert_eq!(v("99999999999.0"), 0);
    }

    #[test]
    fn findings_block_windows_only_and_old_servers() {
        let i = SqlInstance::new(r".\SQLEXPRESS", "test");
        let ok = SqlServerInfo::new("11.0.7001.0", "Express", "SP4", false);
        assert!(check_server(&i, &ok).is_empty());
        let both = SqlServerInfo::new("10.50.6000.34", "Express", "SP3", true);
        let f = check_server(&i, &both);
        assert_eq!(
            f.iter().map(|f| f.code.as_str()).collect::<Vec<_>>(),
            [codes::SQL_WINDOWS_AUTH_ONLY, codes::SQL_SERVER_TOO_OLD]
        );
        assert!(f.iter().all(PreflightFinding::is_error));
        assert!(f[0].message.contains("Windows Authentication"));
        assert!(f[1].message.contains("2012"));
        assert_eq!(ok.to_string(), "11.0.7001.0 SP4 (Express)");
    }
}
