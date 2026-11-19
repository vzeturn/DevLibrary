//! Site events as dl-agent problem reports (B5).
//!
//! The port reports its problems to the PDA cloud as site events (`SiteEventReporter`),
//! each written once to the service log with the marker `site_event=true` and its stable
//! `code` (`SQL_LOGIN_FAILED`, `SCHEMA_UNSUPPORTED`, `PRINT_RELAY_*`, ...). [`ModuleReports`]
//! is a log sink that sees those lines: every `Error` site event goes on to dl-agent's
//! hub as a problem report with the same code and message, at most once per code every
//! [`REPORT_EVERY`] (the hub needs the first one, not the flood). Warnings stay site
//! events only; the module's own state changes already report through `ctx.report`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::format::render_message;
use super::log::{Event, Level, Sink, Value};
use super::site_events::MARKER_PROPERTY;

/// The shortest interval between two reports of one code.
pub const REPORT_EVERY: Duration = Duration::from_secs(15 * 60);

/// Receives one problem report: code, message and context.
pub type ReportFn = Arc<dyn Fn(&str, &str, dl_json::Map) + Send + Sync>;

/// The sink that forwards `Error` site events to a [`ReportFn`].
pub struct ModuleReports {
    report: ReportFn,
    every: Duration,
    last: Mutex<HashMap<String, (Instant, u64)>>,
}

impl std::fmt::Debug for ModuleReports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleReports").finish_non_exhaustive()
    }
}

impl ModuleReports {
    /// Forwards to `report`, at most once per code every [`REPORT_EVERY`].
    pub fn new(report: ReportFn) -> ModuleReports {
        ModuleReports::with_interval(report, REPORT_EVERY)
    }

    /// Forwards to `report`, at most once per code every `every`.
    pub fn with_interval(report: ReportFn, every: Duration) -> ModuleReports {
        ModuleReports {
            report,
            every,
            last: Mutex::new(HashMap::new()),
        }
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

impl Sink for ModuleReports {
    fn emit(&self, e: &Event) {
        if e.level < Level::Error || e.property(MARKER_PROPERTY).is_none() {
            return;
        }
        let Some(code) = text(e.property("code")) else {
            return;
        };
        let now = Instant::now();
        let suppressed = {
            let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
            match last.get_mut(&code) {
                Some((at, skipped)) if now.duration_since(*at) < self.every => {
                    *skipped += 1;
                    return;
                }
                Some((at, skipped)) => {
                    *at = now;
                    std::mem::take(skipped)
                }
                None => {
                    last.insert(code.clone(), (now, 0));
                    0
                }
            }
        };
        let mut context = dl_json::Map::new();
        context.insert("source", "site_event");
        if let Some(id) = text(e.property("event_id")) {
            context.insert("event_id", id);
        }
        if suppressed > 0 {
            context.insert("suppressed", suppressed);
        }
        (self.report)(&code, &render_message(e), context);
    }
}

/// Sends every event to each of its sinks, in order.
pub struct TeeSink(pub Vec<Arc<dyn Sink>>);

impl Sink for TeeSink {
    fn emit(&self, e: &Event) {
        for s in &self.0 {
            s.emit(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::time::Timestamp;
    use crate::reporting::log::Property;

    type Seen = Arc<Mutex<Vec<(String, String, dl_json::Map)>>>;

    fn sink(every: Duration) -> (ModuleReports, Seen) {
        let seen: Seen = Arc::default();
        let s = seen.clone();
        let report: ReportFn = Arc::new(move |code, message, context| {
            s.lock()
                .unwrap()
                .push((code.into(), message.into(), context));
        });
        (ModuleReports::with_interval(report, every), seen)
    }

    fn event(level: Level, code: &str, marker: bool) -> Event {
        let mut e = Event::new(
            Timestamp::from_unix_seconds(1_794_729_600),
            level,
            "SQL login failed for pda_sync",
        );
        e.properties.push(Property::new("code", code));
        if marker {
            e.properties.push(Property::new(MARKER_PROPERTY, true));
            e.properties.push(Property::new("event_id", "0190-ev"));
        }
        e
    }

    #[test]
    fn an_error_site_event_becomes_one_report_with_its_code_and_message() {
        let (s, seen) = sink(REPORT_EVERY);
        s.emit(&event(Level::Error, "SQL_LOGIN_FAILED", true));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "SQL_LOGIN_FAILED");
        assert_eq!(seen[0].1, "SQL login failed for pda_sync");
        assert_eq!(
            seen[0].2.get("event_id").and_then(dl_json::Value::as_str),
            Some("0190-ev")
        );
    }

    #[test]
    fn warnings_and_plain_log_errors_are_not_reported() {
        let (s, seen) = sink(REPORT_EVERY);
        s.emit(&event(Level::Warning, "SQL_LOGIN_FAILED", true));
        s.emit(&event(Level::Error, "SQL_LOGIN_FAILED", false));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_repeated_code_is_reported_once_per_interval_with_the_count_it_held_back() {
        let (s, seen) = sink(REPORT_EVERY);
        for _ in 0..3 {
            s.emit(&event(Level::Error, "SQL_LOGIN_FAILED", true));
        }
        s.emit(&event(Level::Error, "PRINT_RELAY_FAILED", true));
        assert_eq!(seen.lock().unwrap().len(), 2, "one per code");

        let (s, seen) = sink(Duration::ZERO);
        s.emit(&event(Level::Error, "SQL_LOGIN_FAILED", true));
        s.emit(&event(Level::Error, "SQL_LOGIN_FAILED", true));
        assert_eq!(seen.lock().unwrap().len(), 2, "again after the interval");
    }

    #[test]
    fn the_tee_feeds_every_sink() {
        let (a, seen_a) = sink(REPORT_EVERY);
        let (b, seen_b) = sink(REPORT_EVERY);
        TeeSink(vec![Arc::new(a), Arc::new(b)]).emit(&event(Level::Error, "X_CODE", true));
        assert_eq!(seen_a.lock().unwrap().len(), 1);
        assert_eq!(seen_b.lock().unwrap().len(), 1);
    }
}
