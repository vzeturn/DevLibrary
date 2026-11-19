//! The [`Sink`] of a running module: state, metrics and problem reports go to the
//! supervisor's [`ModuleContext`]; warnings and worse also go to the problem journal the
//! hello carries, once one is attached.

use std::sync::{Arc, Mutex, PoisonError};

use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use crate::agent::health::Sink;
use crate::agent::problems::ProblemJournal;

/// Hands the link's health to the supervisor.
pub struct CtxSink(Mutex<ModuleContext>, Mutex<Option<Arc<ProblemJournal>>>);

impl std::fmt::Debug for CtxSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxSink").finish_non_exhaustive()
    }
}

impl CtxSink {
    /// A sink for `ctx`.
    pub fn new(ctx: ModuleContext) -> CtxSink {
        CtxSink(Mutex::new(ctx), Mutex::new(None))
    }

    /// Records warnings and worse into `journal` from now on.
    pub fn attach(&self, journal: Arc<ProblemJournal>) {
        *self.1.lock().unwrap_or_else(PoisonError::into_inner) = Some(journal);
    }

    fn with(&self, f: impl FnOnce(&ModuleContext)) {
        f(&self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

impl Sink for CtxSink {
    fn running(&self, detail: &str) {
        self.with(|c| c.set_running(detail));
    }

    fn degraded(&self, detail: &str) {
        self.with(|c| c.set_degraded(detail));
    }

    fn metric(&self, key: &str, value: Value) {
        self.with(|c| c.set_metric(key, value));
    }

    fn report(&self, severity: Severity, code: &str, message: &str, context: Map) {
        let level = match severity {
            Severity::Info => None,
            Severity::Warning => Some("Warning"),
            Severity::Error => Some("Error"),
            Severity::Critical => Some("Critical"),
        };
        if let Some(level) = level {
            let journal = self
                .1
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(j) = journal {
                j.record(level, "arrival", message, Some(code));
            }
        }
        self.with(|c| c.report(severity, code, message, context));
    }
}
