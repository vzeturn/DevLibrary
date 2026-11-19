//! Runs `sync.verify` (`Commands/VerifyCommandHandler.cs`, ADR 0126/0128): rebuilds each
//! mirrored table's cloud manifest from the agent's shadow
//! ([`shadow_manifest`](crate::capture::shadow_manifest)), fetches the cloud's own manifest
//! and reports the buckets whose row count or digest differ. It reads only the agent's
//! state and the cloud: no store query, no store write, so it needs no command-log row (a
//! redelivery simply compares again).
//!
//! Left out, and listed with a reason instead of being reported as a mismatch: open-check
//! tables, tables the cloud does not mirror, a table whose shadow holds duplicate key
//! occurrences, a stream with batches still queued or a resnapshot in flight (before or
//! after the comparison), and every table while a full resnapshot is owed.
//!
//! Commands run one at a time on the channel and a dispatched command is handed out again
//! after the dispatch lease, so a run stops starting tables after [`BUDGET`] and abandons a
//! table after [`TABLE_LIMIT`]. A run over every table remembers the first table it did not
//! reach ([`NEXT_TABLE_CURSOR`]) and the next run starts there.
//!
//! Repair (ADR 0128): a mismatch over at most [`SYNC_MANIFEST_KEYS_LIMIT`] buckets is first
//! narrowed to row keys by comparing the shadow with the cloud's keys of those buckets. Up
//! to [`MAX_REPAIR_KEYS`] differing keys are queued in the outbox for the capture's key
//! repair job, which sends each row again or tombstones it when the store no longer has
//! it. A table that cannot be narrowed falls back to a targeted resnapshot that respects
//! the recovery-resnapshot cooldown and is not repeated within [`REPAIR_COOLDOWN`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use super::error::{CommandError, Result};
use super::executor::{reasons, utc_now};
use super::verify_manifest::{CloudManifest, CloudManifestKeys, CloudManifestSource};
use crate::capture::runner::allowlisted;
use crate::capture::shadow_manifest::{self, ROWS_PER_PASS};
use crate::capture::{
    CaptureError, CaptureState, ErrorKind, RuntimeConfigSource, StateCommit, StateFactory,
};
use crate::config::codes;
use crate::config::time::SharedClock;
use crate::contracts::catalog::CatalogTable;
use crate::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, SyncVerifyCommandResult, SyncVerifyPayload,
    SyncVerifySkippedTable, SyncVerifyTableResult, sync_streams, sync_verify_repair as repair,
};
use crate::contracts::{SyncCatalog, WireJson};
use crate::outbox::{RECOVERY_RESNAPSHOT_COOLDOWN, SqliteOutbox};
use crate::reporting::{Logger, Value};
use crate::util::ctx::Ctx;

/// Category of the handler's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.VerifyCommandHandler";
/// No new table starts after this much time in one run.
pub const BUDGET: Duration = Duration::from_secs(15);
/// A single table's comparison is abandoned after this long.
pub const TABLE_LIMIT: Duration = Duration::from_secs(25);
/// A stream resnapshotted this recently is not resnapshotted again by a verify run.
pub const REPAIR_COOLDOWN: Duration = Duration::from_secs(7 * 24 * 3600);
/// Most mismatched buckets of one table narrowed to keys (one cloud request).
pub const SYNC_MANIFEST_KEYS_LIMIT: usize = 64;
/// Most differing keys of one table queued for a resend by key; more falls back to a
/// resnapshot.
pub const MAX_REPAIR_KEYS: usize = 500;
/// Capture-state cursor: the first table the last run over every table did not reach.
pub const NEXT_TABLE_CURSOR: &str = "sync_verify.next_table";

/// Why a table was not compared ([`SyncVerifySkippedTable::reason`]).
pub mod skip_reasons {
    /// Not in the catalog.
    pub const UNKNOWN_TABLE: &str = "unknown_table";
    /// Classified out or without a cloud table.
    pub const NOT_MIRRORED: &str = "not_mirrored";
    /// Outside the cloud's table allowlist.
    pub const NOT_CAPTURED: &str = "not_captured";
    /// An open-check table.
    pub const OPEN_CHECK: &str = "open_check";
    /// The shadow holds duplicate key occurrences.
    pub const DUPLICATE_KEYS: &str = "duplicate_keys";
    /// Batches queued or a resnapshot in flight.
    pub const STREAM_BUSY: &str = "stream_busy";
    /// A full resnapshot is owed.
    pub const RESNAPSHOT_PENDING: &str = "resnapshot_pending";
    /// A shadow key could not be read.
    pub const SHADOW_UNREADABLE: &str = "shadow_unreadable";
    /// The run's budget was spent before the table.
    pub const TIME_BUDGET: &str = "time_budget";
    /// The table took longer than its limit.
    pub const TIME_LIMIT: &str = "time_limit";
    /// Prefix of a cloud manifest failure (`manifest_http_409`, `manifest_unreachable`).
    pub const MANIFEST_PREFIX: &str = "manifest_";
}

/// Row keys held by one side only, or by both with different hashes, ordinal order.
pub fn differing(shadow: &HashMap<String, String>, cloud: &HashMap<String, String>) -> Vec<String> {
    let mut keys: Vec<String> = shadow
        .iter()
        .filter(|(k, h)| cloud.get(*k) != Some(*h))
        .map(|(k, _)| k.clone())
        .chain(cloud.keys().filter(|k| !shadow.contains_key(*k)).cloned())
        .collect();
    keys.sort_by(|a, b| crate::contracts::hashing::ordinal_cmp(a, b));
    keys
}

enum Outcome {
    Compared(SyncVerifyTableResult),
    Skip(String),
}

fn skip(reason: &str) -> Result<Outcome> {
    Ok(Outcome::Skip(reason.to_owned()))
}

fn outbox_error(e: impl std::fmt::Display) -> CommandError {
    CommandError::invalid(e.to_string())
}

fn capture_error(e: CaptureError) -> CommandError {
    match e.kind {
        ErrorKind::Cancelled => CommandError::Cancelled,
        ErrorKind::Argument => CommandError::Argument(e.message),
        _ => CommandError::Invalid(e.message),
    }
}

fn transport_error(e: crate::transport::Error) -> CommandError {
    match e {
        crate::transport::Error::Cancelled => CommandError::Cancelled,
        crate::transport::Error::Agent(a) => CommandError::Agent(a),
        e @ crate::transport::Error::Unexpected { .. } => CommandError::invalid(e.to_string()),
    }
}

/// The table limit cut the work short while the command itself goes on.
fn timed_out(limit: &Ctx, ctx: &Ctx) -> bool {
    limit.is_done() && !ctx.is_done()
}

/// What [`VerifyCommandHandler::new`] needs.
pub struct VerifyParts {
    /// The outbox (queues, resnapshot requests, key repairs).
    pub outbox: Arc<SqliteOutbox>,
    /// Opens the capture state; the handle is dropped after each run.
    pub open_state: StateFactory,
    /// The cloud's manifests.
    pub cloud: Arc<dyn CloudManifestSource>,
    /// Clock (elapsed budget, cooldowns, result times).
    pub clock: SharedClock,
    /// The catalog (the embedded one in production).
    pub catalog: Arc<SyncCatalog>,
    /// The cloud-pushed table allowlist; `None` captures every catalog table.
    pub config: Option<Arc<dyn RuntimeConfigSource>>,
}

/// The `sync.verify` handler.
pub struct VerifyCommandHandler {
    p: VerifyParts,
    log: Logger,
}

impl std::fmt::Debug for VerifyCommandHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifyCommandHandler")
            .finish_non_exhaustive()
    }
}

impl VerifyCommandHandler {
    /// A handler; `log` is the root logger.
    pub fn new(parts: VerifyParts, log: &Logger) -> VerifyCommandHandler {
        VerifyCommandHandler {
            p: parts,
            log: log.for_category(CATEGORY),
        }
    }

    /// Applied with a [`SyncVerifyCommandResult`], or rejected as an invalid payload.
    pub fn execute(&self, command: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let payload = SyncVerifyPayload::from_element(&command.payload);
        let problem = match &payload {
            None => Some("payload is not a sync.verify object"),
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
                at_utc: utc_now(&self.p.clock),
                exec_ms: None,
            });
        };
        let state = (self.p.open_state)().map_err(capture_error)?;
        let result = self.run(command, &p, &*state, ctx)?;
        drop(state);
        Ok(SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result: Some(result.to_element()),
            at_utc: utc_now(&self.p.clock),
            exec_ms: None,
        })
    }

    fn elapsed_since(&self, started: i128) -> Duration {
        let nanos = self.p.clock.now().unix_nanos() - started;
        Duration::from_nanos(u64::try_from(nanos.max(0)).unwrap_or(u64::MAX))
    }

    fn run(
        &self,
        command: &SyncCommand,
        p: &SyncVerifyPayload,
        state: &dyn CaptureState,
        ctx: &Ctx,
    ) -> Result<SyncVerifyCommandResult> {
        let (mut targets, mut skipped) = self.resolve(&p.tables);
        let everything = p.tables.is_empty();
        if everything {
            let first = state
                .get_cursor(NEXT_TABLE_CURSOR, ctx)
                .map_err(capture_error)?;
            targets = rotate(targets, first.as_deref());
        }

        let mut compared: Vec<SyncVerifyTableResult> = Vec::new();
        let mut next: Option<String> = None;
        let started = self.p.clock.now().unix_nanos();
        for table in &targets {
            let outcome = if self.p.outbox.needs_resnapshot().map_err(outbox_error)? {
                Outcome::Skip(skip_reasons::RESNAPSHOT_PENDING.to_owned())
            } else if self.elapsed_since(started) >= BUDGET {
                Outcome::Skip(skip_reasons::TIME_BUDGET.to_owned())
            } else {
                self.compare(state, table, p.buckets, ctx)?
            };
            match outcome {
                Outcome::Compared(done) => compared.push(done),
                Outcome::Skip(reason) => {
                    // A table over the per-table limit is not retried first next time, or
                    // one large table would starve the rest.
                    if reason == skip_reasons::TIME_BUDGET && next.is_none() {
                        next = Some(table.table.clone());
                    }
                    skipped.push(SyncVerifySkippedTable::new(&table.table, &reason));
                }
            }
        }
        if everything {
            let mut cursor = StateCommit::default();
            cursor.cursors.insert(NEXT_TABLE_CURSOR, next);
            state.commit(&cursor, ctx).map_err(capture_error)?;
        }

        let mismatched: Vec<&SyncVerifyTableResult> = compared
            .iter()
            .filter(|t| !t.mismatched_buckets.is_empty())
            .collect();
        let unnarrowed: Vec<&str> = mismatched
            .iter()
            .filter(|t| t.repair_keys == 0)
            .map(|t| t.table.as_str())
            .collect();
        let (mut repair, scheduled) = self.repair(command, &unnarrowed)?;
        if !mismatched.is_empty() && unnarrowed.is_empty() {
            repair = repair::KEYS_QUEUED.to_owned();
        }
        let mismatched_tables: Vec<String> = mismatched.iter().map(|t| t.table.clone()).collect();
        let mismatched_buckets: usize = mismatched.iter().map(|t| t.mismatched_buckets.len()).sum();
        let tables: Vec<SyncVerifyTableResult> = compared
            .into_iter()
            .map(|t| SyncVerifyTableResult {
                repair_scheduled: t.repair_keys > 0 || scheduled.contains(&t.table),
                ..t
            })
            .collect();
        let result = SyncVerifyCommandResult {
            buckets: p.buckets,
            store_rows: tables.iter().map(|t| t.store_rows).sum(),
            cloud_rows: tables.iter().map(|t| t.cloud_rows).sum(),
            mismatched_tables: i32::try_from(mismatched_tables.len()).unwrap_or(i32::MAX),
            mismatched_buckets: i32::try_from(mismatched_buckets).unwrap_or(i32::MAX),
            tables,
            skipped,
            repair,
        };
        // Information only: the cloud records the run and raises SYNC_VERIFY_MISMATCH
        // itself; a warning here would be a second site event for the same finding.
        self.log.info(
            "sync.verify {CommandId}: {Compared} tables compared, {Mismatched} mismatched ({Buckets} buckets: {Tables}), {Skipped} skipped; repair {Repair}",
            &[
                Value::from(command.command_id),
                Value::from(result.tables.len()),
                Value::from(mismatched_tables.len()),
                Value::from(result.mismatched_buckets),
                Value::from(if mismatched_tables.is_empty() {
                    "none".to_owned()
                } else {
                    mismatched_tables.join(", ")
                }),
                Value::from(result.skipped.len()),
                Value::from(result.repair.as_str()),
            ],
        );
        Ok(result)
    }

    /// Catalog tables to compare (open-check tables listed as skipped), in catalog or
    /// request order. Only tables the agent captures under the cloud's table allowlist: the
    /// shadow of any other table is empty by design.
    fn resolve(&self, requested: &[String]) -> (Vec<CatalogTable>, Vec<SyncVerifySkippedTable>) {
        let catalog = &*self.p.catalog;
        let allow = self.p.config.as_ref().map(|c| c.current());
        let allow_list = allow.as_ref().and_then(|c| c.table_allowlist.as_deref());
        let captured: HashSet<String> = allowlisted(catalog, allow_list)
            .tables
            .iter()
            .map(|t| t.table.to_lowercase())
            .collect();
        let is_captured = |t: &CatalogTable| captured.contains(&t.table.to_lowercase());
        let mut skipped = Vec::new();
        let mut candidates: Vec<&CatalogTable> = Vec::new();
        if requested.is_empty() {
            candidates.extend(
                catalog
                    .classified()
                    .filter(|t| t.has_pg_target() && is_captured(t)),
            );
        }
        let mut seen: HashSet<String> = HashSet::new();
        for name in requested {
            if !seen.insert(name.to_lowercase()) {
                continue;
            }
            match catalog.find(name) {
                None => skipped.push(SyncVerifySkippedTable::new(
                    name,
                    skip_reasons::UNKNOWN_TABLE,
                )),
                Some(t) if !t.is_classified() || !t.has_pg_target() => {
                    skipped.push(SyncVerifySkippedTable::new(
                        &t.table,
                        skip_reasons::NOT_MIRRORED,
                    ));
                }
                Some(t) if !is_captured(t) => {
                    skipped.push(SyncVerifySkippedTable::new(
                        &t.table,
                        skip_reasons::NOT_CAPTURED,
                    ));
                }
                Some(t) => {
                    if !candidates.iter().any(|c| c.table == t.table) {
                        candidates.push(t);
                    }
                }
            }
        }
        let mut targets = Vec::new();
        for t in candidates {
            if t.capture.as_deref() == Some(sync_streams::OPEN_CAPTURE) {
                skipped.push(SyncVerifySkippedTable::new(
                    &t.table,
                    skip_reasons::OPEN_CHECK,
                ));
            } else {
                targets.push(t.clone());
            }
        }
        (targets, skipped)
    }

    fn compare(
        &self,
        state: &dyn CaptureState,
        table: &CatalogTable,
        buckets: i32,
        ctx: &Ctx,
    ) -> Result<Outcome> {
        let outbox = &self.p.outbox;
        let stream = sync_streams::of(table);
        let seq = outbox.last_seq(stream).map_err(outbox_error)?;
        if outbox.stream_unsettled(stream).map_err(outbox_error)? {
            return skip(skip_reasons::STREAM_BUSY);
        }

        let limit = ctx.with_timeout(TABLE_LIMIT);
        let name = table.table.as_str();
        let store = match shadow_manifest::build(state, name, buckets, ROWS_PER_PASS, &limit) {
            Ok(s) => s,
            Err(e) if e.is_cancelled() && timed_out(&limit, ctx) => return Ok(self.too_long(name)),
            Err(e) if matches!(e.kind, ErrorKind::Format | ErrorKind::Argument) => {
                self.log.warn_ex(
                    e.to_exception(),
                    "sync.verify left {Table} out: a shadow key of the table cannot be read",
                    &[Value::from(name)],
                );
                return skip(skip_reasons::SHADOW_UNREADABLE);
            }
            Err(e) => return Err(capture_error(e)),
        };
        if store.duplicate_keys {
            return skip(skip_reasons::DUPLICATE_KEYS);
        }
        let remote: CloudManifest = match self.p.cloud.get(name, buckets, &limit) {
            Ok(m) => m,
            Err(crate::transport::Error::Cancelled) if timed_out(&limit, ctx) => {
                return Ok(self.too_long(name));
            }
            Err(e) => return Err(transport_error(e)),
        };
        if let Some(error) = &remote.error {
            if error == "unreachable" {
                self.log.info(
                    "sync.verify left {Table} out: the cloud manifest is unreachable",
                    &[Value::from(name)],
                );
            } else {
                self.log.warn(
                    "sync.verify left {Table} out: the cloud manifest answered {Error}",
                    &[Value::from(name), Value::from(error.as_str())],
                );
            }
            return Ok(Outcome::Skip(format!(
                "{}{error}",
                skip_reasons::MANIFEST_PREFIX
            )));
        }
        // A batch queued meanwhile means the shadow moved during the comparison.
        if outbox.last_seq(stream).map_err(outbox_error)? != seq
            || outbox.stream_unsettled(stream).map_err(outbox_error)?
        {
            return skip(skip_reasons::STREAM_BUSY);
        }

        let mismatched = shadow_manifest::mismatched(&store.buckets, &remote.buckets);
        let repair_keys = if mismatched.is_empty() {
            0
        } else {
            self.narrow(state, name, buckets, &mismatched, &limit, ctx)?
        };
        Ok(Outcome::Compared(SyncVerifyTableResult {
            table: name.to_owned(),
            store_rows: store.rows,
            cloud_rows: remote.buckets.iter().map(|b| b.count).sum(),
            mismatched_buckets: mismatched,
            repair_scheduled: false,
            repair_keys: i32::try_from(repair_keys).unwrap_or(i32::MAX),
        }))
    }

    fn too_long(&self, table: &str) -> Outcome {
        self.log.warn(
            "sync.verify left {Table} out: comparing it took longer than {Limit}",
            &[Value::from(table), Value::from("00:00:25")],
        );
        Outcome::Skip(skip_reasons::TIME_LIMIT.to_owned())
    }

    /// Narrows mismatched buckets to the row keys that differ and queues them for a resend
    /// by key (ADR 0128). Returns how many keys were queued; 0 means the table falls back
    /// to a resnapshot.
    fn narrow(
        &self,
        state: &dyn CaptureState,
        table: &str,
        buckets: i32,
        mismatched: &[i32],
        limit: &Ctx,
        ctx: &Ctx,
    ) -> Result<usize> {
        if mismatched.len() > SYNC_MANIFEST_KEYS_LIMIT {
            return Ok(0);
        }
        let fallback = |why: &str| {
            self.log.info(
                "sync.verify could not narrow {Table} to keys ({Why}); falling back to a resnapshot",
                &[Value::from(table), Value::from(why)],
            );
        };
        let wanted: HashSet<i32> = mismatched.iter().copied().collect();
        let shadow = match shadow_manifest::bucket_rows(state, table, buckets, &wanted, limit) {
            Ok(Some(s)) => s,
            Ok(None) => return Ok(0),
            Err(e) if e.is_cancelled() && timed_out(limit, ctx) => {
                fallback("time limit");
                return Ok(0);
            }
            Err(e) if matches!(e.kind, ErrorKind::Format | ErrorKind::Argument) => {
                self.log.warn_ex(
                    e.to_exception(),
                    "sync.verify could not narrow {Table} to keys: a shadow key cannot be read",
                    &[Value::from(table)],
                );
                return Ok(0);
            }
            Err(e) => return Err(capture_error(e)),
        };
        let remote: CloudManifestKeys =
            match self.p.cloud.get_keys(table, buckets, mismatched, limit) {
                Ok(k) => k,
                Err(crate::transport::Error::Cancelled) if timed_out(limit, ctx) => {
                    fallback("time limit");
                    return Ok(0);
                }
                Err(e) => return Err(transport_error(e)),
            };
        if remote.error.is_some() || remote.truncated {
            fallback(remote.error.as_deref().unwrap_or("truncated"));
            return Ok(0);
        }
        let keys = differing(&shadow, &remote.rows);
        if keys.is_empty() || keys.len() > MAX_REPAIR_KEYS {
            self.log.info(
                "sync.verify could not narrow {Table} to keys ({Count} differ); falling back to a resnapshot",
                &[Value::from(table), Value::from(keys.len())],
            );
            return Ok(0);
        }
        let added = self
            .p
            .outbox
            .queue_key_repairs(table, &keys, utc_now(&self.p.clock))
            .map_err(outbox_error)?;
        self.log.info(
            "sync.verify queued {Count} key(s) of {Table} for a resend by key ({Added} new)",
            &[
                Value::from(keys.len()),
                Value::from(table),
                Value::from(added),
            ],
        );
        Ok(keys.len())
    }

    /// Schedules a targeted resnapshot of the mismatched streams, respecting both
    /// cooldowns.
    fn repair(
        &self,
        command: &SyncCommand,
        mismatched: &[&str],
    ) -> Result<(String, HashSet<String>)> {
        if mismatched.is_empty() {
            return Ok((repair::NONE.to_owned(), HashSet::new()));
        }
        let outbox = &self.p.outbox;
        let now = utc_now(&self.p.clock);
        let ticks = |d: Duration| i64::try_from(d.as_nanos() / 100).unwrap_or(i64::MAX);
        let cooling = outbox
            .last_recovery_resnapshot()
            .map_err(outbox_error)?
            .is_some_and(|last| now.ticks_since(last) < ticks(RECOVERY_RESNAPSHOT_COOLDOWN));
        if cooling
            || outbox
                .recovery_resnapshot_deferred()
                .map_err(outbox_error)?
        {
            self.log.info(
                "sync.verify {CommandId}: repair held back by the recovery-resnapshot cooldown",
                &[Value::from(command.command_id)],
            );
            return Ok((repair::COOLDOWN.to_owned(), HashSet::new()));
        }
        let mut due: Vec<&str> = Vec::new();
        for s in mismatched {
            let done = outbox
                .stream_resnapshot_finished_at(s)
                .map_err(outbox_error)?;
            if done.is_none_or(|d| now.ticks_since(d) >= ticks(REPAIR_COOLDOWN)) {
                due.push(s);
            }
        }
        if due.is_empty() {
            self.log.info(
                "sync.verify {CommandId}: every mismatched stream was resnapshotted within {Cooldown}; not repeated",
                &[
                    Value::from(command.command_id),
                    Value::from("7.00:00:00"),
                ],
            );
            return Ok((repair::COOLDOWN.to_owned(), HashSet::new()));
        }
        let why = format!(
            "sync verify {}: {} differ from the cloud",
            command.command_id,
            due.join(", ")
        );
        let request = outbox
            .request_stream_resnapshot(&due, &why, now)
            .map_err(outbox_error)?;
        if request.added.is_empty() {
            return Ok((repair::ALREADY_PENDING.to_owned(), HashSet::new()));
        }
        let pending = if request.already_pending.is_empty() {
            "none".to_owned()
        } else {
            request.already_pending.join(", ")
        };
        self.log.warn(
            "Stream resnapshot of {Streams} scheduled ({Code}) by {Why}; already pending: {AlreadyPending}",
            &[
                Value::from(request.added.join(", ")),
                Value::from(codes::SYNC_STREAM_RESNAPSHOT),
                Value::from(why.as_str()),
                Value::from(pending),
            ],
        );
        Ok((
            repair::SCHEDULED.to_owned(),
            request.added.into_iter().collect(),
        ))
    }
}

/// The targets starting at `first` (catalog order otherwise).
fn rotate(targets: Vec<CatalogTable>, first: Option<&str>) -> Vec<CatalogTable> {
    let at = first.and_then(|f| targets.iter().position(|t| t.table == f));
    match at {
        None | Some(0) => targets,
        Some(i) => {
            let mut out = targets[i..].to_vec();
            out.extend_from_slice(&targets[..i]);
            out
        }
    }
}
