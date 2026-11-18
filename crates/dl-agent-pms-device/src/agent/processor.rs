//! `Agent/CommandProcessor.cs`: sends any results still owed, then claims one command, runs
//! it on the adapter and reports the outcome. lock.* commands go to the door-lock adapter,
//! pos.* commands to the POS device router.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use dl_json::Value;

use super::log::{Log, Logger, or_null};
use super::{AgentError, Observer, ResultReporter, StopToken};
use crate::adapters::{AdapterError, Cx, DeviceAdapter, PosDevice};
use crate::config::{AgentOptions, DotnetError, TimeSpan};
use crate::contracts::{
    CommandOutcome, DeviceCommand, Payload, PayloadError, PendingResult, codes, decode_payload,
    kinds,
};
use crate::queue::CommandQueue;

/// Runs one command per [`CommandProcessor::run_once`].
pub struct CommandProcessor {
    queue: Arc<dyn CommandQueue>,
    adapter: Arc<dyn DeviceAdapter>,
    reporter: ResultReporter,
    command_timeout: TimeSpan,
    log: Log,
    pos: Option<Arc<dyn PosDevice>>,
    observer: Option<Arc<dyn Observer>>,
}

/// Why a command did not produce an outcome.
enum Failure {
    Payload(PayloadError),
    Adapter(AdapterError),
}

impl From<PayloadError> for Failure {
    fn from(e: PayloadError) -> Failure {
        Failure::Payload(e)
    }
}

impl From<AdapterError> for Failure {
    fn from(e: AdapterError) -> Failure {
        Failure::Adapter(e)
    }
}

impl CommandProcessor {
    /// .NET category of the processor log.
    pub const CATEGORY: &'static str = "SmilePms.DeviceAgent.Agent.CommandProcessor";

    /// A processor; `options.command_timeout` bounds each adapter call. Without `pos`,
    /// pos.* commands fail with DEVICE_NOT_CONNECTED.
    pub fn new(
        queue: Arc<dyn CommandQueue>,
        adapter: Arc<dyn DeviceAdapter>,
        reporter: ResultReporter,
        options: &AgentOptions,
        log: Arc<dyn Logger>,
        pos: Option<Arc<dyn PosDevice>>,
    ) -> CommandProcessor {
        CommandProcessor {
            queue,
            adapter,
            reporter,
            command_timeout: options.command_timeout,
            log: Log::new(log, Self::CATEGORY),
            pos,
            observer: None,
        }
    }

    /// The same processor, telling `observer` about each claim and outcome.
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> CommandProcessor {
        self.observer = Some(observer);
        self
    }

    /// `RunOnceAsync`: `Ok(false)` when nothing was queued. Fails with
    /// [`AgentError::PendingResults`] (nothing claimed) while an earlier result is owed, and
    /// with [`AgentError::Stopped`] when the service stopped during the command (nothing is
    /// posted then).
    pub fn run_once(&self, stop: &StopToken) -> Result<bool, AgentError> {
        self.reporter.flush(stop)?;
        if stop.is_stopped() {
            // C# SendAsync with the cancelled stop token never sends the claim.
            return Err(AgentError::Stopped);
        }
        let Some(cmd) = self.queue.claim().map_err(AgentError::Queue)? else {
            return Ok(false);
        };

        self.log.info(&format!(
            "Claimed device command {} {} attempt {}",
            cmd.id, cmd.kind, cmd.attempt
        ));
        if let Some(o) = &self.observer {
            o.claimed(&cmd);
        }
        let outcome = self.execute_safely(&cmd, stop)?;
        if let Some(o) = &self.observer {
            o.finished(&cmd, &outcome);
        }
        self.log.info(&format!(
            "Device command {} finished {} {}",
            cmd.id,
            if outcome.ok() { "done" } else { "failed" },
            or_null(outcome.error.as_ref().map(|e| e.code.as_str()))
        ));

        // The device may have acted: from here the stop must not cancel the report.
        self.reporter
            .report(&PendingResult::from_command(&cmd, &outcome), stop)?;
        Ok(true)
    }

    /// Turns adapter failures into structured outcomes; only a service stop propagates.
    fn execute_safely(
        &self,
        cmd: &DeviceCommand,
        stop: &StopToken,
    ) -> Result<CommandOutcome, AgentError> {
        let limit = self.command_timeout;
        let cx = Cx::new(limit.to_duration().unwrap_or(Duration::ZERO), stop.clone());
        let run = catch_unwind(AssertUnwindSafe(|| self.execute(cmd, &cx)));
        let id = cmd.id;
        Ok(match run {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(Failure::Adapter(e))) if Cx::is_cancellation(&e) => {
                if stop.is_stopped() {
                    return Err(AgentError::Stopped);
                }
                self.log.warn(
                    &format!("Device command {id} timed out after {limit}"),
                    None,
                );
                CommandOutcome::failed(
                    codes::TIMEOUT,
                    &format!(
                        "Adapter did not answer within {} s",
                        limit.total_seconds().round()
                    ),
                )
            }
            Ok(Err(Failure::Payload(e))) => {
                self.log.warn(
                    &format!("Device command {id} has a bad payload"),
                    Some(&e.to_dotnet()),
                );
                CommandOutcome::failed(codes::BAD_PAYLOAD, e.message())
            }
            Ok(Err(Failure::Adapter(e))) if e.code == codes::ADAPTER_FAILED => {
                self.log_failed(cmd, exception_of(&e.message));
                CommandOutcome::failed(codes::ADAPTER_FAILED, &e.message)
            }
            Ok(Err(Failure::Adapter(e))) => {
                self.log.warn(
                    &format!(
                        "Adapter {} refused device command {id}: {}",
                        self.family_of(cmd),
                        e.code
                    ),
                    Some(&DotnetError::new(
                        "SmilePms.DeviceAgent.Adapters.DeviceAdapterException",
                        &e.message,
                    )),
                );
                CommandOutcome::failed(&e.code, &e.message)
            }
            Err(panic) => {
                let text = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_owned());
                let message = format!("Panic: {text}");
                self.log_failed(cmd, exception_of(&message));
                CommandOutcome::failed(codes::ADAPTER_FAILED, &message)
            }
        })
    }

    fn log_failed(&self, cmd: &DeviceCommand, exception: DotnetError) {
        self.log.error(
            &format!(
                "Adapter {} failed on device command {}",
                self.family_of(cmd),
                cmd.id
            ),
            Some(&exception),
        );
    }

    fn execute(&self, cmd: &DeviceCommand, cx: &Cx) -> Result<CommandOutcome, Failure> {
        let a = &self.adapter;
        let p = &cmd.payload;
        let result: Value = match cmd.kind.as_str() {
            kinds::LOCK_ISSUE => a.issue(&payload(p)?, cx)?,
            kinds::LOCK_DUPLICATE => a.duplicate(&payload(p)?, cx)?,
            kinds::LOCK_EXTEND => a.extend(&payload(p)?, cx)?,
            kinds::LOCK_READ => a.read(&payload(p)?, cx)?,
            kinds::LOCK_ERASE => a.erase(&payload(p)?, cx)?,
            kinds::LOCK_CANCEL_ALL => a.cancel_all(&payload(p)?, cx)?,
            kinds::POS_PRINT => self.pos()?.print(&payload(p)?, cx)?,
            kinds::POS_KICK => self.pos()?.kick(&payload(p)?, cx)?,
            kinds::POS_DISPLAY => self.pos()?.display(&payload(p)?, cx)?,
            kinds::POS_READ => self.pos()?.read(&payload(p)?, cx)?,
            kinds::POS_SPEAK => self.pos()?.speak(&payload(p)?, cx)?,
            other => {
                return Ok(CommandOutcome::failed(
                    codes::UNSUPPORTED_KIND,
                    &format!(
                        "Kind '{other}' is not supported by adapter '{}'",
                        a.family()
                    ),
                ));
            }
        };
        Ok(CommandOutcome::done(result))
    }

    fn family_of<'a>(&'a self, cmd: &DeviceCommand) -> &'a str {
        if cmd.kind.starts_with("pos.") {
            "pos"
        } else {
            self.adapter.family()
        }
    }

    fn pos(&self) -> Result<&dyn PosDevice, AdapterError> {
        self.pos.as_deref().ok_or_else(|| {
            AdapterError::new(
                codes::NOT_CONNECTED,
                "POS devices are not set up on this agent",
            )
        })
    }
}

fn payload<T: Payload>(p: &Value) -> Result<T, PayloadError> {
    decode_payload(p)
}

/// The logged exception of a DEVICE_ADAPTER_FAILED message `"{Type}: {message}"`.
fn exception_of(message: &str) -> DotnetError {
    match message.split_once(": ") {
        Some((t, m)) if !t.is_empty() && !t.contains(' ') => DotnetError::new(t, m),
        _ => DotnetError::new("System.Exception", message),
    }
}
