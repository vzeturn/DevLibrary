//! Runs `sync.resnapshot` (`ResnapshotCommandHandler.cs`, ADR 0102): schedules a targeted
//! resnapshot of the named streams in the outbox; capture resends them on its next
//! session. It touches no store table, so it needs no command-log row: a redelivery finds
//! the streams already pending, and resending rows is idempotent cloud-side anyway.
//!
//! The command is how an operator or the cloud overrides the agent's own pacing: it skips
//! the 6 h recovery-resnapshot cooldown once, and says so in a `SYNC_STREAM_RESNAPSHOT`
//! warning with the command id and reason. A stream the catalog does not know makes it a
//! full resnapshot, which then restarts the cooldown.

use std::collections::HashMap;
use std::sync::Arc;

use super::error::{CommandError, Result};
use super::executor::{reasons, utc_now};
use crate::config::codes;
use crate::config::time::SharedClock;
use crate::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, SyncResnapshotPayload, SyncResnapshotResult,
    sync_streams,
};
use crate::contracts::{SyncCatalog, WireJson};
use crate::outbox::{RECOVERY_RESNAPSHOT_COOLDOWN, SqliteOutbox};
use crate::reporting::{Logger, Value};

/// Category of the handler's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.ResnapshotCommandHandler";

/// The `sync.resnapshot` handler.
pub struct ResnapshotCommandHandler {
    outbox: Arc<SqliteOutbox>,
    clock: SharedClock,
    log: Logger,
    /// Wire stream names, keyed case-insensitively (lowercase) to the catalog spelling.
    streams: HashMap<String, String>,
}

fn outbox_error(e: impl std::fmt::Display) -> CommandError {
    CommandError::invalid(e.to_string())
}

impl ResnapshotCommandHandler {
    /// A handler over `outbox` knowing the streams of `catalog`; `log` is the root logger.
    pub fn new(
        outbox: Arc<SqliteOutbox>,
        clock: SharedClock,
        log: &Logger,
        catalog: &SyncCatalog,
    ) -> ResnapshotCommandHandler {
        let mut streams = HashMap::new();
        for table in catalog.classified() {
            let stream = sync_streams::of(table);
            streams
                .entry(stream.to_lowercase())
                .or_insert_with(|| stream.to_owned());
        }
        ResnapshotCommandHandler {
            outbox,
            clock,
            log: log.for_category(CATEGORY),
            streams,
        }
    }

    /// Applied with a [`SyncResnapshotResult`], or rejected as an invalid payload.
    pub fn execute(&self, command: &SyncCommand) -> Result<SyncCommandResult> {
        let payload = SyncResnapshotPayload::from_element(&command.payload);
        let problem = match &payload {
            None => Some("payload is not a sync.resnapshot object"),
            Some(p) => p.problem(),
        };
        let (Some(p), None) = (payload, problem) else {
            return Ok(SyncCommandResult {
                command_id: command.command_id,
                status: CommandStatus::Rejected,
                reason: Some(format!(
                    "{}: {}",
                    reasons::INVALID_PAYLOAD,
                    problem.unwrap_or("invalid")
                )),
                result: None,
                at_utc: utc_now(&self.clock),
                exec_ms: None,
                hops: Default::default(),
            });
        };
        let result = self.schedule(command, &p)?;
        Ok(SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: Some(result.to_element()),
            at_utc: utc_now(&self.clock),
            exec_ms: None,
            hops: Default::default(),
        })
    }

    fn schedule(
        &self,
        command: &SyncCommand,
        p: &SyncResnapshotPayload,
    ) -> Result<SyncResnapshotResult> {
        let now = utc_now(&self.clock);
        let cooldown_ticks =
            i64::try_from(RECOVERY_RESNAPSHOT_COOLDOWN.as_nanos() / 100).unwrap_or(i64::MAX);
        let cooling = self
            .outbox
            .last_recovery_resnapshot()
            .map_err(outbox_error)?
            .is_some_and(|last| now.ticks_since(last) < cooldown_ticks);
        let bypassed = cooling
            || self
                .outbox
                .recovery_resnapshot_deferred()
                .map_err(outbox_error)?;
        let why = format!("cloud command {}: {}", command.command_id, p.reason);
        let unknown: Vec<&str> = p
            .streams
            .iter()
            .filter(|s| !self.streams.contains_key(&s.to_lowercase()))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            let unknown = unknown.join(", ");
            self.outbox
                .request_recovery_resnapshot(
                    &format!("stream resnapshot of unknown streams {unknown} ({why})"),
                    now,
                )
                .map_err(outbox_error)?;
            self.log.warn(
                "Full resnapshot scheduled ({Code}) by {Why}: streams {Unknown} are not in the catalog; cooldown bypassed: {Bypassed}",
                &[
                    Value::from(codes::SYNC_STREAM_RESNAPSHOT),
                    Value::from(why.as_str()),
                    Value::from(unknown),
                    Value::from(bypassed),
                ],
            );
            return Ok(SyncResnapshotResult {
                streams: Vec::new(),
                full: true,
                cooldown_bypassed: bypassed,
                already_pending: false,
            });
        }

        let mut streams: Vec<String> = Vec::new();
        for s in &p.streams {
            if let Some(known) = self.streams.get(&s.to_lowercase())
                && !streams.contains(known)
            {
                streams.push(known.clone());
            }
        }
        let request = self
            .outbox
            .request_stream_resnapshot(&streams, &why, now)
            .map_err(outbox_error)?;
        let pending = if request.already_pending.is_empty() {
            "none".to_owned()
        } else {
            request.already_pending.join(", ")
        };
        self.log.warn(
            "Stream resnapshot of {Streams} scheduled ({Code}) by {Why}; already pending: {AlreadyPending}; cooldown bypassed: {Bypassed}",
            &[
                Value::from(streams.join(", ")),
                Value::from(codes::SYNC_STREAM_RESNAPSHOT),
                Value::from(why.as_str()),
                Value::from(pending),
                Value::from(bypassed),
            ],
        );
        Ok(SyncResnapshotResult {
            streams,
            full: false,
            cooldown_bypassed: bypassed,
            already_pending: request.added.is_empty(),
        })
    }
}
