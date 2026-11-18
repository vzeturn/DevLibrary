//! `Agent/DeviceAgentWorker.cs`: the poll loop. Runs commands while any are queued, then
//! waits `PollInterval`; backs off 2^n s (at most 30 s) after failures.

use std::sync::Arc;
use std::time::Duration;

use super::log::{Log, Logger};
use super::{AgentError, Clock, CommandProcessor, Observer, StopToken};
use crate::config::{AgentOptions, TimeSpan};

/// The longest wait between two failed polls, in seconds.
pub const MAX_BACKOFF_SECS: u64 = 30;

/// How [`DeviceAgentWorker::run`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerExit {
    /// The stop was requested.
    Stopped,
    /// `RunOnce`: the queue is empty (`StopApplication`, exit code 0).
    QueueEmpty,
    /// `RunOnce`: a poll failed (`Environment.ExitCode = 1`).
    RunOnceFailed,
}

impl WorkerExit {
    /// The process exit code the .NET host ends with.
    pub fn exit_code(self) -> i32 {
        match self {
            WorkerExit::RunOnceFailed => 1,
            WorkerExit::Stopped | WorkerExit::QueueEmpty => 0,
        }
    }
}

/// The background worker (`BackgroundService`).
pub struct DeviceAgentWorker {
    processor: CommandProcessor,
    agent_id: String,
    api_base_url: String,
    poll_interval: TimeSpan,
    run_once: bool,
    clock: Arc<dyn Clock>,
    log: Log,
    observer: Option<Arc<dyn Observer>>,
}

impl DeviceAgentWorker {
    /// .NET category of the worker log.
    pub const CATEGORY: &'static str = "SmilePms.DeviceAgent.Agent.DeviceAgentWorker";

    /// A worker over `processor` with `options` (AgentId, ApiBaseUrl, PollInterval, RunOnce).
    pub fn new(
        processor: CommandProcessor,
        options: &AgentOptions,
        clock: Arc<dyn Clock>,
        log: Arc<dyn Logger>,
    ) -> DeviceAgentWorker {
        DeviceAgentWorker {
            processor,
            agent_id: options.agent_id.clone(),
            api_base_url: options.api_base_url.clone(),
            poll_interval: options.poll_interval,
            run_once: options.run_once,
            clock,
            log: Log::new(log, Self::CATEGORY),
            observer: None,
        }
    }

    /// The same worker, telling `observer` how each poll ended.
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> DeviceAgentWorker {
        self.observer = Some(observer);
        self
    }

    fn polled(&self, result: Result<bool, &AgentError>) {
        if let Some(o) = &self.observer {
            o.polled(result);
        }
    }

    /// `ExecuteAsync`: polls until `stop` is set or, with `RunOnce`, until the queue is
    /// empty or a poll fails. A result in flight is reported before a stop takes effect.
    pub fn run(&self, stop: &StopToken) -> WorkerExit {
        self.run_started(stop, || {})
    }

    /// [`DeviceAgentWorker::run`] as the host runs it: `started` is called after the first
    /// log line and before the first poll, where the .NET host logs its lifetime lines
    /// (`ApplicationStarted` fires once `ExecuteAsync` reaches its first await).
    pub fn run_started(&self, stop: &StopToken, started: impl FnOnce()) -> WorkerExit {
        let id = &self.agent_id;
        self.log
            .info(&format!("Device agent {id} polling {}", self.api_base_url));
        started();
        let mut failures: u32 = 0;
        while !stop.is_stopped() {
            let polled = self.processor.run_once(stop);
            match &polled {
                Err(AgentError::Stopped) if stop.is_stopped() => {}
                Ok(worked) => self.polled(Ok(*worked)),
                Err(e) => self.polled(Err(e)),
            }
            match polled {
                Ok(worked) => {
                    failures = 0;
                    if worked {
                        continue;
                    }
                    if self.run_once {
                        self.log.info(&format!(
                            "Device agent {id}: queue empty, run-once mode stops the service"
                        ));
                        return WorkerExit::QueueEmpty;
                    }
                    let poll = self.poll_interval.to_duration().unwrap_or(Duration::ZERO);
                    if !self.clock.sleep(poll, stop) {
                        return WorkerExit::Stopped;
                    }
                }
                Err(AgentError::Stopped) if stop.is_stopped() => return WorkerExit::Stopped,
                Err(e) if self.run_once => {
                    // A smoke run must fail visibly instead of retrying forever.
                    self.log.error(
                        &format!(
                            "Device agent {id}: run-once mode failed; stopping with exit code 1"
                        ),
                        Some(&e.to_dotnet()),
                    );
                    return WorkerExit::RunOnceFailed;
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    let delay = 1u64
                        .checked_shl(failures)
                        .unwrap_or(u64::MAX)
                        .min(MAX_BACKOFF_SECS);
                    self.log.error(
                        &format!(
                            "Device agent poll failed ({failures} in a row); retrying in {}",
                            TimeSpan::from_secs(delay as i64)
                        ),
                        Some(&e.to_dotnet()),
                    );
                    if !self.clock.sleep(Duration::from_secs(delay), stop) {
                        return WorkerExit::Stopped;
                    }
                }
            }
        }
        WorkerExit::Stopped
    }
}
