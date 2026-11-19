//! The service's background loops (port of `Hosting/AgentServices.cs`): each runs on its
//! own thread under [`run_resilient`], which logs an unexpected failure and restarts the
//! loop with capped backoff (`ResilientService`).

use crate::capture::{BatchStore, RuntimeConfigSource, SqlCaptureRunner};
use crate::outbox::{BatchUploader, BlobUploader, HeartbeatSender, UploadCycleResult};
use crate::reporting::{Exception, Logger, SiteEventService, Value, site_event_service};
use crate::transport::RetryPolicy;
use crate::util::ctx::Ctx;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// An unexpected failure of a loop (C#: any exception out of `RunLoopAsync`).
pub type ServiceError = Box<dyn std::error::Error + Send + Sync>;

/// One background loop (`ResilientService.RunLoopAsync`). Returns when `ctx` ends (or the
/// loop is done); an error restarts it.
pub trait LoopService: Send {
    /// The name in the restart log line (`Uploader`, `Heartbeat`, ...).
    fn name(&self) -> &'static str;
    /// Runs until `ctx` ends.
    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError>;
}

/// A wait that something else can cut short (`UploadWake.WaitAsync`): `false` when `ctx`
/// ended.
pub trait IdleWait: Send + Sync {
    /// Waits up to `timeout`, less when woken.
    fn wait(&self, timeout: Duration, ctx: &Ctx) -> bool;
}

/// The log category of the restart line: the full name of the C# service class
/// (`ILogger<T>` of the service). An unknown loop is assumed to be in `Hosting`.
pub fn category(name: &str) -> String {
    let class = match name {
        "Uploader" => "Hosting.UploaderService",
        "BlobUploader" => "Hosting.BlobUploaderService",
        "Heartbeat" => "Hosting.HeartbeatService",
        "Commands" => "Hosting.CommandService",
        "Capture" => "Hosting.CaptureService",
        "SelfUpdate" => "Hosting.SelfUpdateService",
        "OrderGrantsService" => "Commands.OrderGrantsService",
        "StaffSync" => "Staff.StaffSyncService",
        "PrintRelay" => "PrintRelay.PrintRelayService",
        "SiteEvents" => "Reporting.SiteEventService",
        other => return format!("MM.PdaOrder.SyncAgent.Hosting.{other}"),
    };
    format!("MM.PdaOrder.SyncAgent.{class}")
}

/// `ResilientService.ExecuteAsync`: runs `service` until `ctx` ends; a failure is logged
/// (`{Service} failed ({Failures} in a row); restarting in {Delay}`) and the loop restarts
/// after `retry.delay_for(min(failures, 10))`.
pub fn run_resilient(service: &mut dyn LoopService, retry: &RetryPolicy, log: &Logger, ctx: &Ctx) {
    let log = log.for_category(&category(service.name()));
    let mut failures: u32 = 0;
    while !ctx.is_done() {
        match service.run_loop(ctx) {
            Ok(()) => return,
            Err(_) if ctx.is_done() => return,
            Err(e) => {
                failures = failures.saturating_add(1);
                let delay = retry.delay_for(failures.min(10), None);
                log.error_ex(
                    Exception::from_error(&*e),
                    "{Service} failed ({Failures} in a row); restarting in {Delay}",
                    &[
                        Value::from(service.name()),
                        Value::from(failures),
                        Value::time_span(delay),
                    ],
                );
                if !(retry.sleep)(delay, ctx) {
                    return;
                }
            }
        }
    }
}

fn backoff(retry: &RetryPolicy, blocked: &mut u32, ctx: &Ctx) {
    *blocked = blocked.saturating_add(1);
    let _ = (retry.sleep)(retry.delay_for((*blocked).min(8), None), ctx);
}

/// `UploaderService`: drains the outbox; idle 1 s (cut short by the capture's upload wake,
/// ADR 0104 §4), backoff when blocked.
pub struct UploaderService {
    /// The uploader.
    pub uploader: BatchUploader,
    /// `Agent:UploadBatchesPerCycle`.
    pub batches_per_cycle: i32,
    /// Backoff when blocked.
    pub retry: RetryPolicy,
    /// Wakes the idle wait when capture queued the rows of an order command.
    pub wake: Option<Arc<dyn IdleWait>>,
}

impl LoopService for UploaderService {
    fn name(&self) -> &'static str {
        "Uploader"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        let mut blocked = 0;
        while !ctx.is_done() {
            match self.uploader.run_cycle(self.batches_per_cycle, ctx)? {
                UploadCycleResult::Progress => blocked = 0,
                UploadCycleResult::Idle => {
                    blocked = 0;
                    let idle = Duration::from_secs(1);
                    match &self.wake {
                        Some(w) => w.wait(idle, ctx),
                        None => ctx.sleep(idle),
                    };
                }
                _ => backoff(&self.retry, &mut blocked, ctx),
            }
        }
        Ok(())
    }
}

/// `BlobUploaderService` (ADR 0030): apart from batches so a large image backlog never
/// delays row data; idle 2 s.
pub struct BlobUploaderService {
    /// The uploader.
    pub uploader: BlobUploader,
    /// `Agent:UploadBlobsPerCycle`.
    pub blobs_per_cycle: i32,
    /// Backoff when blocked.
    pub retry: RetryPolicy,
}

impl LoopService for BlobUploaderService {
    fn name(&self) -> &'static str {
        "BlobUploader"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        let mut blocked = 0;
        while !ctx.is_done() {
            match self.uploader.run_cycle(self.blobs_per_cycle, ctx)? {
                UploadCycleResult::Progress => blocked = 0,
                UploadCycleResult::Idle => {
                    blocked = 0;
                    ctx.sleep(Duration::from_secs(2));
                }
                _ => backoff(&self.retry, &mut blocked, ctx),
            }
        }
        Ok(())
    }
}

/// Fixed-rate ticks (`PeriodicTimer`): a tick missed while the work ran fires at once,
/// then the schedule continues.
#[derive(Debug, Clone)]
pub struct PeriodicTicks {
    period: Duration,
    next: Instant,
}

impl PeriodicTicks {
    /// Ticks every `period` from now.
    pub fn new(period: Duration) -> PeriodicTicks {
        PeriodicTicks {
            period,
            next: Instant::now() + period,
        }
    }

    /// Waits for the next tick: `false` when `ctx` ended.
    pub fn wait(&mut self, ctx: &Ctx) -> bool {
        let now = Instant::now();
        if self.next > now && !ctx.sleep(self.next - now) {
            return false;
        }
        if ctx.is_done() {
            return false;
        }
        let now = Instant::now();
        let period = self.period.max(Duration::from_millis(1));
        while self.next <= now {
            self.next += period;
        }
        true
    }
}

/// `HeartbeatService`: one heartbeat now, then every `Agent:HeartbeatSeconds`. A send that
/// cannot reach the cloud is an error: the loop restarts with backoff, as in C#.
pub struct HeartbeatService {
    /// The sender.
    pub sender: Arc<HeartbeatSender>,
    /// `Agent:HeartbeatSeconds`; not above 0 fails like `new PeriodicTimer` (no heartbeat,
    /// restarts with backoff).
    pub seconds: i32,
}

impl LoopService for HeartbeatService {
    fn name(&self) -> &'static str {
        "Heartbeat"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        let secs = u64::try_from(self.seconds).ok().filter(|s| *s > 0).ok_or(
            "System.ArgumentOutOfRangeException: period ('Agent:HeartbeatSeconds') must be a positive value.",
        )?;
        let mut timer = PeriodicTicks::new(Duration::from_secs(secs));
        loop {
            self.sender.send(ctx)?;
            if !timer.wait(ctx) {
                return Ok(());
            }
        }
    }
}

/// `CaptureService`: the capture runner over the outbox and the runtime config
/// (`runner.RunAsync(store, config, ct)`).
pub struct CaptureService {
    runner: SqlCaptureRunner,
    store: Arc<dyn BatchStore>,
    config: Arc<dyn RuntimeConfigSource>,
}

impl CaptureService {
    /// The loop of `runner`.
    pub fn new(
        runner: SqlCaptureRunner,
        store: Arc<dyn BatchStore>,
        config: Arc<dyn RuntimeConfigSource>,
    ) -> CaptureService {
        CaptureService {
            runner,
            store,
            config,
        }
    }
}

impl LoopService for CaptureService {
    fn name(&self) -> &'static str {
        "Capture"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        self.runner
            .run(self.store.clone(), self.config.clone(), ctx);
        Ok(())
    }
}

/// `SiteEventService` as a loop: started first and stopped last by the host. At start,
/// when running as the Windows service, the service recovery policy is applied in the
/// background (`WindowsServiceRecovery.TryConfigure`); at stop, `AGENT_STOPPING` and a
/// last bounded flush.
pub struct SiteEventsLoop {
    service: SiteEventService,
    log: Logger,
}

impl SiteEventsLoop {
    /// The loop of `service`; `log` is the service logger.
    pub fn new(service: SiteEventService, log: &Logger) -> SiteEventsLoop {
        SiteEventsLoop {
            service,
            log: log.for_category(site_event_service::SERVICE_CATEGORY),
        }
    }
}

impl LoopService for SiteEventsLoop {
    fn name(&self) -> &'static str {
        "SiteEvents"
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        if dl_winsvc::is_service_process().unwrap_or(false) {
            let log = self.log.clone();
            let _ = std::thread::Builder::new()
                .name("ServiceRecovery".to_owned())
                .spawn(move || crate::bootstrap::workflow::try_configure_service_recovery(&log));
        }
        self.service.run(ctx);
        // The host's stop: the stop flush is bounded by the service itself.
        self.service.stop(&Ctx::background());
        Ok(())
    }
}

/// A loop from a closure (the hooks of not yet ported services, tests).
pub struct FnService<F> {
    name: &'static str,
    f: F,
}

impl<F: FnMut(&Ctx) -> Result<(), ServiceError> + Send> FnService<F> {
    /// A loop named `name` running `f`.
    pub fn new(name: &'static str, f: F) -> FnService<F> {
        FnService { name, f }
    }
}

impl<F: FnMut(&Ctx) -> Result<(), ServiceError> + Send> LoopService for FnService<F> {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run_loop(&mut self, ctx: &Ctx) -> Result<(), ServiceError> {
        (self.f)(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::{Level, LoggerConfig, MemorySink};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn quick_retry(sleeps: Arc<Mutex<Vec<Duration>>>) -> RetryPolicy {
        RetryPolicy {
            jitter: Arc::new(|| 1.0),
            sleep: Arc::new(move |d, ctx| {
                sleeps.lock().unwrap().push(d);
                !ctx.is_done()
            }),
            ..RetryPolicy::default()
        }
    }

    #[test]
    fn a_failing_loop_is_logged_and_restarted_with_backoff() {
        let sink = Arc::new(MemorySink::default());
        let log = Logger::new(LoggerConfig {
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let retry = quick_retry(sleeps.clone());
        let ctx = Ctx::background().child();
        let calls = AtomicU32::new(0);
        let mut svc = FnService::new("Uploader", |c: &Ctx| {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err("boom".into());
            }
            c.cancel();
            Ok(())
        });
        // The closure cancels its own (child) context: a clean return ends the service.
        run_resilient(&mut svc, &retry, &log, &ctx);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            *sleeps.lock().unwrap(),
            [Duration::from_millis(500), Duration::from_secs(1)]
        );
        let errors = sink.messages_at(Level::Error);
        assert_eq!(
            errors,
            [
                "Uploader failed (1 in a row); restarting in 00:00:00.5000000",
                "Uploader failed (2 in a row); restarting in 00:00:01"
            ]
        );
    }

    #[test]
    fn a_failure_after_stop_is_not_restarted() {
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let retry = quick_retry(sleeps.clone());
        let ctx = Ctx::background().child();
        let mut svc = FnService::new("Heartbeat", |c: &Ctx| {
            c.cancel();
            Err("cancelled".into())
        });
        run_resilient(&mut svc, &retry, &Logger::none(), &ctx);
        assert!(sleeps.lock().unwrap().is_empty());
    }

    #[test]
    fn periodic_ticks_stop_with_the_context() {
        let ctx = Ctx::background().child();
        let mut t = PeriodicTicks::new(Duration::from_millis(5));
        assert!(t.wait(&ctx));
        ctx.cancel();
        assert!(!t.wait(&ctx));
    }
}
