//! The collector thread: a cycle at once, then one per poll interval (`Runner.Run`), plus
//! `einvoice.collect_now`. Each cycle's outcome becomes counters, problem reports and the
//! module's health.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use dl_agent::report::Severity;
use dl_agent::sql::transport::Rung;
use dl_agent::supervisor::ModuleContext;
use dl_json::{Map, Value};
use dl_winsvc::StopSignal;

use crate::agent_config::AgentConfig;
use crate::log::{self, V};
use crate::profile::PosItemName;
use crate::runner::{CycleResult, Runner};
use crate::sql::StepDown;
use crate::stats::Stats;

/// The last cycle's summary in the module's data directory, read by the self-test (which
/// may run in another process, `dl-agent selftest`).
pub const LAST_CYCLE_FILE: &str = "last_cycle.json";

/// How often a waiting thread looks at its triggers.
const TICK: Duration = Duration::from_millis(250);
/// Failed hub-config refreshes in a row before it is reported.
const CONFIG_STALE_REPORT_AFTER: u32 = 5;

/// One collector per process: a collector left behind by a stop that timed out finishes
/// its cycle before a new one touches the outbox.
static COLLECTOR_BUSY: Mutex<()> = Mutex::new(());
/// One relay worker per process ("one call at a time", ADR 0016).
pub(crate) static RELAY_BUSY: Mutex<()> = Mutex::new(());

/// Takes `lock`, waiting while an older worker still holds it; `None` when stopped first.
pub(crate) fn exclusive(
    lock: &'static Mutex<()>,
    stop: &StopSignal,
) -> Option<MutexGuard<'static, ()>> {
    loop {
        match lock.try_lock() {
            Ok(g) => return Some(g),
            // The previous holder panicked; the lock only orders workers, it guards no data.
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if stop.wait_timeout(TICK) {
                    return None;
                }
            }
        }
    }
}

/// State the worker threads share with the module thread.
#[derive(Debug, Default)]
pub struct Shared {
    /// Counters published as metrics.
    pub stats: Arc<Stats>,
    /// Set by `einvoice.collect_now`, cleared when the cycle starts.
    pub collect_now: AtomicBool,
    /// The platform said this install was replaced by an agent on another machine.
    pub replaced: AtomicBool,
    /// The last cycle's main problem (`None`: it went well).
    pub problem: Mutex<Option<String>>,
    /// What the collector runs with after the hub's config: the dry run builds with it.
    pub effective: Mutex<Option<Effective>>,
    /// How each source reaches SQL Server ([`crate::sql::Sql::describe`]), as of the last cycle.
    pub sql: Mutex<Value>,
}

/// The collector's config after the hub's refresh, and its POS item-name profile.
#[derive(Debug, Clone)]
pub struct Effective {
    /// The sealed config with the hub's collect-from, sources and recalc applied.
    pub cfg: AgentConfig,
    /// The hub's `collect_profile.pos_item_name`.
    pub pos_item_name: PosItemName,
}

impl Shared {
    /// The last cycle's main problem.
    pub fn problem(&self) -> Option<String> {
        self.problem
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_problem(&self, p: Option<String>) {
        *self.problem.lock().unwrap_or_else(PoisonError::into_inner) = p;
    }

    /// The collector's effective config (`None` before the module started one).
    pub fn effective(&self) -> Option<Effective> {
        self.effective
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Records the collector's effective config.
    pub fn set_effective(&self, e: Effective) {
        *self
            .effective
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(e);
    }

    /// How each source reaches SQL Server, as of the last cycle.
    pub fn sql(&self) -> Value {
        self.sql
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The collect / push loop (and write-back, unless a `WritebackWorker`
/// took it over). The runner is shared with that worker, which locks it only to apply.
pub struct Collector {
    runner: Arc<Mutex<Runner>>,
    interval: Duration,
    stop: StopSignal,
    shared: Arc<Shared>,
    ctx: ModuleContext,
}

impl Collector {
    /// A collector running `runner` every `interval` until `stop`.
    pub fn new(
        runner: Arc<Mutex<Runner>>,
        interval: Duration,
        stop: StopSignal,
        shared: Arc<Shared>,
        ctx: ModuleContext,
    ) -> Collector {
        Collector {
            runner,
            interval,
            stop,
            shared,
            ctx,
        }
    }

    /// Runs until stopped or replaced.
    pub fn run(self) {
        let Some(_one) = exclusive(&COLLECTOR_BUSY, &self.stop) else {
            return;
        };
        let mut next = Instant::now();
        while !self.stop.is_stopped() {
            let due = Instant::now() >= next;
            if due || self.shared.collect_now.swap(false, Ordering::Relaxed) {
                let mut runner = self.runner.lock().unwrap_or_else(PoisonError::into_inner);
                let res = runner.run_cycle();
                self.record(&mut runner, &res);
                drop(runner);
                if res.replaced() {
                    self.shared.replaced.store(true, Ordering::Relaxed);
                    return;
                }
                if due {
                    // A ticker: the next tick is due one interval after the previous one.
                    next += self.interval;
                    if next < Instant::now() {
                        next = Instant::now() + self.interval;
                    }
                }
            }
            let wait = next.saturating_duration_since(Instant::now()).min(TICK);
            self.stop.wait_timeout(wait);
        }
    }

    fn record(&self, runner: &mut Runner, res: &CycleResult) {
        let s = &self.shared.stats;
        Stats::bump(&s.cycles, 1);
        Stats::bump(&s.read_total, count(res.read));
        Stats::bump(&s.pushed_total, count(res.pushed));
        Stats::bump(&s.errored_total, count(res.errored));
        Stats::set(&s.outbox_pending, runner.outbox.len() as u64);
        let now = dl_agent::now();
        Stats::set(&s.last_cycle_at, now);
        if res.pushed + res.errored > 0 {
            Stats::set(&s.last_push_at, now);
        }
        for step in runner.sql.take_step_downs() {
            report_step_down(&self.ctx, &step);
        }
        self.shared.set_effective(Effective {
            cfg: runner.cfg.clone(),
            pos_item_name: runner.sql.pos_item_name(),
        });
        *self
            .shared
            .sql
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = runner.sql.describe();
        let problem = report_cycle(&self.ctx, res, runner.config_stale_streak());
        write_last_cycle(&self.ctx, res, now, problem.as_deref());
        self.shared.set_problem(problem);
    }
}

/// Reports a connection that had to step down the transport ladder at runtime: the server
/// started resetting the login handshake the sealed (or pinned) rung used.
pub fn report_step_down(ctx: &ModuleContext, s: &StepDown) {
    let clear = s.to == Rung::Unencrypted;
    let msg = format!(
        "{}: SQL Server reset the login at transport {} ({}); now connected with {}{}",
        s.database,
        s.from.name(),
        s.cause,
        s.to.name(),
        if clear {
            ": the SQL login travels unencrypted; install the TLS 1.2 update on SQL Server"
        } else {
            ""
        }
    );
    let mut m = Map::new();
    m.insert("database", s.database.as_str());
    m.insert("from", s.from.name());
    m.insert("to", s.to.name());
    m.insert("cause", s.cause.as_str());
    m.insert("unencrypted", clear);
    ctx.report(Severity::Warning, "einvoice_sql_transport", &msg, m);
    log::warn(
        "einvoice SQL transport stepped down",
        &[
            ("database", V::S(&s.database)),
            ("from", V::S(s.from.name())),
            ("to", V::S(s.to.name())),
            ("cause", V::S(&s.cause)),
        ],
    );
}

/// Writes [`LAST_CYCLE_FILE`] (temp file + rename); a failure is logged, never fatal.
fn write_last_cycle(ctx: &ModuleContext, res: &CycleResult, at: u64, problem: Option<&str>) {
    let mut m = Map::new();
    m.insert("at", at);
    m.insert("read", res.read);
    m.insert("pushed", res.pushed);
    m.insert("errored", res.errored);
    m.insert("applied", res.applied);
    m.insert("problem", problem.map_or(Value::Null, Value::from));
    let path = ctx.data_dir().join(LAST_CYCLE_FILE);
    let tmp = path.with_extension("json.tmp");
    let body = dl_json::to_string(&Value::Object(m));
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &path)) {
        log::warn(
            "einvoice could not record the last cycle for the self-test",
            &[
                ("path", V::S(&path.display().to_string())),
                ("error", V::S(&e.to_string())),
            ],
        );
    }
}

fn count(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn with_count(n: usize) -> Map {
    let mut m = Map::new();
    m.insert("count", n as u64);
    m
}

/// Turns a cycle's failures into problem reports; returns the one that degrades the module.
pub fn report_cycle(ctx: &ModuleContext, res: &CycleResult, config_stale: u32) -> Option<String> {
    if res.replaced() {
        return None;
    }
    if let Some(e) = &res.push_failed {
        let mut m = Map::new();
        m.insert("queued", count(res.remaining));
        ctx.report(Severity::Error, "einvoice_push_failed", e, m);
    }
    if let Some(e) = res.sql_errors.first() {
        ctx.report(
            Severity::Error,
            "einvoice_sql_failed",
            e,
            with_count(res.sql_errors.len()),
        );
    }
    if let Some(e) = res.outbox_errors.first() {
        ctx.report(
            Severity::Error,
            "einvoice_outbox_failed",
            e,
            with_count(res.outbox_errors.len()),
        );
    }
    if let Some(e) = res.item_errors.first() {
        ctx.report(
            Severity::Warning,
            "einvoice_collect_failed",
            e,
            with_count(res.item_errors.len()),
        );
    }
    if let Some(e) = &res.poll_failed {
        ctx.report(
            Severity::Warning,
            "einvoice_writeback_failed",
            e,
            Map::new(),
        );
    }
    if res.errored > 0 {
        ctx.report(
            Severity::Warning,
            "einvoice_invoices_rejected",
            &format!("{} invoice(s) rejected by the platform", res.errored),
            with_count(count(res.errored) as usize),
        );
    }
    if config_stale >= CONFIG_STALE_REPORT_AFTER {
        let mut m = Map::new();
        m.insert("consecutive_failures", u64::from(config_stale));
        ctx.report(
            Severity::Warning,
            "einvoice_config_stale",
            "hub config refresh keeps failing; running on the last known collect settings",
            m,
        );
    }
    res.push_failed
        .clone()
        .or_else(|| res.sql_errors.first().cloned())
        .or_else(|| res.outbox_errors.first().cloned())
}

#[cfg(test)]
mod tests {
    use dl_json::Value;

    use super::*;

    fn ctx() -> ModuleContext {
        ModuleContext::standalone("einvoice", Value::Null, std::env::temp_dir())
    }

    #[test]
    fn a_clean_cycle_has_no_problem() {
        assert_eq!(report_cycle(&ctx(), &CycleResult::default(), 0), None);
    }

    #[test]
    fn push_failure_degrades_before_sql_and_outbox() {
        let res = CycleResult {
            push_failed: Some("agent: request failed after 5 attempt(s): timeout".into()),
            sql_errors: vec!["sql: login failed".into()],
            outbox_errors: vec!["outbox: disk full".into()],
            ..Default::default()
        };
        assert_eq!(
            report_cycle(&ctx(), &res, 0).as_deref(),
            Some("agent: request failed after 5 attempt(s): timeout")
        );
        let res = CycleResult {
            sql_errors: vec!["sql: login failed".into()],
            item_errors: vec!["FO:1: bad bill".into()],
            ..Default::default()
        };
        assert_eq!(
            report_cycle(&ctx(), &res, 7).as_deref(),
            Some("sql: login failed")
        );
    }

    #[test]
    fn item_and_writeback_failures_only_warn() {
        let res = CycleResult {
            item_errors: vec!["FO:1: bad bill".into()],
            poll_failed: Some("agent: POST /poll: status 502".into()),
            errored: 2,
            ..Default::default()
        };
        assert_eq!(report_cycle(&ctx(), &res, 0), None);
    }

    #[test]
    fn exclusive_waits_for_the_previous_holder_or_stop() {
        static LOCK: Mutex<()> = Mutex::new(());
        let stop = StopSignal::new();
        let held = exclusive(&LOCK, &stop).unwrap();
        let s2 = stop.clone();
        let waiter = std::thread::spawn(move || exclusive(&LOCK, &s2).is_some());
        std::thread::sleep(Duration::from_millis(50));
        stop.stop();
        assert!(!waiter.join().unwrap(), "stopped while the lock was held");
        drop(held);
        assert!(exclusive(&LOCK, &StopSignal::new()).is_some());
    }
}
