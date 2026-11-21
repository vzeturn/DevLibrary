//! The structured site events of a run (ADR 0135): sources `legacy-export` and
//! `legacy-upload`, codes `LEGACY_EXPORT_STARTED`, `LEGACY_EXPORT_FAILED` and
//! `LEGACY_EXPORT_UPLOADED`, each carrying the run id. Details hold counts, names, codes and
//! hashes; never row data, guest names or invoice values.

use std::sync::{Arc, Mutex, PoisonError};

use dl_json::Map;

use super::payload::event_codes;
use super::sources::ExportError;
use crate::config::time::SharedClock;
use crate::contracts::wire::site_event_levels;
use crate::reporting::site_event_store::SiteEventStore;
use crate::reporting::site_events::{SiteEventReporter, SiteEvents};
use crate::reporting::{Logger, scrub};

/// Where the run reports (`ISiteEvents` with the source chosen per event).
pub trait LegacyEvents: Send + Sync {
    /// Queues one event of `source` and logs it. Never fails.
    fn report(&self, source: &str, level: &str, code: &str, message: &str, detail: Map);
}

/// The events of one run, by run id.
pub type EventsFactory = Arc<dyn Fn(&str) -> Arc<dyn LegacyEvents> + Send + Sync>;

/// The factory of the running agent: reporters over the site event queue, or, when no queue
/// of its own can be opened, the host's reporter.
pub fn queue_events_factory(
    queue: Option<Arc<dyn SiteEventStore>>,
    fallback: Arc<dyn SiteEvents>,
    log: Logger,
    clock: SharedClock,
) -> EventsFactory {
    Arc::new(move |run_id: &str| {
        let own = queue
            .as_ref()
            .and_then(|q| ReporterEvents::new(q.clone(), run_id, &log, clock.clone()).ok());
        match own {
            Some(r) => Arc::new(r) as Arc<dyn LegacyEvents>,
            None => Arc::new(FallbackEvents(fallback.clone())),
        }
    })
}

/// The reporters of one run: one per source, sharing the site event queue.
pub struct ReporterEvents {
    export: SiteEventReporter,
    upload: SiteEventReporter,
}

impl ReporterEvents {
    /// Reporters for `run_id` writing to `queue` and `log` (the root logger).
    pub fn new(
        queue: Arc<dyn SiteEventStore>,
        run_id: &str,
        log: &Logger,
        clock: SharedClock,
    ) -> std::result::Result<ReporterEvents, String> {
        Ok(ReporterEvents {
            export: SiteEventReporter::new(
                queue.clone(),
                event_codes::SOURCE_EXPORT,
                Some(run_id),
                log,
                clock.clone(),
            )?,
            upload: SiteEventReporter::new(
                queue,
                event_codes::SOURCE_UPLOAD,
                Some(run_id),
                log,
                clock,
            )?,
        })
    }
}

impl LegacyEvents for ReporterEvents {
    fn report(&self, source: &str, level: &str, code: &str, message: &str, detail: Map) {
        let reporter = if source == event_codes::SOURCE_UPLOAD {
            &self.upload
        } else {
            &self.export
        };
        reporter.report(level, code, message, Some(detail));
    }
}

/// When no queue of its own can be opened: the host's reporter (source `agent`) carries the
/// event and the intended source goes into the detail.
pub struct FallbackEvents(pub Arc<dyn SiteEvents>);

impl LegacyEvents for FallbackEvents {
    fn report(&self, source: &str, level: &str, code: &str, message: &str, mut detail: Map) {
        detail.insert("event_source", source);
        self.0.report(level, code, message, Some(detail));
    }
}

/// One recorded event.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedEvent {
    /// The source.
    pub source: String,
    /// The level.
    pub level: String,
    /// The code.
    pub code: String,
    /// The message.
    pub message: String,
    /// The detail.
    pub detail: Map,
}

/// Keeps the events in memory (tests, and a run without a queue).
#[derive(Default)]
pub struct MemoryEvents {
    events: Mutex<Vec<RecordedEvent>>,
}

impl MemoryEvents {
    /// Everything reported so far.
    pub fn events(&self) -> Vec<RecordedEvent> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The codes reported so far, in order.
    pub fn codes(&self) -> Vec<String> {
        self.events().into_iter().map(|e| e.code).collect()
    }
}

impl LegacyEvents for MemoryEvents {
    fn report(&self, source: &str, level: &str, code: &str, message: &str, detail: Map) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RecordedEvent {
                source: source.to_owned(),
                level: level.to_owned(),
                code: code.to_owned(),
                message: message.to_owned(),
                detail,
            });
    }
}

/// The run's reporter: every event carries the run id.
pub struct RunEvents {
    sink: Arc<dyn LegacyEvents>,
    run_id: String,
}

impl RunEvents {
    /// Events of `run_id` into `sink`.
    pub fn new(sink: Arc<dyn LegacyEvents>, run_id: &str) -> RunEvents {
        RunEvents {
            sink,
            run_id: run_id.to_owned(),
        }
    }

    /// The run id.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn emit(&self, source: &str, level: &str, code: &str, message: &str, mut detail: Map) {
        detail.insert("run_id", self.run_id.as_str());
        self.sink
            .report(source, level, code, &scrub(message), detail);
    }

    /// A progress or outcome event of the export stages.
    pub fn export(&self, level: &str, code: &str, message: &str, detail: Map) {
        self.emit(event_codes::SOURCE_EXPORT, level, code, message, detail);
    }

    /// The run began (source `legacy-export`).
    pub fn started(&self, mode: &str, source: &str, commit: bool, vietqr_only: bool) {
        let mut d = Map::new();
        d.insert("mode", mode);
        d.insert("store", source);
        d.insert("commit", commit);
        d.insert("vietqr_only", vietqr_only);
        self.export(
            site_event_levels::INFO,
            event_codes::STARTED,
            "Legacy export started by a cloud command",
            d,
        );
    }

    /// A stage failed (never row data). The source follows the stage.
    pub fn failed(&self, e: &ExportError) {
        let mut d = Map::new();
        d.insert("stage", e.stage);
        d.insert("error_code", e.code.as_str());
        if let Some(t) = &e.table {
            d.insert("table", t.as_str());
        }
        if let Some(n) = e.sql_number {
            d.insert("sql_number", n);
        }
        let upload = matches!(
            e.stage,
            super::sources::stages::ZIP
                | super::sources::stages::DRY_RUN
                | super::sources::stages::COMMIT
                | super::sources::stages::POST_COMMIT
        );
        let source = if upload {
            event_codes::SOURCE_UPLOAD
        } else {
            event_codes::SOURCE_EXPORT
        };
        let level = if e.refusal {
            site_event_levels::WARN
        } else {
            site_event_levels::ERROR
        };
        self.emit(
            source,
            level,
            event_codes::FAILED,
            &format!("Legacy export failed at {}: {}", e.stage, e.message),
            d,
        );
    }

    /// The final upload finished and the local bundle was deleted (source `legacy-upload`).
    pub fn uploaded(&self, detail: Map) {
        self.emit(
            event_codes::SOURCE_UPLOAD,
            site_event_levels::INFO,
            event_codes::UPLOADED,
            "Legacy bundle uploaded and the local copy deleted",
            detail,
        );
    }

    /// A progress event of the upload (source `legacy-upload`).
    pub fn upload_progress(&self, level: &str, code: &str, message: &str, detail: Map) {
        self.emit(event_codes::SOURCE_UPLOAD, level, code, message, detail);
    }
}
