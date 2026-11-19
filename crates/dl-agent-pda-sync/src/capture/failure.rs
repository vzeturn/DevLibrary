//! How a capture failure is reported (port of the agent's `Capture/CaptureFailure.cs`).

use super::error::{CaptureError, ErrorKind};
use crate::config::codes;

/// How a capture failure is reported: a stable code for the heartbeat and whether it
/// proves the SQL Server is (`Some(true)`) or is not (`Some(false)`) usable; `None` leaves
/// the last known reachability unchanged (`CaptureFailureInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureFailureInfo {
    /// Stable code (`codes::*`).
    pub code: String,
    /// What the failure proves about the SQL Server.
    pub sql_reachable: Option<bool>,
}

impl CaptureFailureInfo {
    /// A failure info.
    pub fn new(code: &str, sql_reachable: Option<bool>) -> CaptureFailureInfo {
        CaptureFailureInfo {
            code: code.to_owned(),
            sql_reachable,
        }
    }
}

/// Network/instance errors: named pipes/TCP not reachable, instance not found, connection
/// reset, connect timeout.
const UNREACHABLE: [i32; 17] = [
    -2, -1, 2, 26, 40, 53, 64, 121, 233, 258, 1225, 10053, 10054, 10060, 10061, 11001, 11004,
];
const NO_ACCESS: [i32; 5] = [229, 230, 262, 297, 300];
const SCHEMA_MISMATCH: [i32; 3] = [207, 208, 4104];
/// Lock timeout / deadlock victim outside the source's read path: the POS is busy.
const BUSY: [i32; 2] = [1205, 1222];
/// 18456: the login was refused. 18452: a Windows login from an untrusted domain.
const LOGIN_FAILED: [i32; 2] = [18452, 18456];
const CANNOT_OPEN_DATABASE: i32 = 4060;

/// `CaptureFailure.Classify`: maps a session failure to its code (by the outermost kind).
pub fn classify(error: &CaptureError) -> CaptureFailureInfo {
    match &error.kind {
        ErrorKind::Backpressure(code) => CaptureFailureInfo::new(code, Some(true)),
        ErrorKind::Agent(code) => CaptureFailureInfo::new(code, Some(false)),
        ErrorKind::Busy => CaptureFailureInfo::new(codes::SQL_BUSY, Some(true)),
        ErrorKind::Sql => {
            let (number, class) = error
                .sql
                .as_ref()
                .map_or((0, 0), |s| (s.number(), s.class()));
            let code = code_for(number, class);
            let reachable = !matches!(
                code,
                codes::SQL_LOGIN_FAILED | codes::SQL_DATABASE_NOT_FOUND | codes::SQL_UNREACHABLE
            );
            CaptureFailureInfo::new(code, Some(reachable))
        }
        ErrorKind::LocalStore => CaptureFailureInfo::new(codes::LOCAL_STORE_FAILED, None),
        ErrorKind::TimeZone => CaptureFailureInfo::new(codes::SITE_TIME_ZONE_INVALID, None),
        _ => CaptureFailureInfo::new(codes::CAPTURE_FAILED, None),
    }
}

/// `CaptureFailure.CodeFor`: the stable code of a SQL Server error by its number and
/// severity class. The one-shot tools use it too, so a refused login, a wrong database and
/// an unreachable server read the same everywhere.
pub fn code_for(number: i32, severity: u8) -> &'static str {
    if LOGIN_FAILED.contains(&number) {
        codes::SQL_LOGIN_FAILED
    } else if number == CANNOT_OPEN_DATABASE {
        codes::SQL_DATABASE_NOT_FOUND
    } else if BUSY.contains(&number) {
        codes::SQL_BUSY
    } else if UNREACHABLE.contains(&number) || severity >= 20 {
        codes::SQL_UNREACHABLE
    } else if NO_ACCESS.contains(&number) {
        codes::SQL_NO_ACCESS
    } else if SCHEMA_MISMATCH.contains(&number) {
        codes::SQL_SCHEMA_MISMATCH
    } else {
        codes::SQL_ERROR
    }
}
