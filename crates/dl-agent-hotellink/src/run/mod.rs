//! The agent's work (Go `run.go`, `supervise.go` and the boot part of `cmd/agent`): wait for
//! SQL, seed `HLInventorySync`, ensure `RoomTypeMapped`, push the mapping, then run the
//! booking, availability and heartbeat loops, each restarted after a panic.
//!
//! The self-update path of the Go agent is not ported: dl-agent updates itself.

mod audit;
mod booking;
pub mod side;

#[cfg(test)]
pub(crate) mod tests;

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dl_time::Offset;

use crate::breaker::Breaker;
use crate::error::{Error, Kind};
use crate::hubclient::HubClient;
use crate::jobqueue::FileQueue;
use crate::liveness::LoopBeats;
use crate::log::{self, V};
use crate::pms::FullRepo;
use crate::reconcile::{Clock, Reconciler, rfc3339_utc, system_clock};

/// The clock's instant as RFC 3339 UTC.
fn rfc3339_now(now: &Clock) -> String {
    rfc3339_utc(now())
}

/// A stop flag the loops sleep on.
#[derive(Clone, Default)]
pub struct Stop(Arc<(Mutex<bool>, Condvar)>);

impl std::fmt::Debug for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Stop").field(&self.is_stopped()).finish()
    }
}

impl Stop {
    /// Raises the flag and wakes every sleeper.
    pub fn stop(&self) {
        *self.0.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.0.1.notify_all();
    }

    /// Whether the flag is up.
    pub fn is_stopped(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `sleepCtx`: sleeps `d` or until stopped; true when stopped.
    pub fn sleep(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        let mut g = self.0.0.lock().unwrap_or_else(|p| p.into_inner());
        while !*g {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            g = self
                .0
                .1
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        true
    }
}

/// The waits of the loops (Go constants); tests shrink them.
#[derive(Debug, Clone)]
pub struct Timings {
    /// `pollWaitSec`.
    pub poll_wait_sec: u32,
    /// The first retry wait of the poll, config and SQL loops.
    pub backoff_start: Duration,
    /// `maxBackoff` (the SQL wait caps at one minute too).
    pub backoff_max: Duration,
    /// `heartbeatSpan`.
    pub heartbeat: Duration,
    /// `cfgRefreshSpan`.
    pub cfg_refresh: Duration,
    /// `superviseBackoff`.
    pub supervise: Duration,
    /// Availability interval for the hub's minutes (`max(min, 1)` minutes).
    pub avail_interval: fn(i64) -> Duration,
    /// PMS breaker: consecutive failures that open it (Go `breaker.New(5, …)`).
    pub breaker_threshold: u32,
    /// PMS breaker: first cooldown (Go 30 s), doubled per failed probe.
    pub breaker_cooldown: Duration,
    /// PMS breaker: longest cooldown (Go 5 min).
    pub breaker_max: Duration,
    /// The wait before the next poll when the hub handed back only jobs the open breaker
    /// holds (Go has none and polls again at once).
    pub breaker_poll_wait: Duration,
}

fn minutes(m: i64) -> Duration {
    Duration::from_secs(60 * m.max(1) as u64)
}

impl Default for Timings {
    fn default() -> Timings {
        Timings {
            poll_wait_sec: 25,
            backoff_start: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            heartbeat: Duration::from_secs(30),
            cfg_refresh: Duration::from_secs(5 * 60),
            supervise: Duration::from_secs(2),
            avail_interval: minutes,
            breaker_threshold: 5,
            breaker_cooldown: Duration::from_secs(30),
            breaker_max: Duration::from_secs(300),
            breaker_poll_wait: Duration::from_secs(5),
        }
    }
}

/// Something the module turns into a rate-limited report. Messages carry job ids, counts
/// and error kinds only: no guest names, no SQL text, no token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// `hl_hub_failed`, `hl_auth_failed`, `hl_pms_failed`, `hl_reconcile_failed` or
    /// `hl_manual_conflict`.
    pub code: &'static str,
    /// What happened.
    pub message: String,
}

/// The mapping push and job status (`RunnerStatus` plus the module's metrics).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// `lastSyncHash`.
    pub mapping_hash: String,
    /// `mappingCount`.
    pub mapping_count: i64,
    /// `lastSyncAt` (RFC 3339 UTC, empty before the first push).
    pub last_sync_at: String,
    /// When the last job was processed (RFC 3339 UTC).
    pub last_job_at: String,
    /// The loops the last heartbeat found stalled.
    pub stalled: Vec<String>,
}

/// Counters and flags the module reads.
#[derive(Debug, Default)]
pub struct Stats {
    /// Jobs received from the hub.
    pub jobs_polled: AtomicU64,
    /// Jobs applied (or skipped) and marked applied.
    pub jobs_applied: AtomicU64,
    /// Job acks the hub accepted.
    pub jobs_acked: AtomicU64,
    /// Jobs that failed (PMS down or apply error).
    pub jobs_failed: AtomicU64,
    /// Jobs routed to manual handling.
    pub manual_conflicts: AtomicU64,
    /// Availability pushes the hub accepted.
    pub availability_pushes: AtomicU64,
    /// Successful polls.
    pub polls_ok: AtomicU64,
    /// Consecutive failed hub calls of the booking loop.
    pub hub_fail_streak: AtomicU32,
    /// The hub answered 401 since the last successful poll.
    pub unauthorized: AtomicBool,
    /// The hub answered 403: another install took over this hotel.
    pub superseded: AtomicBool,
    /// SQL answered at boot.
    pub sql_ready: AtomicBool,
    /// The last PMS write worked (`pmsOk`).
    pub pms_bad: AtomicBool,
    status: Mutex<Status>,
    events: Mutex<Vec<Event>>,
}

impl Stats {
    /// A copy of the status.
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn with_status(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap_or_else(|p| p.into_inner()));
    }

    fn event(&self, code: &'static str, message: String) {
        let mut ev = self.events.lock().unwrap_or_else(|p| p.into_inner());
        if ev.len() < 100 {
            ev.push(Event { code, message });
        }
    }

    /// Takes the events gathered since the last call.
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// `Runner`.
pub struct Runner {
    hub: HubClient,
    repo: Arc<dyn FullRepo>,
    rec: Reconciler,
    queue: FileQueue,
    breaker: Breaker,
    beats: LoopBeats,
    avail_interval_min: AtomicI64,
    err_ring: Mutex<Vec<String>>,
    /// Where `policy-cache.json` is written.
    state_dir: PathBuf,
    /// Read when `state_dir` has no policy cache yet (the Go agent's directory).
    legacy_policy_dir: Option<PathBuf>,
    now: Clock,
    t: Timings,
    stop: Stop,
    stats: Arc<Stats>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("hub", &self.hub)
            .finish_non_exhaustive()
    }
}

/// What a runner needs.
pub struct Parts {
    /// The hub client.
    pub hub: HubClient,
    /// The PMS.
    pub repo: Arc<dyn FullRepo>,
    /// The local job queue.
    pub queue: FileQueue,
    /// The state directory (policy cache).
    pub state_dir: PathBuf,
    /// The Go agent's directory, for its policy cache.
    pub legacy_policy_dir: Option<PathBuf>,
}

impl Runner {
    /// `NewRunner`: breaker 5 failures, 30 s cooldown up to 5 min ([`Timings::default`]).
    pub fn new(p: Parts) -> Runner {
        Runner::with(p, Timings::default(), system_clock())
    }

    /// A runner with custom waits and clock.
    pub fn with(p: Parts, t: Timings, now: Clock) -> Runner {
        let rec = Reconciler::with_clock(p.repo.clone(), now.clone());
        Runner {
            hub: p.hub,
            repo: p.repo,
            rec,
            queue: p.queue,
            breaker: Breaker::new(t.breaker_threshold, t.breaker_cooldown, t.breaker_max),
            beats: LoopBeats::default(),
            avail_interval_min: AtomicI64::new(0),
            err_ring: Mutex::new(Vec::new()),
            state_dir: p.state_dir,
            legacy_policy_dir: p.legacy_policy_dir,
            now,
            t,
            stop: Stop::default(),
            stats: Arc::new(Stats::default()),
        }
    }

    /// The counters and flags.
    pub fn stats(&self) -> Arc<Stats> {
        self.stats.clone()
    }

    /// Stops every loop: the sleeps wake up and the hub call in flight (the long poll
    /// included) is cut off. A SQL call in flight finishes first (its transaction is never
    /// cut in half).
    pub fn stop(&self) {
        self.stop.stop();
        self.hub.cancel();
    }

    /// Whether the runner was told to stop.
    pub fn is_stopped(&self) -> bool {
        self.stop.is_stopped()
    }

    /// `Breaker.State`.
    pub fn breaker_state(&self) -> &'static str {
        self.breaker.state()
    }

    /// Jobs not acked yet.
    pub fn queue_depth(&self) -> usize {
        self.queue.depth()
    }

    /// Jobs applied but not acked.
    pub fn unacked(&self) -> usize {
        self.queue.unacked().len()
    }

    /// `Run`: blocks until stopped (or superseded).
    pub fn run(self: &Arc<Self>) {
        self.seed_policy_from_cache();
        if !self.boot() {
            return;
        }
        if let Err(e) = self.sync_mapping() {
            log::warn(
                "initial mapping push failed",
                &[("error", V::S(e.message()))],
            );
        }
        type Loop = (&'static str, fn(&Runner));
        let loops: [Loop; 3] = [
            ("booking", Runner::booking_loop),
            ("availability", Runner::availability_loop),
            ("heartbeat", Runner::heartbeat_loop),
        ];
        let mut handles = Vec::new();
        for (name, f) in loops {
            let me = self.clone();
            let h = std::thread::Builder::new()
                .name(format!("hl-{name}"))
                .spawn(move || supervise(name, &me.stop, me.t.supervise, || f(&me)));
            match h {
                Ok(h) => handles.push(h),
                Err(e) => {
                    log::error(
                        "cannot start loop thread",
                        &[("loop", V::S(name)), ("error", V::S(&e.to_string()))],
                    );
                    self.stop.stop();
                }
            }
        }
        for h in handles {
            if h.join().is_err() {
                log::error("loop thread ended with a panic", &[]);
            }
        }
    }

    /// The boot of `cmd/agent`: waits for SQL (1 s doubling to 1 min), then the best-effort
    /// `HLInventorySync` seed and `RoomTypeMapped` check. False when stopped first.
    fn boot(&self) -> bool {
        let mut backoff = self.t.backoff_start;
        loop {
            if self.stop.is_stopped() {
                return false;
            }
            match self.repo.test_connection() {
                Ok(()) => break,
                Err(e) => {
                    log::warn(
                        "SQL not ready",
                        &[
                            ("error", V::S(e.message())),
                            ("retryInMs", V::I(backoff.as_millis() as i64)),
                        ],
                    );
                    self.stats
                        .event("hl_pms_failed", format!("SQL not ready: {:?}", e.kind()));
                }
            }
            if self.stop.sleep(backoff) {
                return false;
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
        self.stats.sql_ready.store(true, Ordering::SeqCst);
        log::info("SQL connection OK", &[]);
        match self
            .repo
            .seed_global_param_if_absent("HLInventorySync", "1")
        {
            Ok(()) => log::info("HLInventorySync seeded to 1 when absent", &[]),
            Err(e) => log::warn(
                "HLInventorySync seed failed (continuing)",
                &[("error", V::S(e.message()))],
            ),
        }
        match self.repo.ensure_room_type_mapped() {
            Ok(n) if n > 0 => log::info("RoomTypeMapped created and seeded", &[("rows", V::I(n))]),
            Ok(_) => {}
            Err(e) => log::warn(
                "RoomTypeMapped check failed (continuing)",
                &[("error", V::S(e.message()))],
            ),
        }
        true
    }

    /// `superseded`: a 403 stops every loop (single-active).
    fn superseded(&self, e: &Error) -> bool {
        if e.kind() != Kind::Superseded {
            return false;
        }
        log::warn(
            "agent superseded by a newer install, stopping (single-active)",
            &[],
        );
        self.stats.superseded.store(true, Ordering::SeqCst);
        self.stop();
        true
    }

    /// Notes a 401 for the module.
    fn note_hub_error(&self, e: &Error) {
        if e.kind() == Kind::Unauthorized && !self.stats.unauthorized.swap(true, Ordering::SeqCst) {
            self.stats.event(
                "hl_auth_failed",
                "the hub rejected the agent token (HTTP 401)".into(),
            );
        }
    }

    fn set_pms_ok(&self, ok: bool) {
        self.stats.pms_bad.store(!ok, Ordering::SeqCst);
    }

    /// `recordError`: at most 20 lines, local time.
    fn record_error(&self, msg: &str) {
        let t = (self.now)().to_offset(Offset::VIETNAM).datetime();
        let line = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {msg}",
            t.year(),
            t.month().number(),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        );
        let mut ring = self.err_ring.lock().unwrap_or_else(|p| p.into_inner());
        ring.push(line);
        let n = ring.len();
        if n > 20 {
            ring.drain(..n - 20);
        }
    }

    /// `drainErrors`.
    fn drain_errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.err_ring.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// `supervise`: runs `f` until it returns normally or the stop flag is up; a panic is
/// logged and `f` restarts after `backoff`.
pub fn supervise(name: &str, stop: &Stop, backoff: Duration, f: impl Fn()) {
    while !stop.is_stopped() {
        match std::panic::catch_unwind(AssertUnwindSafe(&f)) {
            Ok(()) => return,
            Err(p) => {
                let what = p
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".into());
                log::error(
                    "loop panic recovered",
                    &[("where", V::S(name)), ("panic", V::S(&what))],
                );
            }
        }
        if stop.is_stopped() {
            return;
        }
        log::warn("restarting loop after panic", &[("where", V::S(name))]);
        stop.sleep(backoff);
    }
}
