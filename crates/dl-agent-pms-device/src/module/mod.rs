//! The dl-agent module `pms-device`: runs the device agent worker (claim, execute, report)
//! under [`dl_agent::supervisor::Module`].
//!
//! The module is opt-in (`modules.pms-device.enabled = true`). While the settings are
//! invalid or the token file cannot be read it stays degraded with the reason and looks
//! again every 30 s; it does not fail and restart. Otherwise the worker runs on its own
//! thread; the module thread answers commands, publishes metrics and health, and restarts
//! the worker when an appsettings file or the token file changes. The module turns
//! running after the first claim round trip and degraded while the API keeps failing.
//!
//! On stop the worker gets [`STOP_GRACE`]: a device call that ignores the cancellation and
//! blocks longer is left to finish on its own thread; its result is saved in the outbox
//! first, so the next start sends it.

pub mod commands;
pub mod logger;
pub mod prepare;
pub mod selftest;
pub mod settings;
pub mod telemetry;

use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dl_agent::Error;
use dl_agent::supervisor::{Factory, Module, ModuleContext};

use crate::adapters::{self, DeviceAdapter, PosDevice};
use crate::agent::log::{Level, Logger};
use crate::agent::{
    Clock, CommandProcessor, DeviceAgentWorker, ResultOutbox, ResultReporter, StopToken,
    SystemClock, TokenFile, WorkerExit,
};
use crate::queue::{ApiCommandQueue, CommandQueue};
use commands::{CtxReporter, Flusher, View, answer, publish_metrics};
use logger::{DlLogger, module_log};
use prepare::{Prepared, Ready, fingerprint, prepare, process_env};
use settings::Settings;
use telemetry::{Health, Telemetry};

/// The module name: config section, report module and command target.
pub const NAME: &str = "pms-device";
/// The API counts as down once it has failed this long (the worker's longest back-off).
pub const DOWN_AFTER: Duration = Duration::from_secs(crate::agent::MAX_BACKOFF_SECS);
/// How long a stop waits for the worker before leaving it to finish a device call.
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often the module thread answers commands and publishes metrics and health.
const TICK: Duration = Duration::from_millis(250);
/// How often a waiting module looks at the settings again, and a running one at its files.
const RECHECK: Duration = Duration::from_secs(30);
/// How often the outbox is counted for the metrics.
const OUTBOX_EVERY: Duration = Duration::from_secs(2);

/// Parts a test or an embedding host can replace; `None` takes the production default.
#[derive(Clone, Default)]
pub struct Hooks {
    /// The door-lock adapter (default: `Agent:Adapter`).
    pub adapter: Option<Arc<dyn DeviceAdapter>>,
    /// The POS devices (default: the POS router).
    pub pos: Option<Arc<dyn PosDevice>>,
    /// The environment the settings are read with (default: the process environment).
    pub env: Option<Vec<(String, String)>>,
    /// See [`DOWN_AFTER`].
    pub down_after: Option<Duration>,
    /// How often settings and files are looked at again (default 30 s).
    pub recheck: Option<Duration>,
    /// See [`STOP_GRACE`].
    pub stop_grace: Option<Duration>,
}

/// The SMILE PMS device module.
#[derive(Default)]
pub struct PmsDeviceModule {
    hooks: Hooks,
}

/// The factory to register with `App::module`.
pub fn pms_device_module() -> Factory {
    pms_device_module_with(Hooks::default())
}

/// [`pms_device_module`] with replaced parts.
pub fn pms_device_module_with(hooks: Hooks) -> Factory {
    Arc::new(move || {
        Box::new(PmsDeviceModule {
            hooks: hooks.clone(),
        }) as Box<dyn Module>
    })
}

/// Why a worker cycle ended.
enum End {
    /// `ctx.stop()`.
    Stopped,
    /// A watched file changed: start again with fresh settings.
    Changed,
}

impl Module for PmsDeviceModule {
    fn name(&self) -> &str {
        NAME
    }

    fn enabled_by_default(&self) -> bool {
        false
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        let settings = Settings::from_config(ctx.config());
        let reporter = Arc::new(CtxReporter::new(ctx.clone()));
        let tel = Arc::new(Telemetry::new(Some(reporter)));
        loop {
            match prepare(&settings, ctx.data_dir(), &self.env()) {
                Prepared::Ready(ready) => match self.cycle(ctx, &ready, &tel)? {
                    End::Stopped => return Ok(()),
                    End::Changed => {}
                },
                Prepared::Waiting(detail) => {
                    if !self.idle(ctx, &settings, &tel, &detail) {
                        return Ok(());
                    }
                }
            }
        }
    }
}

impl PmsDeviceModule {
    fn env(&self) -> Vec<(String, String)> {
        self.hooks.env.clone().unwrap_or_else(process_env)
    }

    fn recheck(&self) -> Duration {
        self.hooks.recheck.unwrap_or(RECHECK)
    }

    /// Runs the worker until stop, a watched file changes or the worker dies.
    fn cycle(
        &self,
        ctx: &ModuleContext,
        ready: &Ready,
        tel: &Arc<Telemetry>,
    ) -> Result<End, Error> {
        let o = &ready.options;
        let log: Arc<dyn Logger> = Arc::new(DlLogger);
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let adapter = match &self.hooks.adapter {
            Some(a) => a.clone(),
            None => Arc::from(
                adapters::create(&o.adapter, &o.lock).map_err(|e| Error::Module(e.to_string()))?,
            ),
        };
        let pos = match &self.hooks.pos {
            Some(p) => p.clone(),
            None => Arc::from(adapters::pos::router()),
        };
        let token = Arc::new(TokenFile::new(o.token_file.clone()));
        let queue: Arc<dyn CommandQueue> = Arc::new(ApiCommandQueue::with_proxy(
            o,
            token,
            clock.clone(),
            dl_agent::proxy::current().proxy.clone(),
        ));
        let outbox = ResultOutbox::new(o.outbox_directory(), log.clone());
        let reporter = |q: &Arc<dyn CommandQueue>| {
            ResultReporter::new(q.clone(), outbox.clone(), clock.clone(), log.clone())
        };
        let stop = StopToken::new();
        let flusher = Flusher::new(reporter(&queue), outbox.clone(), stop.clone());
        let processor = CommandProcessor::new(
            queue.clone(),
            adapter,
            reporter(&queue),
            o,
            log.clone(),
            Some(pos),
        )
        .with_observer(tel.clone());
        let worker =
            DeviceAgentWorker::new(processor, o, clock.clone(), log).with_observer(tel.clone());
        tel.reset_api();
        let wstop = stop.clone();
        let handle = thread::Builder::new()
            .name("pms-device-agent".into())
            .spawn(move || worker.run(&wstop))
            .map_err(|e| Error::Io("spawn pms-device worker", e))?;

        let watched = ready.watched();
        let fp = fingerprint(&watched);
        let target = format!("{} as {} ({})", o.api_base_url, o.agent_id, o.adapter);
        let connecting = format!("connecting to {target}");
        let running = format!("polling {target}");
        let down_after = self.hooks.down_after.unwrap_or(DOWN_AFTER);
        let mut outbox_count = OutboxCount::default();
        let mut next_check = Instant::now() + self.recheck();
        let end = loop {
            let detail = match tel.health(down_after) {
                Health::Up => {
                    ctx.set_running(&running);
                    running.clone()
                }
                Health::Down { for_secs, reason } => {
                    let d = format!("API failing for {for_secs} s: {reason}");
                    ctx.set_degraded(&d);
                    d
                }
                Health::Pending => connecting.clone(),
            };
            let pending = outbox_count.get(&outbox);
            publish_metrics(ctx, tel, pending);
            let view = View {
                detail: &detail,
                outbox_pending: pending,
                flusher: Some(&flusher),
            };
            while let Some(cmd) = ctx.next_command() {
                answer(ctx, &cmd, tel, view);
            }
            if handle.is_finished() {
                break None;
            }
            if Instant::now() >= next_check {
                next_check = Instant::now() + self.recheck();
                if fingerprint(&watched) != fp {
                    module_log(
                        Level::Information,
                        "Settings or token changed; restarting the worker",
                    );
                    break Some(End::Changed);
                }
            }
            if ctx.stop().wait_timeout(TICK) {
                break Some(End::Stopped);
            }
        };
        stop.stop();
        let Some(end) = end else {
            return Err(ended(handle.join()));
        };
        finish(handle, self.hooks.stop_grace.unwrap_or(STOP_GRACE));
        publish_metrics(ctx, tel, OutboxCount::default().get(&outbox));
        Ok(end)
    }

    /// Stays degraded with `detail`, answering commands, until stop (`false`) or until the
    /// settings read differently (`true`).
    fn idle(
        &self,
        ctx: &ModuleContext,
        settings: &Settings,
        tel: &Telemetry,
        detail: &str,
    ) -> bool {
        ctx.set_degraded(detail);
        module_log(
            Level::Warning,
            &format!("Device agent not started: {detail}"),
        );
        let view = View {
            detail,
            outbox_pending: None,
            flusher: None,
        };
        let mut next = Instant::now() + self.recheck();
        loop {
            publish_metrics(ctx, tel, None);
            while let Some(cmd) = ctx.next_command() {
                answer(ctx, &cmd, tel, view);
            }
            if Instant::now() >= next {
                next = Instant::now() + self.recheck();
                if prepare(settings, ctx.data_dir(), &self.env()).detail() != Some(detail) {
                    return true;
                }
            }
            if ctx.stop().wait_timeout(TICK) {
                return false;
            }
        }
    }
}

/// The outbox size for the metrics, counted at most every [`OUTBOX_EVERY`]; a failure is
/// logged once until the count works again.
#[derive(Default)]
struct OutboxCount {
    next: Option<Instant>,
    last: Option<u64>,
    failing: bool,
}

impl OutboxCount {
    fn get(&mut self, outbox: &ResultOutbox) -> Option<u64> {
        if self.next.is_some_and(|n| Instant::now() < n) {
            return self.last;
        }
        self.next = Some(Instant::now() + OUTBOX_EVERY);
        self.last = match outbox.pending_count() {
            Ok(n) => {
                self.failing = false;
                Some(n as u64)
            }
            Err(e) => {
                if !self.failing {
                    let dir = outbox.directory().display();
                    module_log(
                        Level::Warning,
                        &format!("Outbox {dir} cannot be counted: {e}"),
                    );
                }
                self.failing = true;
                None
            }
        };
        self.last
    }
}

/// The error for a worker that ended without a stop (it only ends on a stop).
fn ended(res: thread::Result<WorkerExit>) -> Error {
    match res {
        Err(_) => Error::Module("pms-device worker thread panicked".into()),
        Ok(exit) => Error::Module(format!("pms-device worker ended unexpectedly ({exit:?})")),
    }
}

/// Waits up to `grace` for the stopped worker; one still inside a device call is left to
/// finish it on its own thread.
fn finish(handle: JoinHandle<WorkerExit>, grace: Duration) {
    let deadline = Instant::now() + grace;
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if !handle.is_finished() {
        module_log(
            Level::Warning,
            "The worker is still inside a device call; it finishes on its own thread and its result waits in the outbox",
        );
        return;
    }
    match handle.join() {
        Err(_) => module_log(Level::Error, "The worker panicked while stopping"),
        Ok(_) => module_log(Level::Information, "Device agent stopped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timings_stay_inside_the_supervisor_limits() {
        // The supervisor waits 10 s for a module; the brief caps the worker grace at 5 s.
        assert!(STOP_GRACE <= Duration::from_secs(5));
        assert!(TICK < STOP_GRACE);
        assert_eq!(DOWN_AFTER.as_secs(), 30);
    }
}
