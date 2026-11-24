//! `ChangeWatcher`: runs the configured catalog reads against the PMS on a timer and, when a
//! result's ETag changes, queues a tiny change notice (no data) for the cloud. The cloud then
//! refreshes only if someone is looking, and gets a delta.
//!
//! Parameters keep the .NET rules: exactly `{today}` becomes the hotel's date, a new day
//! forgets the last ETag, and the notice's parameter hash uses the spelling of the settings.
//! The executor gets the catalog spelling (the .NET dictionary matched names ignoring case).

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_time::Offset;
use dl_winsvc::StopSignal;

use crate::agent::clock::Clock;
use crate::agent::executor::{ExecError, Executor};
use crate::agent::health::Health;
use crate::agent::outbox::Outbox;
use crate::agent::settings::Watch;
use crate::contracts::{
    Catalog, Operation, OperationKind, Params, PmsChangeNotice, PmsRequest, delta, hasher,
    validator,
};

/// The shortest interval a watch runs at.
pub const MIN_INTERVAL: Duration = Duration::from_secs(5);
/// The parameter value replaced by the hotel date.
pub const TODAY: &str = "{today}";

/// A watch that names a catalog read.
#[derive(Debug, Clone)]
pub struct WatchRun {
    /// The read it runs.
    pub op: &'static Operation,
    /// Parameters as configured (spelling kept).
    pub parameters: Params,
    /// Time between runs.
    pub interval: Duration,
}

/// The watches that name a catalog read (others are ignored, as in .NET).
pub fn plan(catalog: &'static Catalog, watches: &[Watch]) -> Vec<WatchRun> {
    watches
        .iter()
        .filter_map(|w| {
            let op = catalog.get(&w.operation)?;
            (op.kind == OperationKind::Read).then(|| WatchRun {
                op,
                parameters: w.parameters.clone(),
                interval: Duration::from_secs(u64::try_from(w.interval_seconds).unwrap_or(0))
                    .max(MIN_INTERVAL),
            })
        })
        .collect()
}

/// `parameters` with every value exactly `{today}` replaced by `today`.
pub fn substitute(parameters: &Params, today: &str) -> Params {
    let mut out = Params::new();
    for (k, v) in parameters.iter() {
        out.insert(
            k,
            v.map(|v| {
                if v == TODAY {
                    today.to_string()
                } else {
                    v.to_string()
                }
            }),
        );
    }
    out
}

/// `parameters` under the catalog spelling of `op` (unknown names kept, for the validator to
/// refuse).
pub fn rekey(op: &Operation, parameters: &Params) -> Params {
    let mut out = Params::new();
    for (k, v) in parameters.iter() {
        let name = op.find_parameter(k).map_or(k, |p| p.name.as_str());
        out.insert(name, v.map(str::to_string));
    }
    out
}

/// What one run of a watch did.
#[derive(Debug, Clone, PartialEq)]
pub enum Tick {
    /// The result is as before (or this was the first run of the day).
    Unchanged,
    /// The result changed: this notice was queued.
    Changed(PmsChangeNotice),
    /// The watch can never run (the validator's reason); it stops.
    Misconfigured(String),
    /// The PMS call failed; the watch retries on its next tick.
    Failed(ExecError),
}

/// Last state of one watch.
#[derive(Debug, Default)]
pub struct WatchState {
    last_etag: Option<String>,
    last_day: Option<String>,
}

/// Runs the watches.
pub struct Watcher {
    executor: Arc<dyn Executor>,
    notices: Arc<Outbox<PmsChangeNotice>>,
    health: Arc<Health>,
    clock: Arc<dyn Clock>,
    offset: Offset,
    catalog: &'static Catalog,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("offset_s", &self.offset.whole_seconds())
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// A watcher queuing into `notices`; `offset` is the hotel's UTC offset.
    pub fn new(
        executor: Arc<dyn Executor>,
        notices: Arc<Outbox<PmsChangeNotice>>,
        health: Arc<Health>,
        clock: Arc<dyn Clock>,
        offset: Offset,
        catalog: &'static Catalog,
    ) -> Watcher {
        Watcher {
            executor,
            notices,
            health,
            clock,
            offset,
            catalog,
        }
    }

    /// The hotel's date, `yyyy-MM-dd`.
    pub fn today(&self) -> String {
        let local = self.clock.now().to_offset(self.offset);
        format!(
            "{:04}-{:02}-{:02}",
            local.year(),
            local.month().number(),
            local.day()
        )
    }

    /// One run of `run`.
    pub fn tick(&self, run: &WatchRun, state: &mut WatchState) -> Tick {
        let today = self.today();
        let configured = substitute(&run.parameters, &today);
        if state.last_day.as_deref() != Some(today.as_str()) {
            state.last_etag = None;
            state.last_day = Some(today);
        }
        let parameters = rekey(run.op, &configured);
        let request = PmsRequest {
            request_id: String::new(),
            operation: run.op.name.clone(),
            parameters,
            base_etag: None,
            idempotency_key: None,
            projection: None,
            background: false,
            deadline_ms: None,
        };
        if let Err(error) = validator::validate(self.catalog, &request) {
            return Tick::Misconfigured(error);
        }
        let started = Instant::now();
        let sets = match self.executor.execute(run.op, &request.parameters, None) {
            Ok(sets) => {
                self.health.pms_ok(started.elapsed());
                sets
            }
            Err(e) => {
                if let ExecError::Unavailable(m) = &e {
                    self.health.pms_failed(m);
                }
                return Tick::Failed(e);
            }
        };
        let rows: usize = sets.iter().map(Vec::len).sum();
        let (etag, _) = delta::full(sets);
        let changed = state.last_etag.as_ref().is_some_and(|last| *last != etag);
        state.last_etag = Some(etag.clone());
        if !changed {
            return Tick::Unchanged;
        }
        let notice = PmsChangeNotice {
            operation: run.op.name.clone(),
            parameters_hash: hasher::hash_parameters(configured.iter()),
            new_etag: etag,
            changed_rows: i32::try_from(rows).unwrap_or(i32::MAX),
        };
        if self.notices.push(notice.clone()) {
            dl_log::warn!("change notice queue full; the oldest notice was dropped");
        }
        Tick::Changed(notice)
    }

    /// Starts one thread per watch; each runs at once, then every interval, until `stop`.
    pub fn spawn(self: &Arc<Self>, runs: Vec<WatchRun>, stop: &StopSignal) -> Vec<JoinHandle<()>> {
        runs.into_iter()
            .filter_map(|run| {
                let (me, stop) = (Arc::clone(self), stop.clone());
                let name = run.op.name.clone();
                std::thread::Builder::new()
                    .name(format!("arrival-watch-{name}"))
                    .spawn(move || me.run(&run, &stop))
                    .map_err(|e| dl_log::error!("could not start a watch thread"; operation = %name, error = %e))
                    .ok()
            })
            .collect()
    }

    fn run(&self, run: &WatchRun, stop: &StopSignal) {
        let mut state = WatchState::default();
        loop {
            let started = Instant::now();
            match self.tick(run, &mut state) {
                Tick::Misconfigured(error) => {
                    dl_log::error!("Watch is misconfigured"; operation = %run.op.name, error = %error);
                    return;
                }
                Tick::Failed(e) => {
                    dl_log::warn!("Watch failed; will retry."; operation = %run.op.name, error = %e);
                }
                Tick::Unchanged | Tick::Changed(_) => {}
            }
            let wait = (started + run.interval).saturating_duration_since(Instant::now());
            if stop.wait_timeout(wait) {
                return;
            }
        }
    }
}
