//! The log sink that turns every `Warning`+ line of the service into a site event (port
//! of `Reporting/SiteEventLogBridge.cs`).
//!
//! The same code and message template within [`DEDUP_WINDOW`] collapse into one event plus
//! a later `repeats` summary (carrying the detail of the last occurrence); at most
//! [`PER_MINUTE`] events a minute pass, the rest are summarised as one
//! `EVENTS_SUPPRESSED`. Lines the reporting code writes (its own context, a flush in
//! progress, an event's own log line) are ignored, so a reporting failure cannot feed back.

use std::cell::Cell;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dl_json::{Map, Number, Value as JsonValue};

use super::agent_logging::REPORTING_NAMESPACE;
use super::format::{as_text, field_name, is_code, render_message};
use super::log::{Event, Level, SOURCE_CONTEXT, Sink, Value};
use super::site_events::{MARKER_PROPERTY, SiteEvents, truncate_utf16};
use super::sql_error;
use crate::config::time::SharedClock;
use crate::contracts::wire::site_event_levels;

/// Code of a warning line.
pub const WARNING_CODE: &str = "LOG_WARNING";
/// Code of an error line.
pub const ERROR_CODE: &str = "LOG_ERROR";
/// Code of the suppression summary.
pub const SUPPRESSED_CODE: &str = "EVENTS_SUPPRESSED";
/// Repeats of a line within this window are one event plus a summary.
pub const DEDUP_WINDOW: Duration = Duration::from_secs(300);
/// Events a minute.
pub const PER_MINUTE: i32 = 60;
const MAX_BUFFERED: usize = 200;
const MAX_DETAIL_STRING: usize = 200;
const MINUTE_NANOS: i128 = 60_000_000_000;

/// Properties the event or the batch already carries.
const LIFTED: [&str; 9] = [
    "Code", "code", "source", "run_id", "RunId", "agent_id", "AgentId", "tenant", "TenantId",
];

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Restores the previous quiet state when dropped.
#[derive(Debug)]
pub struct SuppressGuard {
    previous: bool,
}

impl Drop for SuppressGuard {
    fn drop(&mut self) {
        QUIET.with(|q| q.set(self.previous));
    }
}

/// Within the returned scope (on this thread) no log line becomes an event (`Suppress`).
pub fn suppress() -> SuppressGuard {
    let previous = QUIET.with(|q| q.replace(true));
    SuppressGuard { previous }
}

#[derive(Debug, Clone)]
struct Pending {
    level: &'static str,
    code: String,
    message: String,
    detail: Option<Map>,
}

#[derive(Debug)]
struct Repeat {
    code: String,
    since: i128,
    level: &'static str,
    last_message: String,
    last_detail: Map,
    count: i64,
}

#[derive(Default)]
struct Inner {
    recent: Vec<(String, Repeat)>,
    buffer: Vec<Pending>,
    events: Option<Arc<dyn SiteEvents>>,
    /// `None`: `DateTimeOffset.MinValue`.
    minute_start: Option<i128>,
    minute_count: i32,
    suppressed: i64,
}

impl Inner {
    fn admit(&mut self, p: Pending, due: &mut Vec<Pending>) {
        if self.minute_count >= PER_MINUTE {
            self.suppressed += 1;
            return;
        }
        self.minute_count += 1;
        due.push(p);
    }

    /// Closes the rate window (one suppression summary) and the dedup windows (one repeat
    /// summary each).
    fn expire(&mut self, now: i128, due: &mut Vec<Pending>, force: bool) {
        if force || self.minute_start.is_none_or(|s| now - s >= MINUTE_NANOS) {
            if self.suppressed > 0 {
                let mut detail = Map::new();
                detail.insert("suppressed", self.suppressed);
                detail.insert("window_seconds", 60);
                due.push(Pending {
                    level: site_event_levels::WARN,
                    code: SUPPRESSED_CODE.to_owned(),
                    message: format!(
                        "{} log events were not reported: more than {PER_MINUTE} a minute",
                        self.suppressed
                    ),
                    detail: Some(detail),
                });
            }
            self.minute_start = Some(now);
            self.minute_count = 0;
            self.suppressed = 0;
        }
        let window = DEDUP_WINDOW.as_nanos() as i128;
        let mut i = 0;
        while i < self.recent.len() {
            if !(force || now - self.recent[i].1.since >= window) {
                i += 1;
                continue;
            }
            let (_, r) = self.recent.remove(i);
            if r.count == 0 {
                continue;
            }
            let mut detail = r.last_detail;
            detail.insert("repeats", r.count);
            detail.insert("window_seconds", 300);
            let p = Pending {
                level: r.level,
                code: r.code,
                message: format!(
                    "{} (repeated {} more times in 5 min)",
                    r.last_message, r.count
                ),
                detail: Some(detail),
            };
            self.admit(p, due);
        }
    }
}

/// The bridge (`SiteEventLogBridge`): a [`Sink`] of the service logger from `Warning` up.
pub struct SiteEventLogBridge {
    clock: SharedClock,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for SiteEventLogBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteEventLogBridge").finish_non_exhaustive()
    }
}

fn cap(s: &str) -> String {
    truncate_utf16(s, MAX_DETAIL_STRING).to_owned()
}

fn code_of(e: &Event) -> String {
    for name in ["Code", "code"] {
        if let Some(Value::Str(s)) = e.property(name)
            && is_code(s)
        {
            return s.clone();
        }
    }
    if e.level >= Level::Error {
        ERROR_CODE
    } else {
        WARNING_CODE
    }
    .to_owned()
}

/// Scalar properties (no exception message, which can carry data), the logger, the
/// exception type and, for a SQL Server error, its facts (`DetailOf`).
fn detail_of(e: &Event) -> Map {
    let mut detail = Map::new();
    for p in &e.properties {
        if !p.value.is_scalar() || LIFTED.contains(&p.name.as_str()) {
            continue;
        }
        let v = match &p.value {
            Value::Null => JsonValue::Null,
            Value::Bool(b) => JsonValue::Bool(*b),
            Value::Int(n) => JsonValue::from(*n),
            Value::Float(f) if f.is_finite() => crate::util::stj::f64_text(*f)
                .and_then(|t| Number::from_raw(&t))
                .map_or_else(|| JsonValue::from(*f), JsonValue::Number),
            Value::Decimal(s) => {
                Number::from_raw(s).map_or_else(|| JsonValue::from(cap(s)), JsonValue::Number)
            }
            other => JsonValue::from(cap(&as_text(other))),
        };
        detail.insert(field_name(&p.name), v);
    }
    if let Some(ex) = &e.exception {
        detail.insert("exception_type", ex.type_name.as_str());
    }
    sql_error::add_to(&mut detail, e.exception.as_ref());
    detail
}

impl SiteEventLogBridge {
    /// A bridge buffering until [`Self::attach`].
    pub fn new(clock: SharedClock) -> SiteEventLogBridge {
        SiteEventLogBridge {
            clock,
            inner: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> i128 {
        self.clock.now().unix_nanos()
    }

    /// Starts reporting; lines logged before this are reported now (up to a small buffer).
    pub fn attach(&self, events: Arc<dyn SiteEvents>) {
        let buffered = {
            let mut inner = self.lock();
            inner.events = Some(events);
            std::mem::take(&mut inner.buffer)
        };
        self.report(buffered);
    }

    /// Called by the service loop: emits repeat and suppression summaries whose window
    /// has closed.
    pub fn tick(&self) {
        let mut due = Vec::new();
        self.lock().expire(self.now(), &mut due, false);
        self.report(due);
    }

    /// At the end of a run: emits every pending repeat and suppression summary now.
    pub fn drain(&self) {
        let mut due = Vec::new();
        self.lock().expire(self.now(), &mut due, true);
        self.report(due);
    }

    fn report(&self, due: Vec<Pending>) {
        if due.is_empty() {
            return;
        }
        let events = {
            let mut inner = self.lock();
            match inner.events.clone() {
                Some(e) => e,
                None => {
                    for p in due {
                        if inner.buffer.len() < MAX_BUFFERED {
                            inner.buffer.push(p);
                        } else {
                            inner.suppressed += 1;
                        }
                    }
                    return;
                }
            }
        };
        for p in due {
            events.report(p.level, &p.code, &p.message, p.detail);
        }
    }
}

impl Sink for SiteEventLogBridge {
    fn emit(&self, e: &Event) {
        if QUIET.with(Cell::get) || e.level < Level::Warning {
            return;
        }
        if e.property(MARKER_PROPERTY).is_some() {
            return;
        }
        if let Some(ctx) = e.property(SOURCE_CONTEXT)
            && as_text(ctx).starts_with(REPORTING_NAMESPACE)
        {
            return;
        }
        let code = code_of(e);
        let level = if e.level >= Level::Error {
            site_event_levels::ERROR
        } else {
            site_event_levels::WARN
        };
        let message = render_message(e);
        let key = format!("{code}\n{}", e.template.text());
        let detail = detail_of(e);
        let now = self.now();
        let mut due = Vec::new();
        {
            let mut inner = self.lock();
            inner.expire(now, &mut due, false);
            if let Some((_, seen)) = inner.recent.iter_mut().find(|(k, _)| *k == key) {
                seen.count += 1;
                seen.last_message = message;
                seen.last_detail = detail;
                if level == site_event_levels::ERROR {
                    seen.level = level;
                }
            } else {
                let p = Pending {
                    level,
                    code: code.clone(),
                    message: message.clone(),
                    detail: Some(detail.clone()),
                };
                inner.recent.push((
                    key,
                    Repeat {
                        code,
                        since: now,
                        level,
                        last_message: message,
                        last_detail: detail,
                        count: 0,
                    },
                ));
                inner.admit(p, &mut due);
            }
        }
        self.report(due);
    }
}
