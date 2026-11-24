//! `sql.perf` (PROTOCOL.md §9.9): what the agent's own statements cost the server, read from
//! DMVs: the agent's sessions (identified by the `dl-agent` program name), their logical
//! reads, CPU and elapsed time, and the requests currently blocked (ids and wait types).
//! No row data and no SQL text, the agent's or anyone's, leaves the server.

use dl_json::{Value, json};

use super::pool::Connection;
use crate::Error;

/// The program name the agent logs in with: the only sessions `sql.perf` reports.
pub const APP: &str = "dl-agent";

/// The agent's sessions: counters only.
pub const SESSIONS_SQL: &str = "SELECT CAST(s.session_id AS nvarchar(10)) AS id, \
CAST(s.logical_reads AS nvarchar(20)) AS logical_reads, CAST(s.reads AS nvarchar(20)) AS physical_reads, \
CAST(s.cpu_time AS nvarchar(20)) AS cpu_ms, CAST(s.total_elapsed_time AS nvarchar(20)) AS elapsed_ms, \
CAST(s.row_count AS nvarchar(20)) AS last_row_count \
FROM sys.dm_exec_sessions s WHERE s.program_name = @app AND s.is_user_process = 1";

/// Requests currently blocked, counted: ids and wait times only, no SQL text.
pub const BLOCKED_SQL: &str = "SELECT CAST(r.session_id AS nvarchar(10)) AS id, \
CAST(r.blocking_session_id AS nvarchar(10)) AS blocked_by, CAST(r.wait_time AS nvarchar(20)) AS wait_ms, \
r.wait_type AS wait_type FROM sys.dm_exec_requests r WHERE r.blocking_session_id <> 0";

/// The agent's session counters, costliest statements and blocked requests.
///
/// # Errors
/// [`Error::Sql`] when a DMV is refused (`VIEW SERVER STATE` missing): reported per part,
/// the other parts still answer.
pub fn perf(conn: &mut impl Connection, app: &str) -> Result<Value, Error> {
    let part = |conn: &mut dyn FnMut() -> Result<Vec<Value>, Error>| match conn() {
        Ok(rows) => json!({"ok": true, "rows": rows}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    let sessions = part(&mut || conn.query_json(SESSIONS_SQL, &[("app", app)]));
    let blocked = part(&mut || conn.query_json(BLOCKED_SQL, &[]));
    Ok(json!({"app": app, "sessions": sessions, "blocked": blocked}))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Db;
    impl Connection for Db {
        fn ping(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn is_broken(&self) -> bool {
            false
        }
        fn in_transaction(&self) -> bool {
            false
        }
        fn set_query_timeout(&mut self, _: Option<std::time::Duration>) -> Result<(), Error> {
            Ok(())
        }
        fn query_json(&mut self, sql: &str, p: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
            if sql.contains("dm_exec_sessions s WHERE") {
                assert_eq!(p, [("app", "dl-agent")]);
                return Ok(vec![json!({"id": "55", "logical_reads": "10"})]);
            }
            if sql.contains("dm_exec_requests r WHERE") {
                return Ok(vec![]);
            }
            Err(Error::Sql("unexpected".into()))
        }
    }

    struct Refused;
    impl Connection for Refused {
        fn ping(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn is_broken(&self) -> bool {
            false
        }
        fn in_transaction(&self) -> bool {
            false
        }
        fn set_query_timeout(&mut self, _: Option<std::time::Duration>) -> Result<(), Error> {
            Ok(())
        }
        fn query_json(&mut self, sql: &str, _: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
            if sql.contains("dm_exec_requests") {
                return Ok(vec![]);
            }
            Err(Error::Sql("VIEW SERVER STATE permission denied".into()))
        }
    }

    #[test]
    fn counters_only_and_one_refused_dmv_does_not_hide_the_other() {
        let v = perf(&mut Db, "dl-agent").unwrap();
        assert_eq!(v["sessions"]["ok"].as_bool(), Some(true));
        assert_eq!(v["blocked"]["ok"].as_bool(), Some(true));
        assert!(v["blocked"]["rows"].as_array().unwrap().is_empty());
        assert!(!v.to_string().contains("text"));
        let err = perf(&mut Refused, "dl-agent").unwrap();
        assert_eq!(err["sessions"]["ok"].as_bool(), Some(false));
        assert!(
            err["sessions"]["error"]
                .as_str()
                .unwrap()
                .contains("VIEW SERVER STATE")
        );
    }
}
