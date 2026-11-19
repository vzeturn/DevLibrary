//! The relay's job flow (port of `PrintRelay/PrintRelayEngine.cs`, `.Acks.cs` and
//! `.Spooler.cs`, ADR 0098 with the ADR 0022 §6 rules): a job is journaled before
//! anything else, marked `printing` on disk before its first byte goes out, and its
//! outcome is journaled before it is acked; the ack is retried until the cloud takes it.
//! One job at a time per printer. A job whose outcome is in doubt is never printed again:
//! it is acked failed and non-retryable, so a person checks the printer and reprints by
//! hand. A `spooler:<queue>` host (ADR 0105) prints RAW through the local Windows
//! spooler, only to a queue the last discovery reported, with the same journal rules.
//!
//! The C# tasks are threads here: every print and every ack started outside the ack loop
//! runs on its own thread, tracked so [`PrintRelayEngine::drain`] can wait for them.

use super::journal::{PrintJournal, PrintJournalEntry, print_job_states};
use super::policy::{PrinterTargetPolicy, TargetRefusal};
use super::raw_tcp::{PrintError, RawPrinter};
use super::spooler::{SpoolerPrinter, SpoolerQueues, event_code};
use crate::config::time::SharedClock;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::print_spooler_address;
use crate::contracts::wire::site_event_levels::{ERROR, WARN};
use crate::reporting::{Exception, Logger, SiteEvents, Value};
use crate::util::ctx::Ctx;
use dl_json::{Map, Value as JsonValue};
use dl_uuid::Uuid;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// `ILogger<PrintRelayEngine>`.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.PrintRelay.PrintRelayEngine";
/// Jobs in progress beyond which a push is refused as busy (retryable).
pub const MAX_INFLIGHT: usize = 256;
/// Longest printer host name.
pub const MAX_HOST_LENGTH: usize = 253;
/// How long a settled job id is remembered past its expiry against a late re-push.
pub const REMEMBER_FINISHED: Duration = Duration::from_secs(10 * 60);
/// An ack still undelivered this long past the job's expiry is dropped.
pub const ACK_GIVE_UP_AFTER_EXPIRY: Duration = Duration::from_secs(3600);
/// Each ack call is bounded to this.
pub const ACK_CALL_TIMEOUT: Duration = Duration::from_secs(15);
const ACK_BACKOFF_MIN: Duration = Duration::from_secs(1);
const ACK_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The error of a job that was printing when the agent stopped.
pub const STOPPED_WHILE_PRINTING: &str = "outcome unknown: the agent stopped while printing; check the printer and reprint from Failed jobs if needed";
/// The error of a job that expired before it could print.
pub const EXPIRED: &str = "job expired before the agent could print it";

/// A job pushed by the cloud (`PrintRelayJob`, the order hub's `PrintRelayJobMessage`).
#[derive(Debug, Clone, PartialEq)]
pub struct PrintRelayJob {
    /// Job id.
    pub job_id: Uuid,
    /// Branch.
    pub rvc_no: i32,
    /// Printer host or `spooler:<queue>` (empty when the cloud sent none).
    pub host: String,
    /// Printer port.
    pub port: i32,
    /// The ESC/POS bytes.
    pub payload: Vec<u8>,
    /// When the cloud gives up on the job.
    pub expires_at: DateTimeOffset,
    /// Delivery attempts so far.
    pub attempts: i32,
}

/// The hub call that settles a job (`IPrintRelayLink.AckAsync`): `Ok(applied)`, or the
/// failure when the ack did not reach the cloud.
pub trait PrintRelayLink: Send + Sync {
    /// `AckPrintJob(jobId, ok, error, retryable)`.
    fn ack(
        &self,
        job_id: Uuid,
        ok: bool,
        error: Option<&str>,
        retryable: bool,
        ctx: &Ctx,
    ) -> Result<bool, Box<Exception>>;
}

fn ticks(d: Duration) -> i64 {
    i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn io_type_name(e: &std::io::Error) -> &'static str {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        "UnauthorizedAccessException"
    } else {
        "IOException"
    }
}

fn io_exception(e: &std::io::Error) -> Exception {
    Exception::new(format!("System.{}", io_type_name(e)), e.to_string())
}

/// A tracked print or ack.
type Task = Box<dyn FnOnce() + Send>;

/// What to send once the job is journaled `printing`.
enum SendTo {
    Spooler { queue: String, document: String },
    Raw(std::net::SocketAddr),
}

#[derive(Default)]
struct Running {
    count: usize,
}

struct Inner {
    journal: PrintJournal,
    policy: PrinterTargetPolicy,
    printer: Arc<dyn RawPrinter>,
    events: Arc<dyn SiteEvents>,
    clock: SharedClock,
    log: Logger,
    spooler: Arc<dyn SpoolerPrinter>,
    spooler_queues: Arc<SpoolerQueues>,
    inflight: Mutex<HashMap<Uuid, PrintJournalEntry>>,
    /// Job id -> UTC ticks until which a re-push is ignored.
    finished: Mutex<HashMap<Uuid, i64>>,
    lanes: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    running: Mutex<Running>,
    running_cv: Condvar,
    sending: Mutex<HashSet<Uuid>>,
    /// Job id -> (failures, due UTC ticks).
    ack_retry: Mutex<HashMap<Uuid, (i32, i64)>>,
    kick: Mutex<u32>,
    kick_cv: Condvar,
    link: Mutex<Option<Arc<dyn PrintRelayLink>>>,
}

/// The engine (`PrintRelayEngine`). Cheap to clone (shared state).
#[derive(Clone)]
pub struct PrintRelayEngine(Arc<Inner>);

impl std::fmt::Debug for PrintRelayEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintRelayEngine")
            .field("inflight", &self.inflight())
            .finish_non_exhaustive()
    }
}

/// What the engine works with.
pub struct PrintRelayEngineParts {
    /// The durable journal.
    pub journal: PrintJournal,
    /// The LAN target gate.
    pub policy: PrinterTargetPolicy,
    /// Raw TCP printing.
    pub printer: Arc<dyn RawPrinter>,
    /// Site events.
    pub events: Arc<dyn SiteEvents>,
    /// The clock (expiry checks).
    pub clock: SharedClock,
    /// Windows spooler printing.
    pub spooler: Arc<dyn SpoolerPrinter>,
    /// The reported USB/WSD queues.
    pub spooler_queues: Arc<SpoolerQueues>,
}

impl PrintRelayEngine {
    /// An engine over `parts`, logging under [`CATEGORY`].
    pub fn new(parts: PrintRelayEngineParts, log: &Logger) -> PrintRelayEngine {
        PrintRelayEngine(Arc::new(Inner {
            journal: parts.journal,
            policy: parts.policy,
            printer: parts.printer,
            events: parts.events,
            clock: parts.clock,
            log: log.for_category(CATEGORY),
            spooler: parts.spooler,
            spooler_queues: parts.spooler_queues,
            inflight: Mutex::default(),
            finished: Mutex::default(),
            lanes: Mutex::default(),
            running: Mutex::default(),
            running_cv: Condvar::new(),
            sending: Mutex::default(),
            ack_retry: Mutex::default(),
            kick: Mutex::new(0),
            kick_cv: Condvar::new(),
            link: Mutex::new(None),
        }))
    }

    /// Jobs journaled and not yet settled.
    pub fn inflight(&self) -> usize {
        lock(&self.0.inflight).len()
    }

    fn now_ticks(&self) -> i64 {
        DateTimeOffset::from_unix_nanos(self.0.clock.now().unix_nanos(), 0)
            .unwrap_or(DateTimeOffset::UNIX_EPOCH)
            .utc_ticks()
    }

    /// Restart recovery: a job left `printing` is in doubt and becomes a non-retryable
    /// failure; a `received` job never reached the printer and is printed (unless it
    /// expired); an `acking` one waits for the ack loop. A journal that cannot be read
    /// fails the call.
    pub fn recover(&self) -> std::io::Result<()> {
        let (entries, corrupt) = self.0.journal.load()?;
        if !corrupt.is_empty() {
            let mut detail = Map::new();
            detail.insert(
                "files",
                JsonValue::Array(
                    corrupt
                        .iter()
                        .take(20)
                        .map(|f| JsonValue::from(f.as_str()))
                        .collect(),
                ),
            );
            self.0.events.report(
                ERROR,
                "PRINT_JOURNAL_CORRUPT",
                &format!(
                    "{} print journal file(s) could not be read and were set aside; check those jobs on the printer",
                    corrupt.len()
                ),
                Some(detail),
            );
        }
        let now = self.now_ticks();
        for entry in entries {
            match entry.state.as_str() {
                print_job_states::PRINTING => {
                    self.decide(&entry, false, Some(STOPPED_WHILE_PRINTING), false)?;
                }
                print_job_states::RECEIVED
                    if entry.payload.is_none() || now >= entry.expires_at.utc_ticks() =>
                {
                    self.decide(&entry, false, Some(EXPIRED), false)?;
                }
                print_job_states::RECEIVED => {
                    lock(&self.0.inflight).insert(entry.job_id, entry.clone());
                    let me = self.clone();
                    self.track(move || me.print(&entry, &Ctx::background()));
                }
                _ => {
                    lock(&self.0.inflight).insert(entry.job_id, entry);
                }
            }
        }
        Ok(())
    }

    /// Takes a pushed job; never blocks the hub callback. Printing runs in the job's
    /// printer lane. A journal write that fails is logged (C#: the exception escapes the
    /// hub callback).
    pub fn accept(&self, job: &PrintRelayJob) {
        if let Err(e) = self.try_accept(job) {
            self.0.log.error_ex(
                io_exception(&e),
                "Print job {JobId} could not be processed",
                &[Value::from(job.job_id)],
            );
        }
    }

    fn try_accept(&self, job: &PrintRelayJob) -> std::io::Result<()> {
        let known = lock(&self.0.finished).contains_key(&job.job_id)
            || lock(&self.0.inflight).contains_key(&job.job_id);
        if known {
            self.0.log.info(
                "Print job {JobId} pushed again; already handled or in progress",
                &[Value::from(job.job_id)],
            );
            return Ok(());
        }
        let entry = PrintJournalEntry {
            job_id: job.job_id,
            rvc_no: job.rvc_no,
            host: job.host.clone(),
            port: job.port,
            expires_at: job.expires_at,
            attempts: job.attempts,
            state: print_job_states::RECEIVED.to_owned(),
            payload: Some(job.payload.clone()),
            ok: false,
            error: None,
            retryable: false,
        };
        if let Some(problem) = invalid(job) {
            let error = format!("invalid print job: {problem}");
            return self.decide(&entry, false, Some(&error), false);
        }
        if self.now_ticks() >= job.expires_at.utc_ticks() {
            return self.decide(&entry, false, Some(EXPIRED), false);
        }
        if self.inflight() >= MAX_INFLIGHT {
            // Not journaled, nothing sent: the cloud may push it again once the backlog
            // drains.
            let busy = entry.outcome(
                false,
                Some("agent busy: too many print jobs in progress".into()),
                true,
            );
            let me = self.clone();
            self.track(move || me.send_ack(&busy, &Ctx::background()));
            return Ok(());
        }
        self.0.journal.write(&entry)?;
        lock(&self.0.inflight).insert(job.job_id, entry.clone());
        let me = self.clone();
        self.track(move || me.print(&entry, &Ctx::background()));
        Ok(())
    }

    /// Waits until no print or ack is running, including acks started by those prints
    /// (tests; a clean stop).
    pub fn drain(&self) {
        let mut r = lock(&self.0.running);
        while r.count > 0 {
            r = self
                .0
                .running_cv
                .wait(r)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn track(&self, f: impl FnOnce() + Send + 'static) {
        lock(&self.0.running).count += 1;
        let me = self.clone();
        let done = move || {
            let mut r = lock(&me.0.running);
            r.count -= 1;
            me.0.running_cv.notify_all();
        };
        let job: Arc<Mutex<Option<Task>>> = Arc::new(Mutex::new(Some(Box::new(f))));
        let j = job.clone();
        let spawned = std::thread::Builder::new()
            .name("PrintRelay".to_owned())
            .spawn(move || {
                if let Some(f) = lock(&j).take() {
                    f();
                }
                done();
            });
        if spawned.is_err() {
            // No thread: run it here (the closure was not taken by a thread).
            if let Some(f) = lock(&job).take() {
                f();
            }
            let mut r = lock(&self.0.running);
            r.count -= 1;
            self.0.running_cv.notify_all();
        }
    }

    fn lane(&self, entry: &PrintJournalEntry) -> Arc<Mutex<()>> {
        let key = format!("{}:{}", entry.host, entry.port).to_lowercase();
        lock(&self.0.lanes).entry(key).or_default().clone()
    }

    fn print(&self, entry: &PrintJournalEntry, ctx: &Ctx) {
        let lane = self.lane(entry);
        let _turn = lock(&lane);
        if let Err(e) = self.print_in_lane(entry, ctx) {
            // The journal could not be written: leave the file as it is (received or
            // printing) for the next start to settle.
            self.0.log.error_ex(
                io_exception(&e),
                "Print job {JobId} could not be processed",
                &[Value::from(entry.job_id)],
            );
            let mut detail = Map::new();
            detail.insert("job_id", entry.job_id.to_string());
            detail.insert("rvc", i64::from(entry.rvc_no));
            self.0.events.report(
                ERROR,
                "PRINT_JOB_STUCK",
                &format!(
                    "Print job could not be processed ({}); it is settled when the agent restarts",
                    io_type_name(&e)
                ),
                Some(detail),
            );
        }
    }

    fn print_in_lane(&self, entry: &PrintJournalEntry, ctx: &Ctx) -> std::io::Result<()> {
        if self.now_ticks() >= entry.expires_at.utc_ticks() {
            return self.decide(entry, false, Some(EXPIRED), false);
        }
        let send = match self.resolve_send(entry, ctx) {
            Ok(s) => s,
            Err(refusal) => {
                let error = format!("printer target refused: {}", refusal.reason);
                return self.decide(entry, false, Some(&error), refusal.retryable);
            }
        };
        // Fail closed from here: once this is on disk a restart never prints the job again.
        let printing = PrintJournalEntry {
            state: print_job_states::PRINTING.to_owned(),
            ..entry.clone()
        };
        self.0.journal.write(&printing)?;
        lock(&self.0.inflight).insert(entry.job_id, printing.clone());
        let payload = entry.payload.as_deref().unwrap_or_default();
        let result = match &send {
            SendTo::Spooler { queue, document } => {
                self.0.spooler.print(queue, document, payload, ctx)
            }
            SendTo::Raw(target) => self.0.printer.print(*target, payload, ctx),
        };
        match result {
            Ok(()) => self.decide(&printing, true, None, false),
            Err(PrintError::Send(e)) => {
                if let Some(win32) = e.win32_error {
                    self.report_spooler_problem(entry, win32);
                }
                let error = if e.not_sent {
                    e.message.clone()
                } else {
                    format!("outcome unknown: {}", e.message)
                };
                self.decide(&printing, false, Some(&error), e.retryable())
            }
            // Stopping (C#: the cancellation escapes the print task): the journal keeps
            // the job `printing` for the next start to settle.
            Err(PrintError::Cancelled) => Ok(()),
        }
    }

    /// The send to run once the job is journaled `printing`, or why the target is
    /// refused (`ResolveSendAsync`).
    fn resolve_send(&self, entry: &PrintJournalEntry, ctx: &Ctx) -> Result<SendTo, TargetRefusal> {
        if print_spooler_address::is_spooler(Some(&entry.host)) {
            let Some(queue) = self.0.spooler_queues.try_resolve(&entry.host) else {
                return Err(TargetRefusal {
                    reason: format!(
                        "'{}' is not a USB or WSD print queue this server reported ({} reported)",
                        entry.host,
                        self.0.spooler_queues.count()
                    ),
                    retryable: false,
                });
            };
            return Ok(SendTo::Spooler {
                queue,
                document: format!("PDA print {}", entry.job_id),
            });
        }
        self.0
            .policy
            .resolve(&entry.host, entry.port, ctx)
            .map(SendTo::Raw)
    }

    /// A spooler problem a person must fix on the server gets its own site event, next to
    /// the failed job.
    fn report_spooler_problem(&self, entry: &PrintJournalEntry, win32: i32) {
        let Some(code) = event_code(win32) else {
            return;
        };
        let message = match code {
            "PRINT_SPOOLER_ACCESS_DENIED" => {
                "The SyncAgent service account may not print to a Windows print queue; give it Print permission on the queue"
            }
            "PRINT_SPOOLER_UNAVAILABLE" => {
                "The Windows Print Spooler service is not running on this server; start it"
            }
            _ => {
                "A Windows print queue does not take RAW data; install a v3 driver or Generic / Text Only for that printer"
            }
        };
        let mut detail = Map::new();
        detail.insert("job_id", entry.job_id.to_string());
        detail.insert("rvc", i64::from(entry.rvc_no));
        detail.insert("target", target_text(entry));
        detail.insert("win32_error", i64::from(win32));
        self.0.events.report(ERROR, code, message, Some(detail));
    }

    /// Journals the outcome, reports a failure, then sends the ack once (the ack loop
    /// retries it).
    fn decide(
        &self,
        entry: &PrintJournalEntry,
        ok: bool,
        error: Option<&str>,
        retryable: bool,
    ) -> std::io::Result<()> {
        let decided = entry.outcome(ok, error.map(str::to_owned), retryable);
        self.0.journal.write(&decided)?;
        lock(&self.0.inflight).insert(entry.job_id, decided.clone());
        if !ok {
            let mut detail = Map::new();
            detail.insert("job_id", entry.job_id.to_string());
            detail.insert("rvc", i64::from(entry.rvc_no));
            detail.insert("target", target_text(entry));
            detail.insert("retryable", retryable);
            detail.insert("attempts", i64::from(entry.attempts));
            self.0.events.report(
                WARN,
                "PRINT_JOB_FAILED",
                &format!("Print job failed: {}", error.unwrap_or("")),
                Some(detail),
            );
        }
        let me = self.clone();
        self.track(move || me.send_ack(&decided, &Ctx::background()));
        Ok(())
    }

    /// The hub link acks go through; `None` while disconnected (acks then wait in the
    /// journal).
    pub fn connect(&self, link: Option<Arc<dyn PrintRelayLink>>) {
        let connected = link.is_some();
        *lock(&self.0.link) = link;
        if connected {
            self.kick();
        }
    }

    /// Retries every pending ack now (after a reconnect).
    pub fn kick(&self) {
        lock(&self.0.ack_retry).clear();
        *lock(&self.0.kick) += 1;
        self.0.kick_cv.notify_one();
    }

    /// Waits up to `max` for a kick (taking one); false when `ctx` ended.
    fn wait_kick(&self, max: Duration, ctx: &Ctx) -> bool {
        let end = std::time::Instant::now() + max;
        let mut k = lock(&self.0.kick);
        loop {
            if ctx.is_done() {
                return false;
            }
            if *k > 0 {
                *k -= 1;
                return true;
            }
            let now = std::time::Instant::now();
            if now >= end {
                return true;
            }
            let step = (end - now).min(Duration::from_millis(50));
            k = self
                .0
                .kick_cv
                .wait_timeout(k, step)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Ack delivery (`RunAckLoopAsync`): every decided job is acked until the cloud takes
    /// it (backoff 1 s to 30 s, each call bounded to 15 s, a kick after every reconnect)
    /// or until an hour past its expiry. Returns when `ctx` ends.
    pub fn run_ack_loop(&self, ctx: &Ctx) {
        while !ctx.is_done() {
            if !self.wait_kick(ACK_BACKOFF_MIN, ctx) {
                return;
            }
            let now = self.now_ticks();
            lock(&self.0.finished).retain(|_, until| *until > now);
            let acking: Vec<PrintJournalEntry> = lock(&self.0.inflight)
                .values()
                .filter(|e| e.state == print_job_states::ACKING)
                .cloned()
                .collect();
            for entry in acking {
                if now > entry.expires_at.utc_ticks() + ticks(ACK_GIVE_UP_AFTER_EXPIRY) {
                    self.abandon(&entry);
                    continue;
                }
                let due = lock(&self.0.ack_retry).get(&entry.job_id).map(|r| r.1);
                if due.is_some_and(|d| d > now) {
                    continue;
                }
                self.send_ack(&entry, ctx);
            }
        }
    }

    fn send_ack(&self, entry: &PrintJournalEntry, ctx: &Ctx) {
        let Some(link) = lock(&self.0.link).clone() else {
            return;
        };
        if !lock(&self.0.sending).insert(entry.job_id) {
            return;
        }
        let call = ctx.with_timeout(ACK_CALL_TIMEOUT);
        let result = link.ack(
            entry.job_id,
            entry.ok,
            entry.error.as_deref(),
            entry.retryable,
            &call,
        );
        let result = match result {
            Ok(applied) => {
                if !applied {
                    self.0.log.info(
                        "Print job {JobId} ack (ok={Ok}) was late or a duplicate; the cloud had settled it",
                        &[Value::from(entry.job_id), Value::from(entry.ok)],
                    );
                }
                self.finish(entry).map_err(|e| Box::new(io_exception(&e)))
            }
            Err(ex) => Err(ex),
        };
        match result {
            Ok(()) => {}
            // Stopping: the ack stays in the journal and is sent after the next start.
            Err(_) if ctx.is_done() => {}
            Err(ex) => {
                let failures = lock(&self.0.ack_retry)
                    .get(&entry.job_id)
                    .map_or(1, |r| r.0 + 1);
                let shift = u32::try_from((failures - 1).clamp(0, 5)).unwrap_or(5);
                let delay = (ACK_BACKOFF_MIN * (1u32 << shift)).min(ACK_BACKOFF_MAX);
                let due = self.now_ticks() + ticks(delay);
                lock(&self.0.ack_retry).insert(entry.job_id, (failures, due));
                self.0.log.warn_ex(
                    *ex,
                    "Print job {JobId} ack failed ({Failures} in a row); retrying in {Delay}",
                    &[
                        Value::from(entry.job_id),
                        Value::from(failures),
                        Value::time_span(delay),
                    ],
                );
            }
        }
        lock(&self.0.sending).remove(&entry.job_id);
    }

    fn finish(&self, entry: &PrintJournalEntry) -> std::io::Result<()> {
        lock(&self.0.ack_retry).remove(&entry.job_id);
        if lock(&self.0.inflight).remove(&entry.job_id).is_none() {
            return Ok(()); // a "busy" refusal was never journaled
        }
        self.0.journal.delete(entry.job_id)?;
        // A retryable outcome may be pushed again and must print then; anything else is
        // remembered against a late re-push.
        if !entry.retryable {
            lock(&self.0.finished).insert(
                entry.job_id,
                entry.expires_at.utc_ticks() + ticks(REMEMBER_FINISHED),
            );
        }
        Ok(())
    }

    fn abandon(&self, entry: &PrintJournalEntry) {
        lock(&self.0.ack_retry).remove(&entry.job_id);
        lock(&self.0.inflight).remove(&entry.job_id);
        if let Err(e) = self.0.journal.delete(entry.job_id) {
            self.0.log.warn_ex(
                io_exception(&e),
                "Print journal file of job {JobId} could not be deleted",
                &[Value::from(entry.job_id)],
            );
        }
        let mut detail = Map::new();
        detail.insert("job_id", entry.job_id.to_string());
        detail.insert("rvc", i64::from(entry.rvc_no));
        detail.insert("ok", entry.ok);
        self.0.events.report(
            WARN,
            "PRINT_ACK_ABANDONED",
            "A print job's result could not be sent to the cloud within an hour of its expiry and was dropped",
            Some(detail),
        );
    }
}

/// What is wrong with a pushed job, if anything (`Invalid`).
pub fn invalid(job: &PrintRelayJob) -> Option<&'static str> {
    if job.job_id == Uuid::NIL {
        return Some("job id is empty");
    }
    if job.host.trim().is_empty() {
        return Some("printer host is empty");
    }
    if job.host.encode_utf16().count() > MAX_HOST_LENGTH {
        return Some("printer host is longer than 253 characters");
    }
    if !(1..=65535).contains(&job.port) {
        return Some("printer port is out of range");
    }
    if job.payload.is_empty() {
        return Some("payload is empty");
    }
    if job.expires_at.utc_ticks() == DateTimeOffset::MIN.utc_ticks() {
        return Some("job has no expiry");
    }
    None
}

/// `host` for a spooler job, else `host:port`.
fn target_text(entry: &PrintJournalEntry) -> String {
    if print_spooler_address::is_spooler(Some(&entry.host)) {
        entry.host.clone()
    } else {
        format!("{}:{}", entry.host, entry.port)
    }
}
