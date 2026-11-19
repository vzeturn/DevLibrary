//! The [`Sink`] of a running module: state, metrics and problem reports go to the
//! supervisor's [`ModuleContext`]; warnings and worse also go to the problem journal the
//! hello carries, once one is attached; answered cloud exchanges feed the supervisor's
//! cloud-link watchdog once [`CtxSink::link_to`] named the cloud.

use std::sync::{Arc, Mutex, PoisonError};

use dl_agent::report::Severity;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};

use crate::agent::health::Sink;
use crate::agent::problems::ProblemJournal;

/// Hands the link's health to the supervisor.
pub struct CtxSink {
    ctx: Mutex<ModuleContext>,
    journal: Mutex<Option<Arc<ProblemJournal>>>,
    /// The cloud base URL the link polls (`None`: not linked, or stopped on purpose).
    cloud: Mutex<Option<String>>,
}

impl std::fmt::Debug for CtxSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxSink").finish_non_exhaustive()
    }
}

impl CtxSink {
    /// A sink for `ctx`.
    pub fn new(ctx: ModuleContext) -> CtxSink {
        CtxSink {
            ctx: Mutex::new(ctx),
            journal: Mutex::new(None),
            cloud: Mutex::new(None),
        }
    }

    /// Records warnings and worse into `journal` from now on.
    pub fn attach(&self, journal: Arc<ProblemJournal>) {
        *self.journal.lock().unwrap_or_else(PoisonError::into_inner) = Some(journal);
    }

    /// The link starts polling `cloud_url`: the supervisor watches it from now on (no
    /// answer yet, so `ok_at` is 0).
    pub fn link_to(&self, cloud_url: &str) {
        *self.cloud.lock().unwrap_or_else(PoisonError::into_inner) = Some(cloud_url.to_string());
        self.with(|c| c.set_cloud_link(cloud_url, 0));
    }

    /// The link stopped polling on purpose: nothing to watch any more.
    pub fn unlink(&self) {
        *self.cloud.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.with(ModuleContext::clear_cloud_link);
    }

    fn with(&self, f: impl FnOnce(&ModuleContext)) {
        f(&self.ctx.lock().unwrap_or_else(PoisonError::into_inner));
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
                .journal
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(j) = journal {
                j.record(level, "arrival", message, Some(code));
            }
        }
        self.with(|c| c.report(severity, code, message, context));
    }

    fn cloud_link_ok(&self, unix_seconds: u64) {
        let cloud = self
            .cloud
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(url) = cloud {
            self.with(|c| c.set_cloud_link(&url, unix_seconds));
        }
    }
}
