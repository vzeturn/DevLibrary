//! The safe facts of a SQL Server error for a site event (port of
//! `Reporting/SqlErrorDetail.cs`, AD1), and [`SqlFailure`], the part of a C# `SqlException`
//! those facts come from, built from a `dl_tds` error.
//!
//! Every SQL error gives `sql_number` and `sql_class`. A schema error (207 invalid column,
//! 208 invalid object, 4104 multi-part identifier not bound) also gives the identifier SQL
//! Server names in its message and the capture table that was being read. The rest of the
//! message is never kept: other errors can quote row values.

use super::log::Exception;
use dl_json::{Map, Value};

/// `Invalid column name 'X'.`
pub const INVALID_COLUMN: i32 = 207;
/// `Invalid object name 'dbo.X'.`
pub const INVALID_OBJECT: i32 = 208;
/// `The multi-part identifier "a.X" could not be bound.`
pub const MULTI_PART_NOT_BOUND: i32 = 4104;
/// A SQL Server identifier (sysname) is at most 128 characters.
pub const MAX_IDENTIFIER: usize = 128;

/// One `SqlError` of a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlErrorInfo {
    /// The error number.
    pub number: i32,
    /// The severity class.
    pub class: u8,
    /// The server's message.
    pub message: String,
}

/// What a C# `SqlException` carries that the agent reads: its errors (the first one gives
/// `Number` and `Class`) and the capture table the capture source attached
/// (`ex.Data[SqlServerCaptureSource.CaptureTableDataKey]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlFailure {
    /// The errors in the order the server sent them (never empty).
    pub errors: Vec<SqlErrorInfo>,
    /// The capture table being read, if the capture source set it.
    pub capture_table: Option<String>,
}

/// The .NET type name of a SQL Server failure.
pub const SQL_EXCEPTION_TYPE: &str = "Microsoft.Data.SqlClient.SqlException";

impl SqlFailure {
    /// A failure with one error.
    pub fn single(number: i32, class: u8, message: impl Into<String>) -> SqlFailure {
        SqlFailure {
            errors: vec![SqlErrorInfo {
                number,
                class,
                message: message.into(),
            }],
            capture_table: None,
        }
    }

    /// `SqlException.Number`: the first error's number.
    pub fn number(&self) -> i32 {
        self.errors.first().map_or(0, |e| e.number)
    }

    /// `SqlException.Class`: the first error's class.
    pub fn class(&self) -> u8 {
        self.errors.first().map_or(0, |e| e.class)
    }

    /// `SqlException.Message`: the messages joined by the platform newline.
    pub fn message(&self) -> String {
        self.errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join(crate::util::stj::NEWLINE)
    }

    /// The failure a `Microsoft.Data.SqlClient` call would raise for a `dl_tds` error, or
    /// `None` for an error SqlClient does not report as a `SqlException` (parameter and
    /// conversion errors are client-side exceptions there).
    ///
    /// `messages` are the server messages the request produced
    /// (`Client::take_messages`): their errors become the error list, as SqlClient
    /// collects every ERROR token. Without them a server error gives one entry. Errors
    /// without a server answer map like SqlClient's: a timeout is number -2 class 11; a
    /// network failure the OS error number (Winsock code on Windows) with class 20; a TLS
    /// or login-protocol failure, a browser lookup failure and other transport failures
    /// number 0 (browser: -1) with class 20.
    pub fn from_tds(error: &dl_tds::Error, messages: &[dl_tds::Message]) -> Option<SqlFailure> {
        let server_errors: Vec<SqlErrorInfo> = messages
            .iter()
            .filter(|m| m.kind == dl_tds::MessageKind::Error)
            .map(|m| SqlErrorInfo {
                number: m.number,
                class: m.class,
                message: m.message.clone(),
            })
            .collect();
        let failure =
            |number: i32, class: u8, message: String| SqlFailure::single(number, class, message);
        Some(match error {
            dl_tds::Error::Server {
                number,
                class,
                message,
                ..
            } => {
                if server_errors.is_empty() {
                    failure(*number, *class, message.clone())
                } else {
                    SqlFailure {
                        errors: server_errors,
                        capture_table: None,
                    }
                }
            }
            dl_tds::Error::Timeout => failure(
                -2,
                11,
                "Execution Timeout Expired.  The timeout period elapsed prior to completion of the operation or the server is not responding.".to_owned(),
            ),
            dl_tds::Error::Io(e) => failure(e.raw_os_error().unwrap_or(0), 20, error.to_string()),
            dl_tds::Error::Browser(_) => failure(-1, 20, error.to_string()),
            dl_tds::Error::Tls(_)
            | dl_tds::Error::Malformed { .. }
            | dl_tds::Error::EncryptionNotSupported
            | dl_tds::Error::Routing { .. }
            | dl_tds::Error::Broken
            | dl_tds::Error::Sspi { .. } => failure(0, 20, error.to_string()),
            _ => return None,
        })
    }

    /// This failure as a logged exception (`SqlException`).
    pub fn to_exception(&self) -> Exception {
        let mut ex = Exception::new(SQL_EXCEPTION_TYPE, self.message());
        ex.sql = Some(self.clone());
        ex
    }
}

/// Whether `number` is one of the schema errors.
pub fn is_schema_error(number: i32) -> bool {
    matches!(
        number,
        INVALID_COLUMN | INVALID_OBJECT | MULTI_PART_NOT_BOUND
    )
}

/// At most [`MAX_IDENTIFIER`] UTF-16 units of `s` (whole characters only).
fn cap(s: &str) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > MAX_IDENTIFIER {
            break;
        }
        out.push(c);
    }
    out
}

/// The identifier a schema error names: `Invalid column name 'X'.`, `Invalid object name
/// 'dbo.X'.`, `The multi-part identifier "a.X" could not be bound.` `None` for any other
/// error or an unexpected message shape (a localized server message): only the first
/// quoted part (1 to 512 characters) of these fixed texts is taken, capped at 128.
pub fn identifier(number: i32, message: Option<&str>) -> Option<String> {
    let message = message.filter(|m| !m.is_empty())?;
    if !is_schema_error(number) {
        return None;
    }
    let quote = if number == MULTI_PART_NOT_BOUND {
        '"'
    } else {
        '\''
    };
    let open = message.find(quote)?;
    let rest = &message[open + 1..];
    let close = rest.find(quote)?;
    let id = &rest[..close];
    let len: usize = id.chars().map(char::len_utf16).sum();
    if len == 0 || len > 512 {
        return None;
    }
    Some(cap(id))
}

/// Adds the facts of the first SQL failure in the exception chain to `detail`; no-op
/// without one.
pub fn add_to(detail: &mut Map, exception: Option<&Exception>) {
    let Some(sql) = exception.and_then(|e| e.chain().find_map(|x| x.sql.as_ref())) else {
        return;
    };
    detail.insert("sql_number", Value::from(i64::from(sql.number())));
    detail.insert("sql_class", Value::from(i64::from(sql.class())));
    let Some(schema) = sql.errors.iter().find(|e| is_schema_error(e.number)) else {
        return;
    };
    if let Some(id) = identifier(schema.number, Some(&schema.message)) {
        detail.insert("sql_identifier", Value::from(id));
    }
    if schema.number != sql.number() {
        detail.insert("sql_schema_number", Value::from(i64::from(schema.number)));
    }
    if let Some(table) = &sql.capture_table {
        detail.insert("capture_table", Value::from(cap(table)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_is_the_quoted_name_of_a_schema_error_only() {
        for (n, m, want) in [
            (207, "Invalid column name 'RvcNo2'.", Some("RvcNo2")),
            (
                208,
                "Invalid object name 'dbo.tmpTrnInfo'.",
                Some("dbo.tmpTrnInfo"),
            ),
            (
                4104,
                "The multi-part identifier \"t.KdsDone\" could not be bound.",
                Some("t.KdsDone"),
            ),
            (207, "Ungültiger Spaltenname", None),
            (
                245,
                "Conversion failed when converting the varchar value 'secret' to data type int.",
                None,
            ),
        ] {
            assert_eq!(identifier(n, Some(m)).as_deref(), want, "{m}");
        }
    }

    #[test]
    fn identifier_is_capped_at_a_sysname() {
        let id = "c".repeat(300);
        assert_eq!(
            identifier(207, Some(&format!("Invalid column name '{id}'.")))
                .unwrap()
                .len(),
            MAX_IDENTIFIER
        );
        assert_eq!(
            identifier(
                207,
                Some(&format!("Invalid column name '{}'.", "c".repeat(513)))
            ),
            None
        );
    }

    #[test]
    fn a_schema_error_gives_number_class_identifier_and_capture_table() {
        let mut sql = SqlFailure::single(207, 16, "Invalid column name 'RvcNo2'.");
        sql.capture_table = Some("tmpTrnInfo".into());
        let ex = Exception::new("System.InvalidOperationException", "wrapped")
            .with_inner(sql.to_exception());
        let mut detail = Map::new();
        add_to(&mut detail, Some(&ex));
        assert_eq!(detail.get("sql_number").and_then(Value::as_i64), Some(207));
        assert_eq!(detail.get("sql_class").and_then(Value::as_i64), Some(16));
        assert_eq!(
            detail.get("sql_identifier").and_then(Value::as_str),
            Some("RvcNo2")
        );
        assert_eq!(
            detail.get("capture_table").and_then(Value::as_str),
            Some("tmpTrnInfo")
        );
    }

    #[test]
    fn another_sql_error_gives_number_and_class_only() {
        let mut detail = Map::new();
        let ex = SqlFailure::single(
            245,
            16,
            "Conversion failed when converting the varchar value 'secret' to data type int.",
        )
        .to_exception();
        add_to(&mut detail, Some(&ex));
        let keys: Vec<&str> = detail.keys().map(String::as_str).collect();
        assert_eq!(keys, ["sql_number", "sql_class"]);
        add_to(&mut detail, None);
        assert_eq!(detail.len(), 2);
    }

    #[test]
    fn a_later_schema_error_reports_its_own_number() {
        let sql = SqlFailure {
            errors: vec![
                SqlErrorInfo {
                    number: 8180,
                    class: 16,
                    message: "Statement(s) could not be prepared.".into(),
                },
                SqlErrorInfo {
                    number: 207,
                    class: 16,
                    message: "Invalid column name 'X'.".into(),
                },
            ],
            capture_table: None,
        };
        let mut detail = Map::new();
        add_to(&mut detail, Some(&sql.to_exception()));
        assert_eq!(detail.get("sql_number").and_then(Value::as_i64), Some(8180));
        assert_eq!(
            detail.get("sql_schema_number").and_then(Value::as_i64),
            Some(207)
        );
        assert_eq!(
            detail.get("sql_identifier").and_then(Value::as_str),
            Some("X")
        );
    }

    #[test]
    fn tds_errors_map_like_sqlclient() {
        let e = dl_tds::Error::Timeout;
        let f = SqlFailure::from_tds(&e, &[]).unwrap();
        assert_eq!((f.number(), f.class()), (-2, 11));
        let e = dl_tds::Error::Server {
            number: 207,
            state: 1,
            class: 16,
            message: "Invalid column name 'B'.".into(),
            server: "s".into(),
            procedure: String::new(),
            line: 1,
        };
        let f = SqlFailure::from_tds(&e, &[]).unwrap();
        assert_eq!((f.number(), f.class()), (207, 16));
        assert!(SqlFailure::from_tds(&dl_tds::Error::ColumnNotFound("x".into()), &[]).is_none());
    }
}
