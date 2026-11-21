//! Entry point of the capture library (`Strategies/CaptureEngine.cs`): one method per
//! capture job. The caller (agent host or [`CaptureScheduler`](super::scheduler::CaptureScheduler))
//! decides when to run each; every call is bounded by the poll budget and safe to repeat.

use std::sync::Arc;

use super::context::CaptureContext;
use super::error::Result;
use super::governor::CycleDeadline;
use super::key_repair::{KeyRepairCapture, KeyRepairList};
use super::ledger::{LedgerCapture, reopen_exclude, watermark_cursor};
use super::master::MasterScanCapture;
use super::open_check::{self, OpenCheckCapture};
use super::options::CaptureOptions;
use super::pipeline::blob_table;
use super::plan::CapturePlan;
use super::report::CaptureReport;
use super::sinks::{BatchSink, BlobSink, EchoSuppressor, NoEchoSuppressor};
use super::snapshot::{InitialSnapshotRunner, SnapshotResult, cursors_of};
use super::source::CaptureSource;
use super::state::{CaptureState, StateCommit};
use crate::config::time::{SharedClock, SystemClock};
use crate::contracts::catalog::SyncCatalog;
use crate::contracts::mapping::{CatalogRowMapper, MapperOptions};
use crate::util::ctx::Ctx;

/// Wire stream shared by every open-check table, so one check's changes stay in order.
pub const OPEN_STREAM: &str = open_check::STREAM;

/// Cursor holding the text revision the local capture state was built with (ADR 0123).
pub const TEXT_REVISION_CURSOR: &str = "mapper:text_rev";

/// Revision 1: text values lose their trailing spaces
/// ([`trim_fixed_width`](crate::contracts::mapping::transforms::trim_fixed_width)).
pub const TEXT_REVISION: &str = "1";

/// The capture engine.
pub struct CaptureEngine {
    cx: CaptureContext,
    plan: CapturePlan,
    reopen_exclude: Vec<String>,
    repairs: Option<Arc<dyn KeyRepairList>>,
}

impl std::fmt::Debug for CaptureEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureEngine").finish_non_exhaustive()
    }
}

impl CaptureEngine {
    /// An engine over `catalog`. `blob_sink: None` counts blobs as skipped; `echo: None`
    /// never suppresses; `clock: None` is the system clock.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        options: CaptureOptions,
        catalog: &SyncCatalog,
        source: Arc<dyn CaptureSource>,
        state: Arc<dyn CaptureState>,
        sink: Arc<dyn BatchSink>,
        blob_sink: Option<Arc<dyn BlobSink>>,
        echo: Option<Arc<dyn EchoSuppressor>>,
        clock: Option<SharedClock>,
    ) -> Result<CaptureEngine> {
        let plan = CapturePlan::build(catalog, &options)?;
        let mapper = CatalogRowMapper::new(
            catalog.clone(),
            MapperOptions::new((*options.site_time_zone).clone()),
        );
        let reopen_exclude = reopen_exclude(catalog, &plan);
        let cx = CaptureContext::new(
            options,
            source,
            state,
            sink,
            blob_sink,
            mapper,
            echo.unwrap_or_else(|| Arc::new(NoEchoSuppressor)),
            clock.unwrap_or_else(SystemClock::shared),
        );
        Ok(CaptureEngine {
            cx,
            plan,
            reopen_exclude,
            repairs: None,
        })
    }

    /// The queue of rows `sync.verify` asks to send again by key (ADR 0128); `None`, the
    /// default, keeps [`Self::run_key_repairs`] a no-op.
    pub fn with_key_repairs(mut self, repairs: Option<Arc<dyn KeyRepairList>>) -> CaptureEngine {
        self.repairs = repairs;
        self
    }

    /// The capture plan.
    pub fn plan(&self) -> &CapturePlan {
        &self.plan
    }

    fn deadline(&self) -> CycleDeadline {
        CycleDeadline::new(self.cx.options.budget.max_cycle_duration)
    }

    /// One open-check cycle.
    pub fn run_open_checks(&self, ctx: &Ctx) -> Result<CaptureReport> {
        OpenCheckCapture::new(&self.cx).run(&self.plan.open, ctx)
    }

    /// The ledger tail pass.
    pub fn run_ledger_tail(&self, ctx: &Ctx) -> Result<CaptureReport> {
        LedgerCapture::new(&self.cx, &self.reopen_exclude).tail(
            &self.plan.ledger,
            &self.deadline(),
            ctx,
        )
    }

    /// The ledger rescan pass.
    pub fn run_ledger_rescan(&self, ctx: &Ctx) -> Result<CaptureReport> {
        LedgerCapture::new(&self.cx, &self.reopen_exclude).rescan(
            &self.plan.ledger,
            &self.deadline(),
            ctx,
        )
    }

    /// The master scan.
    pub fn run_masters(&self, ctx: &Ctx) -> Result<CaptureReport> {
        MasterScanCapture::new(&self.cx).run(&self.plan.masters, ctx)
    }

    /// Sends again, or tombstones, the rows `sync.verify` queued by key (ADR 0128); nothing
    /// without a queue.
    pub fn run_key_repairs(&self, ctx: &Ctx) -> Result<CaptureReport> {
        match &self.repairs {
            None => Ok(CaptureReport::new()),
            Some(q) => KeyRepairCapture::new(&self.cx, &**q).run(&self.plan, &self.deadline(), ctx),
        }
    }

    /// Finishes the initial load of ledger tables that were skipped after a schema error (AD1).
    pub fn run_ledger_load_resume(&self, ctx: &Ctx) -> Result<CaptureReport> {
        InitialSnapshotRunner::new(&self.cx).resume_ledger(&self.plan, &self.deadline(), ctx)
    }

    /// Runs (or resumes) the initial load until it completes or the cycle budget is spent.
    pub fn run_initial_snapshot(&self, ctx: &Ctx) -> Result<SnapshotResult> {
        InitialSnapshotRunner::new(&self.cx).run(&self.plan, &self.deadline(), ctx)
    }

    /// Forgets what was sent for every planned table (row and blob shadows, open-check
    /// hashes and miss counters, snapshot progress). Ledger watermarks are kept. Safe to repeat.
    pub fn reset(&self, ctx: &Ctx) -> Result<()> {
        let state = &*self.cx.state;
        for p in self.plan.all() {
            state.reset_table(p.name(), ctx)?;
            state.reset_table(&blob_table(p.name()), ctx)?;
        }
        state.reset_table(open_check::OPEN_TABLE, ctx)?;
        let mut commit = StateCommit::default();
        for p in &self.plan.ledger {
            for c in cursors_of(p) {
                commit.cursors.insert(c, None);
            }
        }
        commit.cursors.insert(open_check::MISS_CURSOR, None);
        state.commit(&commit, ctx)
    }

    /// Every wire stream of the plan: [`OPEN_STREAM`] for the open-check tables, else the
    /// table name.
    pub fn streams(&self) -> Vec<String> {
        let mut s = Vec::new();
        if !self.plan.open.is_empty() {
            s.push(OPEN_STREAM.to_owned());
        }
        s.extend(
            self.plan
                .ledger
                .iter()
                .chain(&self.plan.masters)
                .map(|p| p.name().to_owned()),
        );
        s
    }

    /// [`Self::reset`] for the named streams only (ADR 0102). Returns `false`, resetting
    /// nothing, when a stream is not in the plan.
    pub fn reset_streams(&self, streams: &[String], ctx: &Ctx) -> Result<bool> {
        let open = streams.iter().any(|s| s == OPEN_STREAM) && !self.plan.open.is_empty();
        let tables: Vec<_> = self
            .plan
            .ledger
            .iter()
            .chain(&self.plan.masters)
            .filter(|p| streams.iter().any(|s| s == p.name()))
            .collect();
        let mut distinct: Vec<&String> = Vec::new();
        for s in streams {
            if !distinct.contains(&s) {
                distinct.push(s);
            }
        }
        if usize::from(open) + tables.len() != distinct.len() {
            return Ok(false);
        }
        let state = &*self.cx.state;
        let open_tables: Vec<_> = if open {
            self.plan.open.iter().collect()
        } else {
            Vec::new()
        };
        for p in tables.iter().chain(&open_tables) {
            state.reset_table(p.name(), ctx)?;
            state.reset_table(&blob_table(p.name()), ctx)?;
        }
        let mut commit = StateCommit::default();
        for p in self
            .plan
            .ledger
            .iter()
            .filter(|p| tables.iter().any(|t| std::ptr::eq(*t, *p)))
        {
            for c in cursors_of(p) {
                commit.cursors.insert(c, None);
            }
        }
        if open {
            state.reset_table(open_check::OPEN_TABLE, ctx)?;
            commit.cursors.insert(open_check::MISS_CURSOR, None);
        }
        state.commit(&commit, ctx)?;
        Ok(true)
    }

    /// `TextUpgradeStreamsAsync` (ADR 0123): `None` when the local state already has the
    /// current [`TEXT_REVISION`]. Otherwise the streams whose history must be sent again
    /// after the mapper's text form changed: the ledger streams that were ever loaded,
    /// because only their rows inside the rescan window are compared again. Masters and open
    /// checks are compared in full on every scan, so they correct themselves and keep their
    /// shadows (a master needs its old keys to send their tombstones). A state that never
    /// loaded a ledger table answers an empty list.
    pub fn text_upgrade_streams(&self, ctx: &Ctx) -> Result<Option<Vec<String>>> {
        let state = &*self.cx.state;
        if state.get_cursor(TEXT_REVISION_CURSOR, ctx)?.as_deref() == Some(TEXT_REVISION) {
            return Ok(None);
        }
        let mut loaded = Vec::new();
        for p in &self.plan.ledger {
            for c in cursors_of(p).into_iter().chain([watermark_cursor(p)]) {
                if state.get_cursor(&c, ctx)?.is_some() {
                    loaded.push(p.name().to_owned());
                    break;
                }
            }
        }
        Ok(Some(loaded))
    }

    /// `MarkTextRevisionAsync`: records that the local state follows the current
    /// [`TEXT_REVISION`].
    pub fn mark_text_revision(&self, ctx: &Ctx) -> Result<()> {
        let mut commit = StateCommit::default();
        commit
            .cursors
            .insert(TEXT_REVISION_CURSOR, Some(TEXT_REVISION.to_owned()));
        self.cx.state.commit(&commit, ctx)
    }
}
