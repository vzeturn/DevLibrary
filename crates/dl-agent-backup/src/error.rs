//! The crate's error type. Every variant maps to one stable report code ([`Error::code`]),
//! and no variant ever carries a password, a token or a connection string.

use std::fmt;

/// What went wrong in the backup module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The hub could not be reached or answered with an error (`backup_hub_failed`).
    Hub(String),
    /// The hub rejected the agent's or the enroll key's credentials (`backup_auth_failed`).
    Auth(String),
    /// SQL Server refused a connection or a statement (`backup_sql_failed`).
    Sql(String),
    /// The cloud destination refused an upload, download or delete (`backup_upload_failed`).
    Upload(String),
    /// A credential could not be opened, here or at the hub (`backup_credential_error`).
    Credential(String),
    /// The schedule targets a provider this build cannot write to yet
    /// (`backup_destination_unsupported`).
    Unsupported(String),
    /// A log backup was asked of a database in SIMPLE recovery (`backup_recovery_model`).
    RecoveryModel(String),
    /// A schedule's cron expression or time zone is not usable (`backup_schedule_invalid`).
    Schedule(String),
    /// Local files: the backup file, the module's state (`backup_io_failed`).
    Io(String),
    /// The work was stopped because the agent is shutting down (`backup_cancelled`).
    Cancelled,
    /// No `backup-enroll` has run on this machine (`backup_not_enrolled`).
    NotEnrolled,
}

impl Error {
    /// The stable report code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Hub(_) => "backup_hub_failed",
            Error::Auth(_) => "backup_auth_failed",
            Error::Sql(_) => "backup_sql_failed",
            Error::Upload(_) => "backup_upload_failed",
            Error::Credential(_) => "backup_credential_error",
            Error::Unsupported(_) => "backup_destination_unsupported",
            Error::RecoveryModel(_) => "backup_recovery_model",
            Error::Schedule(_) => "backup_schedule_invalid",
            Error::Io(_) => "backup_io_failed",
            Error::Cancelled => "backup_cancelled",
            Error::NotEnrolled => "backup_not_enrolled",
        }
    }

    /// A local I/O failure with what was being done.
    pub fn io(what: &str, e: &std::io::Error) -> Error {
        Error::Io(format!("{what}: {e}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Hub(m)
            | Error::Auth(m)
            | Error::Sql(m)
            | Error::Upload(m)
            | Error::Credential(m)
            | Error::Unsupported(m)
            | Error::RecoveryModel(m)
            | Error::Schedule(m)
            | Error::Io(m) => f.write_str(m),
            Error::Cancelled => f.write_str("stopped because the agent is shutting down"),
            Error::NotEnrolled => {
                f.write_str("not enrolled: run `dl-agent backup-enroll --hub-url <url>` first")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for dl_agent::Error {
    fn from(e: Error) -> dl_agent::Error {
        match e {
            Error::NotEnrolled => dl_agent::Error::NotEnrolled,
            Error::Sql(m) => dl_agent::Error::Sql(m),
            other => dl_agent::Error::Module(format!("{}: {other}", other.code())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_brief_code_is_stable() {
        let cases = [
            (Error::Hub(String::new()), "backup_hub_failed"),
            (Error::Auth(String::new()), "backup_auth_failed"),
            (Error::Sql(String::new()), "backup_sql_failed"),
            (Error::Upload(String::new()), "backup_upload_failed"),
            (Error::Credential(String::new()), "backup_credential_error"),
            (
                Error::Unsupported(String::new()),
                "backup_destination_unsupported",
            ),
            (Error::RecoveryModel(String::new()), "backup_recovery_model"),
        ];
        for (e, code) in cases {
            assert_eq!(e.code(), code);
        }
    }

    #[test]
    fn module_error_carries_the_code() {
        let e: dl_agent::Error = Error::Upload("bucket gone".into()).into();
        assert!(
            e.to_string().contains("backup_upload_failed: bucket gone"),
            "{e}"
        );
    }
}
