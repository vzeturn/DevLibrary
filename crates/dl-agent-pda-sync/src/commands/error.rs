//! Errors of the command store and units: what a C# command threw (`SqlException`,
//! `InvalidOperationException`, cancellation). Every one rolls the command's transaction
//! back; the channel reports it as a failed command the cloud retries.

use crate::reporting::Exception;
use crate::reporting::sql_error::SqlFailure;
use crate::util::ctx::Ctx;

/// SQL error numbers of a missing grant (`SqlPermissionDenied.Is`: 229, 230, 262).
pub const PERMISSION_DENIED: [i32; 3] = [229, 230, 262];

/// Why a command did not produce a result.
#[derive(Debug)]
pub enum CommandError {
    /// SQL Server or the connection failed (`SqlException`), with the server messages
    /// of the request (SqlClient's `SqlException.Errors`).
    Sql(dl_tds::Error, Vec<dl_tds::Message>),
    /// The store answered something the agent cannot use (`InvalidOperationException`).
    Invalid(String),
    /// A programming error: an argument outside what the caller may pass
    /// (`ArgumentException`).
    Argument(String),
    /// The command's context was cancelled or its deadline passed.
    Cancelled,
    /// A coded failure before SQL ran, e.g. no SQL target or password (`AgentException`).
    Agent(crate::config::AgentError),
}

impl CommandError {
    /// An [`CommandError::Invalid`] with `message`.
    pub fn invalid(message: impl Into<String>) -> CommandError {
        CommandError::Invalid(message.into())
    }

    /// The SQL error number, when SQL Server raised one (`SqlException.Number`).
    pub fn sql_number(&self) -> Option<i32> {
        match self {
            CommandError::Sql(e, _) => e.number(),
            _ => None,
        }
    }

    /// `SqlPermissionDenied.Is`: the login lacks a grant (229, 230 or 262 as the number
    /// or among the server's errors).
    pub fn is_permission_denied(&self) -> bool {
        match self {
            CommandError::Sql(e, messages) => {
                e.number().is_some_and(|n| PERMISSION_DENIED.contains(&n))
                    || messages.iter().any(|m| {
                        m.kind == dl_tds::MessageKind::Error
                            && PERMISSION_DENIED.contains(&m.number)
                    })
            }
            _ => false,
        }
    }

    /// The failure as the exception the C# agent logged.
    pub fn to_exception(&self) -> Exception {
        match self {
            CommandError::Sql(e, messages) => match SqlFailure::from_tds(e, messages) {
                Some(f) => f.to_exception(),
                None => Exception::new("System.InvalidOperationException", e.to_string()),
            },
            CommandError::Invalid(m) => Exception::new("System.InvalidOperationException", m),
            CommandError::Argument(m) => Exception::new("System.ArgumentException", m),
            CommandError::Cancelled => Exception::new(
                "System.OperationCanceledException",
                "The operation was canceled.",
            ),
            CommandError::Agent(e) => Exception::from_agent_error(e),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Sql(e, _) => e.fmt(f),
            CommandError::Invalid(m) | CommandError::Argument(m) => f.write_str(m),
            CommandError::Cancelled => f.write_str("The operation was canceled."),
            CommandError::Agent(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CommandError::Sql(e, _) => Some(e),
            CommandError::Agent(e) => Some(e),
            _ => None,
        }
    }
}

impl From<crate::config::AgentError> for CommandError {
    fn from(e: crate::config::AgentError) -> Self {
        CommandError::Agent(e)
    }
}

impl From<dl_tds::Error> for CommandError {
    fn from(e: dl_tds::Error) -> Self {
        CommandError::Sql(e, Vec::new())
    }
}

/// Result of the command store and units.
pub type Result<T> = std::result::Result<T, CommandError>;

/// [`CommandError::Cancelled`] when `ctx` is done (the C# `ct.ThrowIfCancellationRequested`).
pub fn check_ctx(ctx: &Ctx) -> Result<()> {
    if ctx.is_done() {
        Err(CommandError::Cancelled)
    } else {
        Ok(())
    }
}
