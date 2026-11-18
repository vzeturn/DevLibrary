//! `Agent/ResultReporter.cs`: delivers results after the adapter has acted. Each result goes
//! to the [`ResultOutbox`] first, then is sent (never cancelled by the service stop),
//! retried with backoff while the claim's lease lasts, and otherwise left in the outbox for
//! [`ResultReporter::flush`] before the next claim. The API accepts a late result from the
//! last holder while no newer claim exists (migration 172).

use std::sync::Arc;
use std::time::Duration;

use dl_json::{Value, json};
use dl_uuid::Uuid;

use super::clock::{Clock, add_duration};
use super::log::{Log, Logger, or_null};
use super::{AgentError, PendingResultsError, ResultOutbox, StopToken};
use crate::config::DotnetError;
use crate::contracts::PendingResult;
use crate::queue::{CommandQueue, QueueError, StaleClaimReason};

const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(8);

/// Sends results to the queue, through the outbox.
pub struct ResultReporter {
    queue: Arc<dyn CommandQueue>,
    outbox: ResultOutbox,
    clock: Arc<dyn Clock>,
    log: Log,
}

impl ResultReporter {
    /// .NET category of the reporter log.
    pub const CATEGORY: &'static str = "SmilePms.DeviceAgent.Agent.ResultReporter";

    /// `ResultReporter.SendTimeout`: the C# upper bound on one send. Here a send is bounded
    /// by the queue's own HTTP timeout (15 s), which normally ended it first in C# too.
    pub const SEND_TIMEOUT: Duration = Duration::from_secs(20);

    /// A reporter over `queue` and `outbox`.
    pub fn new(
        queue: Arc<dyn CommandQueue>,
        outbox: ResultOutbox,
        clock: Arc<dyn Clock>,
        log: Arc<dyn Logger>,
    ) -> ResultReporter {
        ResultReporter {
            queue,
            outbox,
            clock,
            log: Log::new(log, Self::CATEGORY),
        }
    }

    /// `ReportAsync`: `Ok(true)` when the API took the result or refused it for good,
    /// `Ok(false)` when it is left in the outbox. A stop ends the retries (after at least
    /// one send) but never the first send. Errors are the ones not worth a retry (token
    /// file, malformed request), like the C# exceptions that escape `TrySendAsync`.
    pub fn report(&self, result: &PendingResult, stop: &StopToken) -> Result<bool, AgentError> {
        let durable = self.try_save(result);
        let mut delay = FIRST_RETRY_DELAY;
        let mut attempt = 1;
        loop {
            if self.try_send(result, attempt)? {
                return Ok(true);
            }
            if stop.is_stopped()
                || add_duration(self.clock.now(), delay) >= result.lease_until
                || !self.clock.sleep(delay, stop)
            {
                break;
            }
            delay = (delay * 2).min(MAX_RETRY_DELAY);
            attempt += 1;
        }

        if durable {
            self.log.warn(
                &format!(
                    "Result for device command {} not delivered yet; kept in the outbox for the next poll",
                    result.command_id
                ),
                None,
            );
        } else {
            self.log.critical(
                &format!(
                    "Result for device command {} could neither be delivered nor saved: {}",
                    result.command_id,
                    describe(result)
                ),
                None,
            );
        }
        Ok(false)
    }

    /// `FlushAsync`: sends every result left in the outbox once; fails with
    /// [`AgentError::PendingResults`] while any is still owed, so the agent does not claim.
    pub fn flush(&self, stop: &StopToken) -> Result<(), AgentError> {
        let pending = self.outbox.list().map_err(AgentError::io)?;
        let mut left = 0;
        for result in &pending {
            if stop.is_stopped() {
                return Err(AgentError::Stopped);
            }
            if !self.try_send(result, 0)? {
                left += 1;
            }
        }
        if left > 0 {
            return Err(AgentError::PendingResults(PendingResultsError {
                pending: left,
            }));
        }
        Ok(())
    }

    fn try_save(&self, result: &PendingResult) -> bool {
        match self.outbox.save(result) {
            Ok(()) => true,
            Err(e) => {
                self.log.error(
                    &format!(
                        "Could not write the result of device command {} to the outbox; sending it anyway",
                        result.command_id
                    ),
                    Some(&AgentError::io(e).to_dotnet()),
                );
                false
            }
        }
    }

    /// `Ok(true)` when the API answered for good (the outbox entry is removed), `Ok(false)`
    /// when the send should be retried.
    fn try_send(&self, result: &PendingResult, attempt: u32) -> Result<bool, AgentError> {
        let id = result.command_id;
        match self.queue.send_result(result) {
            Ok(()) => self.log.info(&format!(
                "Device command {id} result {} {} delivered",
                result.body.outcome,
                or_null(result.body.error.as_ref().map(|e| e.code.as_str()))
            )),
            Err(QueueError::StaleClaim(stale)) => {
                let ex = QueueError::StaleClaim(stale.clone()).to_dotnet();
                self.log_refused(stale.reason, result, &ex);
            }
            Err(e) => {
                let invalid =
                    matches!(&e, QueueError::Api(api) if api.status == 400 || api.status == 422);
                let retry = matches!(
                    &e,
                    QueueError::Transport(_) | QueueError::Api(_) | QueueError::Timeout
                );
                if invalid {
                    // The body itself is invalid: resending it can never succeed.
                    self.log.error(
                        &format!(
                            "API refused the result of device command {id} as invalid; dropped: {}",
                            describe(result)
                        ),
                        Some(&e.to_dotnet()),
                    );
                } else if retry {
                    // Unreachable API, 5xx, 401/403/429 or a send timeout: worth another try.
                    self.log.warn(
                        &format!(
                            "Result for device command {id} not delivered (attempt {attempt})"
                        ),
                        Some(&e.to_dotnet()),
                    );
                    return Ok(false);
                } else {
                    return Err(AgentError::Queue(e));
                }
            }
        }
        self.remove(id);
        Ok(true)
    }

    fn log_refused(&self, reason: StaleClaimReason, result: &PendingResult, ex: &DotnetError) {
        let id = result.command_id;
        let d = describe(result);
        match reason {
            StaleClaimReason::OutcomeConflict => self.log.warn(
                &format!(
                    "Device command {id} was already completed with the other outcome; ours dropped: {d}"
                ),
                Some(ex),
            ),
            StaleClaimReason::Missing => self.log.error(
                &format!("Device command {id} no longer exists; its result is dropped: {d}"),
                Some(ex),
            ),
            StaleClaimReason::NotHeld => self.log.error(
                &format!(
                    "Device command {id} has a newer claim; the device acted but this result is dropped: {d}"
                ),
                Some(ex),
            ),
        }
    }

    fn remove(&self, id: Uuid) {
        if let Err(e) = self.outbox.remove(id) {
            // Harmless: the next flush resends it and the API answers a repeat idempotently.
            self.log.error(
                &format!("Could not remove the outbox entry of device command {id}"),
                Some(&AgentError::io(e).to_dotnet()),
            );
        }
    }
}

/// The result for an operator to recover by hand. It holds card data, never the token.
fn describe(result: &PendingResult) -> String {
    let error = result.body.error.as_ref().map_or(
        Value::Null,
        |e| json!({"code": e.code.as_str(), "message": e.message.as_str()}),
    );
    crate::contracts::to_dotnet_json(&json!({
        "commandId": result.command_id.to_string(),
        "kind": result.kind.as_str(),
        "outcome": result.body.outcome.as_str(),
        "result": result.body.result.clone().unwrap_or(Value::Null),
        "error": error,
    }))
}
