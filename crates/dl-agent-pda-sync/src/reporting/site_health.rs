//! Health transitions as site events (port of `Reporting/SiteHealthMonitor.cs`): POS
//! database unreachable / recovered, capture error set / cleared, token refresh failing /
//! recovered, upload backlog over the threshold / drained. The first observation is a
//! silent baseline except for a problem that is already present.

use std::sync::{Arc, Mutex, PoisonError};

use dl_json::{Map, Value as JsonValue};

use super::log::Exception;
use super::site_events::SiteEvents;
use crate::config::codes;
use crate::contracts::wire::site_event_levels::{ERROR, INFO, WARN};
use crate::outbox::CaptureStatus;

/// The database is reachable again.
pub const SQL_RECOVERED: &str = "SQL_RECOVERED";
/// Capture recovered from an error.
pub const CAPTURE_ERROR_CLEARED: &str = "CAPTURE_ERROR_CLEARED";
/// The cloud token cannot be refreshed.
pub const TOKEN_REFRESH_FAILING: &str = "TOKEN_REFRESH_FAILING";
/// The cloud token refreshes again.
pub const TOKEN_REFRESH_RECOVERED: &str = "TOKEN_REFRESH_RECOVERED";
/// The upload backlog is over the threshold.
pub const UPLOAD_BACKLOG: &str = "UPLOAD_BACKLOG";
/// The upload backlog drained.
pub const UPLOAD_BACKLOG_DRAINED: &str = "UPLOAD_BACKLOG_DRAINED";

const WARN_ONLY: [&str; 3] = [codes::SQL_BUSY, codes::OUTBOX_FULL, codes::BLOB_QUEUE_FULL];

/// [`CaptureStatus`] plus the safe facts of the current capture error (the C#
/// `ICaptureStatus.CaptureErrorDetail`, default `null`).
pub trait CaptureHealth: CaptureStatus {
    /// Safe facts about the capture error for its site event (failing job, skipped
    /// tables, SQL number).
    fn capture_error_detail(&self) -> Option<Map> {
        None
    }
}

/// Reads the upload backlog depth (the outbox depth); an error is logged by the caller.
pub type BacklogDepth = Arc<dyn Fn() -> Result<i64, Box<Exception>> + Send + Sync>;
/// The code of the failing token refresh, `None` when it works.
pub type TokenFailure = Arc<dyn Fn() -> Option<String> + Send + Sync>;

#[derive(Debug, Default)]
struct State {
    sql_down: bool,
    capture_error: Option<String>,
    token_failure: Option<String>,
    backlog: bool,
}

/// The monitor (`SiteHealthMonitor`).
pub struct SiteHealthMonitor {
    events: Arc<dyn SiteEvents>,
    capture: Arc<dyn CaptureHealth>,
    backlog_depth: BacklogDepth,
    token_failure: TokenFailure,
    backlog_threshold: i32,
    state: Mutex<State>,
}

impl std::fmt::Debug for SiteHealthMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteHealthMonitor")
            .field("backlog_threshold", &self.backlog_threshold)
            .finish_non_exhaustive()
    }
}

fn obj(pairs: Vec<(&str, JsonValue)>) -> Option<Map> {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k, v);
    }
    Some(m)
}

impl SiteHealthMonitor {
    /// A monitor.
    pub fn new(
        events: Arc<dyn SiteEvents>,
        capture: Arc<dyn CaptureHealth>,
        backlog_depth: BacklogDepth,
        token_failure: TokenFailure,
        backlog_threshold: i32,
    ) -> SiteHealthMonitor {
        SiteHealthMonitor {
            events,
            capture,
            backlog_depth,
            token_failure,
            backlog_threshold,
            state: Mutex::default(),
        }
    }

    /// One check of every health signal.
    pub fn check(&self) -> Result<(), Box<Exception>> {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.check_sql(&mut st);
        self.check_capture(&mut st);
        self.check_token(&mut st);
        self.check_backlog(&mut st)
    }

    fn check_sql(&self, st: &mut State) {
        let error = self.capture.capture_error();
        let down = !self.capture.sql_reachable() && error.is_some();
        if down && !st.sql_down {
            let e = error.unwrap_or_default();
            self.events.report(
                ERROR,
                codes::SQL_UNREACHABLE,
                &format!("The POS database cannot be reached ({e}); capture is paused"),
                obj(vec![("capture_error", JsonValue::from(e))]),
            );
        } else if !down && st.sql_down && self.capture.sql_reachable() {
            self.events.report(
                INFO,
                SQL_RECOVERED,
                "The POS database is reachable again; capture resumes",
                None,
            );
        } else {
            return;
        }
        st.sql_down = down;
    }

    fn check_capture(&self, st: &mut State) {
        let current = self.capture.capture_error();
        if current == st.capture_error {
            return;
        }
        let previous = std::mem::replace(&mut st.capture_error, current.clone());
        let Some(current) = current else {
            // Recovery from an unreachable database is SQL_RECOVERED; one event per transition.
            if previous.as_deref() != Some(codes::SQL_UNREACHABLE) {
                let p = previous.clone().unwrap_or_default();
                self.events.report(
                    INFO,
                    CAPTURE_ERROR_CLEARED,
                    &format!("Capture recovered from {p}"),
                    obj(vec![("previous", JsonValue::from(previous))]),
                );
            }
            return;
        };
        // SQL_UNREACHABLE with the database down is already reported as its own transition.
        if current == codes::SQL_UNREACHABLE && st.sql_down {
            return;
        }
        let mut detail = self.capture.capture_error_detail().unwrap_or_default();
        detail.insert("capture_error", current.as_str());
        detail.insert("sql_reachable", self.capture.sql_reachable());
        if let Some(p) = &previous {
            detail.insert("previous", p.as_str());
        }
        let level = if WARN_ONLY.contains(&current.as_str()) {
            WARN
        } else {
            ERROR
        };
        self.events.report(
            level,
            &current,
            &format!("Capture stopped with {current}"),
            Some(detail),
        );
    }

    fn check_token(&self, st: &mut State) {
        let failure = (self.token_failure)();
        if failure == st.token_failure {
            return;
        }
        let previous = std::mem::replace(&mut st.token_failure, failure.clone());
        match failure {
            None => self.events.report(
                INFO,
                TOKEN_REFRESH_RECOVERED,
                "The cloud token refreshes again",
                obj(vec![("previous", JsonValue::from(previous))]),
            ),
            Some(f) if previous.is_none() => {
                let level = if f == codes::REENROLL_REQUIRED {
                    ERROR
                } else {
                    WARN
                };
                self.events.report(
                    level,
                    TOKEN_REFRESH_FAILING,
                    &format!("The cloud token cannot be refreshed ({f})"),
                    obj(vec![("error_code", JsonValue::from(f.as_str()))]),
                );
            }
            Some(_) => {}
        }
    }

    fn check_backlog(&self, st: &mut State) -> Result<(), Box<Exception>> {
        let depth = (self.backlog_depth)()?;
        let threshold = i64::from(self.backlog_threshold);
        if !st.backlog && depth > threshold {
            st.backlog = true;
            self.events.report(
                WARN,
                UPLOAD_BACKLOG,
                &format!(
                    "{depth} batches are waiting to upload (threshold {})",
                    self.backlog_threshold
                ),
                obj(vec![
                    ("depth", JsonValue::from(depth)),
                    ("threshold", JsonValue::from(threshold)),
                ]),
            );
        } else if st.backlog && depth <= i64::from(self.backlog_threshold / 10) {
            st.backlog = false;
            self.events.report(
                INFO,
                UPLOAD_BACKLOG_DRAINED,
                &format!("The upload backlog drained ({depth} batches waiting)"),
                obj(vec![("depth", JsonValue::from(depth))]),
            );
        }
        Ok(())
    }
}
