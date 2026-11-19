//! Executes the site commands of contract v1 against the store database
//! (`SiteCommandExecutor.cs`, `.Orders.cs`, `.Tables.cs`, `.Combo.cs`; sync S9, ADR 0029).
//! Each command runs in one SQL transaction with its `Pda_SyncCommandLog` row, so a
//! redelivered command id returns the logged result instead of writing twice. A command
//! the agent cannot run (unknown type, invalid payload) is rejected without touching the
//! database; a business refusal (check closed, reservation gone) is logged like an
//! applied command so a replay answers the same.

use std::collections::HashSet;
use std::sync::Arc;

use dl_json::Value as Json;
use dl_uuid::Uuid;

use super::error::{CommandError, Result};
use super::resnapshot::ResnapshotCommandHandler;
use super::schema_guard::TableSchemaGuard;
use super::units::{CommandWork, SiteCommandStore, SiteCommandUnit};
use crate::capture::CaptureNudge;
use crate::config::time::SharedClock;
use crate::contracts::WireJson;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::{
    CheckStateFingerprint, CommandStatus, KdsLineStatePayload, KdsLineStateResult,
    OrderCheckExpectation, OrderCheckMergePayload, OrderCheckOpenPayload, OrderCheckResult,
    OrderComboPostPayload, OrderCoversPayload, OrderItemPostPayload, OrderItemVoidPayload,
    OrderItemsMovePayload, OrderMultiCheckResult, OrderSendPayload, OrderTableChangePayload,
    OrderTableTransferPayload, ReservationCancelPayload, ReservationSavePayload,
    ReservationSavedResult, SyncCommand, SyncCommandResult, check_aggregate, kds_line_actions,
    order_command_reasons as order, order_command_types, site_command_reasons, site_command_types,
    table_command_reasons as table, table_command_types,
};
use crate::reporting::{Logger, Value};
use crate::util::ctx::Ctx;

/// Rejection reasons the cloud stores in `sync.commands.last_error`
/// (`SiteCommandExecutor.Reasons`).
pub mod reasons {
    /// The agent does not know the command type.
    pub const UNKNOWN_TYPE: &str = "unknown_command_type";
    /// The payload is not a valid instance (`invalid_payload: <problem>`).
    pub const INVALID_PAYLOAD: &str = "invalid_payload";
    /// The check is not open in the RVC.
    pub const CHECK_NOT_FOUND: &str = "check_not_found";
    /// The KDS accept columns are missing on `tmpTrnInfo`.
    pub const KDS_ACCEPT_UNSUPPORTED: &str = "kds_accept_unsupported";
    /// Unaccept of lines none of which was accepted.
    pub const NOT_ACCEPTED: &str = "not_accepted";
    /// Undone of lines none of which was done.
    pub const NOT_DONE: &str = "not_done";
    /// The reservation is not in the RVC.
    pub const RESERVATION_NOT_FOUND: &str = "reservation_not_found";
    /// The check in SMILE_POS no longer matches the expected state the user saw.
    pub const CHECK_CHANGED: &str = super::site_command_reasons::CHECK_CHANGED;
    /// An expected state was sent for an aggregate that is not a check.
    pub const NOT_A_CHECK: &str = "expected_hash_without_check";
}

/// Category of the executor's log lines.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Commands.SiteCommandExecutor";

/// What the command channel runs a command with (`ICommandExecutor`).
pub trait CommandExecutor: Send + Sync {
    /// Runs `command` once and returns its outcome; an error is a failed command the
    /// cloud retries.
    fn execute(&self, command: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult>;

    /// The outcome already recorded for `command_id` (the store's command log); `None`
    /// when it never ran. Consulted before an expired command is refused.
    fn find_recorded(&self, _command_id: Uuid, _ctx: &Ctx) -> Result<Option<SyncCommandResult>> {
        Ok(None)
    }
}

/// Safe stand-in with no store behind it: refuses everything without side effects
/// (`RejectingCommandExecutor`).
pub struct RejectingCommandExecutor {
    clock: SharedClock,
}

impl RejectingCommandExecutor {
    /// A rejecting executor.
    pub fn new(clock: SharedClock) -> RejectingCommandExecutor {
        RejectingCommandExecutor { clock }
    }
}

impl CommandExecutor for RejectingCommandExecutor {
    fn execute(&self, command: &SyncCommand, _ctx: &Ctx) -> Result<SyncCommandResult> {
        Ok(SyncCommandResult {
            command_id: command.command_id,
            status: CommandStatus::Rejected,
            reason: Some("executor_not_available".to_owned()),
            result: None,
            at_utc: utc_now(&self.clock),
            exec_ms: None,
        })
    }
}

/// Re-checks the order grants after SQL Server refused an order verb (the
/// `OrderGrantsMonitor` seam): tells the operator once per episode.
pub trait GrantsCheck: Send + Sync {
    /// Probes the grants now.
    fn check(&self, ctx: &Ctx);
}

/// `clock.GetUtcNow()` as a wire `DateTimeOffset` (offset 0).
pub(crate) fn utc_now(clock: &SharedClock) -> DateTimeOffset {
    DateTimeOffset::from_unix_nanos(clock.now().unix_nanos(), 0)
        .unwrap_or(DateTimeOffset::UNIX_EPOCH)
}

/// Whether `type_` is an order verb: refused at once on a missing grant and nudging
/// capture.
pub fn is_order_verb(type_: &str) -> bool {
    order_command_types::ALL.contains(&type_) || table_command_types::ALL.contains(&type_)
}

/// The site command executor.
pub struct SiteCommandExecutor {
    store: Arc<dyn SiteCommandStore>,
    clock: SharedClock,
    log: Logger,
    resnapshot: Option<Arc<ResnapshotCommandHandler>>,
    nudge: Option<Arc<dyn CaptureNudge>>,
    grants: Option<Arc<dyn GrantsCheck>>,
    schema: Arc<TableSchemaGuard>,
}

struct LockedCheck {
    check_no: i64,
    status: Option<i32>,
    matches: bool,
}

impl SiteCommandExecutor {
    /// An executor over `store`; `log` is the root logger. Without
    /// [`with_schema`](Self::with_schema) the table verbs still probe the schema and only
    /// log a refusal.
    pub fn new(store: Arc<dyn SiteCommandStore>, clock: SharedClock, log: &Logger) -> Self {
        let log = log.for_category(CATEGORY);
        let schema = Arc::new(TableSchemaGuard::log_only(&log));
        SiteCommandExecutor {
            store,
            clock,
            log,
            resnapshot: None,
            nudge: None,
            grants: None,
            schema,
        }
    }

    /// Runs `sync.resnapshot` with `handler` (without one the type is unknown).
    pub fn with_resnapshot(mut self, handler: Arc<ResnapshotCommandHandler>) -> Self {
        self.resnapshot = Some(handler);
        self
    }

    /// Nudges capture after an applied order verb.
    pub fn with_nudge(mut self, nudge: Arc<dyn CaptureNudge>) -> Self {
        self.nudge = Some(nudge);
        self
    }

    /// Re-checks the grants after a permission error.
    pub fn with_grants(mut self, grants: Arc<dyn GrantsCheck>) -> Self {
        self.grants = Some(grants);
        self
    }

    /// The host's schema guard (reports `SCHEMA_UNSUPPORTED` site events).
    pub fn with_schema(mut self, schema: Arc<TableSchemaGuard>) -> Self {
        self.schema = schema;
        self
    }

    /// Runs `command` (`ExecuteAsync`).
    pub fn execute(&self, command: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let result = match self.dispatch(command, ctx) {
            Ok(r) => r,
            Err(e) if is_order_verb(&command.type_) && e.is_permission_denied() => {
                // Terminal, not retried to dead (GR1): the transaction rolled back and
                // nothing was written, so a replay after `grants` applies normally.
                self.log.warn_ex(
                    e.to_exception(),
                    "Site command {Id} ({Type}) refused by SQL Server: pda_sync lacks a grant (error {Number})",
                    &[
                        Value::from(command.command_id),
                        Value::from(command.type_.as_str()),
                        Value::from(e.sql_number().unwrap_or(0)),
                    ],
                );
                if let Some(g) = &self.grants {
                    g.check(ctx);
                }
                self.reject(command, order::SQL_GRANTS_MISSING)
            }
            Err(e) => return Err(e),
        };
        self.log.info(
            "Site command {Id} ({Type}) {Status} {Reason}",
            &[
                Value::from(command.command_id),
                Value::from(command.type_.as_str()),
                Value::from(result.status.name()),
                result.reason.as_deref().map_or(Value::Null, Value::from),
            ],
        );
        // Read-your-writes (ADR 0104 §4): an applied order verb asks capture to read the
        // open checks now, so the cloud mirror reaches the result's fingerprint quickly.
        if result.status == CommandStatus::Applied
            && is_order_verb(&command.type_)
            && let Some(n) = &self.nudge
        {
            n.nudge_open_checks();
        }
        Ok(result)
    }

    /// The outcome logged for `command_id` (`FindRecordedAsync`).
    pub fn find_recorded(&self, command_id: Uuid, ctx: &Ctx) -> Result<Option<SyncCommandResult>> {
        self.store.find_recorded(command_id, ctx)
    }

    fn dispatch(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        use order_command_types as o;
        use site_command_types as s;
        use table_command_types as t;
        match c.type_.as_str() {
            s::KDS_LINE_STATE => self.kds_line_state(c, ctx),
            s::RESERVATION_SAVE => self.reservation_save(c, ctx),
            s::RESERVATION_CANCEL => self.reservation_cancel(c, ctx),
            s::SYNC_RESNAPSHOT => match &self.resnapshot {
                Some(h) => h.execute(c),
                None => Ok(self.reject(c, reasons::UNKNOWN_TYPE)),
            },
            o::CHECK_OPEN => self.check_open(c, ctx),
            o::ITEM_POST => self.item_post(c, ctx),
            o::SEND => self.send(c, ctx),
            o::ITEM_VOID => self.item_void(c, ctx),
            o::COVERS => self.covers(c, ctx),
            o::COMBO_POST => self.combo_post(c, ctx),
            t::TABLE_CHANGE => self.table_change(c, ctx),
            t::TABLE_TRANSFER => self.table_transfer(c, ctx),
            t::CHECK_MERGE => self.check_merge(c, ctx),
            t::ITEMS_MOVE => self.items_move(c, ctx),
            _ => Ok(self.reject(c, reasons::UNKNOWN_TYPE)),
        }
    }

    // ---- results

    fn applied(&self, c: &SyncCommand, result: Option<Json>) -> SyncCommandResult {
        SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Applied,
            reason: None,
            result,
            at_utc: utc_now(&self.clock),
            exec_ms: None,
        }
    }

    fn reject(&self, c: &SyncCommand, reason: &str) -> SyncCommandResult {
        SyncCommandResult {
            command_id: c.command_id,
            status: CommandStatus::Rejected,
            reason: Some(reason.to_owned()),
            result: None,
            at_utc: utc_now(&self.clock),
            exec_ms: None,
        }
    }

    fn invalid(&self, c: &SyncCommand, problem: &str) -> SyncCommandResult {
        self.reject(c, &format!("{}: {problem}", reasons::INVALID_PAYLOAD))
    }

    /// The payload of `c`, or the `invalid_payload` rejection.
    fn payload<T: WireJson>(
        &self,
        c: &SyncCommand,
        what: &str,
        problem: impl Fn(&T) -> Option<&'static str>,
    ) -> std::result::Result<T, SyncCommandResult> {
        match T::from_element(&c.payload) {
            None => Err(self.invalid(c, &format!("payload is not {what} object"))),
            Some(p) => match problem(&p) {
                Some(why) => Err(self.invalid(c, why)),
                None => Ok(p),
            },
        }
    }

    /// Runs `work` exactly once per command id. When the command carries an expected
    /// check state, the check is fingerprinted first inside the same transaction and a
    /// mismatch is a logged `check_changed` rejection.
    fn run_guarded(
        &self,
        c: &SyncCommand,
        work: CommandWork<'_>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let Some(expected) = c.expected_hash.as_deref() else {
            return self.store.run_once(c.command_id, work, ctx);
        };
        let Some((_, check_no)) = check_aggregate::try_parse(Some(&c.aggregate_key)) else {
            return Ok(self.reject(c, reasons::NOT_A_CHECK));
        };
        let mut guarded = |unit: &mut dyn SiteCommandUnit, t: &Ctx| -> Result<SyncCommandResult> {
            let Some(actual) = unit.check_fingerprint(check_no, t)? else {
                return Ok(self.reject(c, reasons::CHECK_NOT_FOUND));
            };
            if !CheckStateFingerprint::matches(Some(expected), Some(&actual)) {
                self.log.warn(
                    "Site command {Id} ({Type}) refused: check {CheckNo} changed on the site since the user saw it",
                    &[
                        Value::from(c.command_id),
                        Value::from(c.type_.as_str()),
                        Value::from(check_no),
                    ],
                );
                return Ok(self.reject(c, reasons::CHECK_CHANGED));
            }
            work(unit, t)
        };
        self.store.run_once(c.command_id, &mut guarded, ctx)
    }

    // ---- KDS and reservations

    fn kds_line_state(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "a kds.line_state", KdsLineStatePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if !unit.open_check_belongs_to_rvc(p.rvc_no, p.check_no, t)? {
                    return Ok(self.reject(c, reasons::CHECK_NOT_FOUND));
                }
                let needs_accept_columns =
                    p.action == kds_line_actions::ACCEPT || p.action == kds_line_actions::UNACCEPT;
                if needs_accept_columns && !unit.kds_accept_columns_exist(t)? {
                    return Ok(self.reject(c, reasons::KDS_ACCEPT_UNSUPPORTED));
                }
                let mut seen = HashSet::new();
                let seqs: Vec<i64> = p
                    .line_seqs
                    .iter()
                    .copied()
                    .filter(|s| seen.insert(*s))
                    .collect();
                let lines =
                    unit.apply_kds_line_state(p.check_no, &seqs, &p.action, &p.cashier, t)?;
                // Undoing something that is not there is refused, as the legacy
                // SRsp_KdsUnAccept / SRsp_KdsUnDone did; accept and done are idempotent.
                if lines == 0 && p.action == kds_line_actions::UNACCEPT {
                    return Ok(self.reject(c, reasons::NOT_ACCEPTED));
                }
                if lines == 0 && p.action == kds_line_actions::UNDONE {
                    return Ok(self.reject(c, reasons::NOT_DONE));
                }
                Ok(self.applied(c, Some(KdsLineStateResult { lines }.to_element())))
            },
            ctx,
        )
    }

    fn reservation_save(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "a reservation.save", ReservationSavePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                let id = unit.insert_reservation(&p, t)?;
                Ok(self.applied(c, Some(ReservationSavedResult { id }.to_element())))
            },
            ctx,
        )
    }

    fn reservation_cancel(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "a reservation.cancel", ReservationCancelPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                Ok(if unit.delete_reservation(p.rvc_no, p.id, t)? {
                    self.applied(c, None)
                } else {
                    self.reject(c, reasons::RESERVATION_NOT_FOUND)
                })
            },
            ctx,
        )
    }

    // ---- order verbs (OW1b, ADR 0104)

    /// Read-your-writes (ADR 0104 §4): the result is the check read inside the command's
    /// transaction.
    #[allow(clippy::too_many_arguments)]
    fn applied_check(
        &self,
        c: &SyncCommand,
        unit: &mut dyn SiteCommandUnit,
        rvc_no: i32,
        check_no: i64,
        ctx: &Ctx,
        trn_seq: Option<i64>,
        sent_seqs: Option<Vec<i64>>,
    ) -> Result<SyncCommandResult> {
        let Some(mut check) = unit.orders().read_check(rvc_no, check_no, ctx)? else {
            return Err(CommandError::invalid(format!(
                "Check {check_no} of RVC {rvc_no} vanished inside its own order command."
            )));
        };
        check.trn_seq = trn_seq;
        check.sent_seqs = sent_seqs;
        Ok(self.applied(c, Some(check.to_element())))
    }

    fn check_open(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.check_open", OrderCheckOpenPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                let check_no = unit.orders().open_check(&p, t)?;
                self.applied_check(c, unit, p.rvc_no, check_no, t, None, None)
            },
            ctx,
        )
    }

    fn item_post(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.item_post", OrderItemPostPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                // ADR 0104 §3: a check missing from tmpInvoice is refused; never recreated.
                if unit
                    .orders()
                    .check_status(p.rvc_no, p.check_no, t)?
                    .is_none()
                {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                }
                if !unit.orders().item_exists(&p.item_code, t)? {
                    return Ok(self.reject(c, order::ITEM_NOT_FOUND));
                }
                let posted = unit.orders().post_item(&p, t)?;
                if let Some(reason) = &posted.reason {
                    return Ok(self.reject(c, reason));
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, Some(posted.trn_seq), None)
            },
            ctx,
        )
    }

    fn send(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.send", OrderSendPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if unit
                    .orders()
                    .check_status(p.rvc_no, p.check_no, t)?
                    .is_none()
                {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                }
                let seqs = unit.orders().pending_send_seqs(p.rvc_no, p.check_no, t)?;
                unit.orders().send(&p, t)?;
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, None, Some(seqs))
            },
            ctx,
        )
    }

    fn item_void(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.item_void", OrderItemVoidPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                // The legacy batches RAISERROR BILL_NOT_OPEN on a check that is not open;
                // refuse it before writing instead.
                match unit.orders().check_status(p.rvc_no, p.check_no, t)? {
                    None => return Ok(self.reject(c, order::CHECK_NOT_FOUND)),
                    Some(1) => {}
                    Some(_) => return Ok(self.reject(c, order::BILL_NOT_OPEN)),
                }
                let Some(line) = unit
                    .orders()
                    .find_line(p.rvc_no, p.check_no, p.trn_seq, t)?
                else {
                    return Ok(self.reject(c, order::LINE_NOT_FOUND));
                };
                if !line.is_item() {
                    return Ok(self.reject(c, order::NOT_AN_ITEM_LINE));
                }
                // ADR 0104 §5: the unsent removal never deletes a line the kitchen has.
                if !p.sent && line.sent {
                    return Ok(self.reject(c, order::ITEM_ALREADY_SENT));
                }
                if p.sent {
                    unit.orders().void_sent(&p, t)?;
                } else {
                    unit.orders().void_unsent(&p, t)?;
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, None, None)
            },
            ctx,
        )
    }

    /// OW1c (ADR 0107): sets the covers of an open check with the legacy batch.
    fn covers(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.covers", OrderCoversPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                match unit.orders().check_status(p.rvc_no, p.check_no, t)? {
                    None => return Ok(self.reject(c, order::CHECK_NOT_FOUND)),
                    Some(1) => {}
                    Some(_) => return Ok(self.reject(c, order::BILL_NOT_OPEN)),
                }
                // The batch repeats both checks before it writes; a refusal here means
                // POS WIN closed the check in between.
                if !unit.orders().set_covers(&p, t)? {
                    let now = unit.orders().check_status(p.rvc_no, p.check_no, t)?;
                    return Ok(self.reject(
                        c,
                        if now.is_none() {
                            order::CHECK_NOT_FOUND
                        } else {
                            order::BILL_NOT_OPEN
                        },
                    ));
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, None, None)
            },
            ctx,
        )
    }

    /// OW1d (ADR 0110): the combo verb; the batch itself decides item_not_found.
    fn combo_post(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.combo.post", OrderComboPostPayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if unit
                    .orders()
                    .check_status(p.rvc_no, p.check_no, t)?
                    .is_none()
                {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                }
                let posted = unit.orders().post_combo(&p, t)?;
                if let Some(reason) = &posted.reason {
                    return Ok(self.reject(c, reason));
                }
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, Some(posted.trn_seq), None)
            },
            ctx,
        )
    }

    // ---- table and multi-check verbs (OW1e, ADR 0112)

    fn schema_refusal(
        &self,
        c: &SyncCommand,
        unit: &mut dyn SiteCommandUnit,
        ctx: &Ctx,
    ) -> Result<Option<SyncCommandResult>> {
        let missing = self.schema.missing(&c.type_, unit.tables(), ctx)?;
        Ok((!missing.is_empty()).then(|| self.reject(c, table::SCHEMA_UNSUPPORTED)))
    }

    fn open_refusal(
        &self,
        c: &SyncCommand,
        unit: &mut dyn SiteCommandUnit,
        rvc_no: i32,
        check_no: i64,
        ctx: &Ctx,
    ) -> Result<Option<SyncCommandResult>> {
        Ok(match unit.orders().check_status(rvc_no, check_no, ctx)? {
            None => Some(self.reject(c, order::CHECK_NOT_FOUND)),
            Some(1) => None,
            Some(_) => Some(self.reject(c, order::BILL_NOT_OPEN)),
        })
    }

    fn table_change(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.table_change", OrderTableChangePayload::problem) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if let Some(r) = self.schema_refusal(c, unit, t)? {
                    return Ok(r);
                }
                if let Some(r) = self.open_refusal(c, unit, p.rvc_no, p.check_no, t)? {
                    return Ok(r);
                }
                unit.tables().change_table(&p, t)?;
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, None, None)
            },
            ctx,
        )
    }

    fn table_transfer(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(
            c,
            "an order.table_transfer",
            OrderTableTransferPayload::problem,
        ) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.run_guarded(
            c,
            &mut |unit, t| {
                if let Some(r) = self.schema_refusal(c, unit, t)? {
                    return Ok(r);
                }
                if let Some(r) = self.open_refusal(c, unit, p.rvc_no, p.check_no, t)? {
                    return Ok(r);
                }
                let Some(check) = unit.orders().read_check(p.rvc_no, p.check_no, t)? else {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                };
                let current = check.table_no.as_deref().unwrap_or("").trim();
                if eq_ignore_case(current, p.to_table_no.trim()) {
                    return Ok(self.reject(c, table::SAME_TABLE));
                }
                unit.tables().transfer(&p, t)?;
                self.applied_check(c, unit, p.rvc_no, p.check_no, t, None, None)
            },
            ctx,
        )
    }

    /// A multi-check verb carries its expected states in the payload; one on the command
    /// itself is a contract error.
    fn multi_check_problem(c: &SyncCommand) -> Option<&'static str> {
        c.expected_hash.is_some().then_some(
            "a multi-check command carries its expected states in checks[], not expected_hash",
        )
    }

    fn check_merge(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.check_merge", |p: &OrderCheckMergePayload| {
            p.problem().or_else(|| Self::multi_check_problem(c))
        }) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        self.store.run_once(
            c.command_id,
            &mut |unit, t| {
                if let Some(r) = self.schema_refusal(c, unit, t)? {
                    return Ok(r);
                }
                let locked = Self::lock_checks(unit, p.rvc_no, &p.checks, t)?;
                if locked.iter().any(|l| l.status.is_none()) {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                }
                if let Some(r) = self.changed_refusal(c, &locked) {
                    return Ok(r);
                }
                if locked.iter().any(|l| l.status != Some(1)) {
                    return Ok(self.reject(c, order::BILL_NOT_OPEN));
                }
                for l in &locked {
                    if unit
                        .tables()
                        .money_facts(p.rvc_no, l.check_no, t)?
                        .has_payment
                    {
                        return Ok(self.reject(c, table::BILL_HAS_PAYMENT));
                    }
                }
                unit.tables().merge(&p, t)?;
                self.applied_checks(
                    c,
                    unit,
                    p.rvc_no,
                    &[p.target_check_no],
                    &[p.source_check_no],
                    t,
                )
            },
            ctx,
        )
    }

    fn items_move(&self, c: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        let p = match self.payload(c, "an order.items_move", |p: &OrderItemsMovePayload| {
            p.problem().or_else(|| Self::multi_check_problem(c))
        }) {
            Ok(p) => p,
            Err(r) => return Ok(r),
        };
        // The legacy service refuses a move onto the source itself before it reads.
        if p.from_check_no == p.to_check_no {
            return Ok(self.reject(c, table::INVALID_MOVE_TARGET));
        }
        self.store.run_once(
            c.command_id,
            &mut |unit, t| {
                if let Some(r) = self.schema_refusal(c, unit, t)? {
                    return Ok(r);
                }
                let locked = Self::lock_checks(unit, p.rvc_no, &p.checks, t)?;
                let status_of = |n: i64| locked.iter().find(|l| l.check_no == n).and_then(|l| l.status);
                let (from, to) = (status_of(p.from_check_no), status_of(p.to_check_no));
                if from.is_none() {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                }
                if to.is_none() {
                    return Ok(self.reject(c, table::INVALID_MOVE_TARGET));
                }
                if let Some(r) = self.changed_refusal(c, &locked) {
                    return Ok(r);
                }
                if from != Some(1) {
                    return Ok(self.reject(c, order::BILL_NOT_OPEN));
                }
                if to != Some(1) {
                    return Ok(self.reject(c, table::INVALID_MOVE_TARGET));
                }
                let from_money = unit.tables().money_facts(p.rvc_no, p.from_check_no, t)?;
                let to_money = unit.tables().money_facts(p.rvc_no, p.to_check_no, t)?;
                if from_money.has_payment || to_money.has_payment {
                    return Ok(self.reject(c, table::BILL_HAS_PAYMENT));
                }
                if from_money.has_discount {
                    return Ok(self.reject(c, table::DISCOUNT_PRESENT));
                }
                let existing: HashSet<i64> = unit
                    .tables()
                    .lock_item_lines(p.rvc_no, p.from_check_no, t)?
                    .into_iter()
                    .collect();
                if existing.is_empty() {
                    return Ok(self.reject(c, order::CHECK_NOT_FOUND));
                }
                let mut seen = HashSet::new();
                let requested: Vec<i64> =
                    p.trn_seqs.iter().copied().filter(|s| seen.insert(*s)).collect();
                if requested.iter().any(|s| !existing.contains(s)) {
                    return Ok(self.reject(c, order::ITEM_NOT_FOUND));
                }
                if let Some(audit_error) = unit.tables().move_items(&p, &requested, t)? {
                    self.log.warn(
                        "Site command {Id} moved {Count} lines {From}->{To}; the store refused the MOVE audit (SQL error {Number})",
                        &[
                            Value::from(c.command_id),
                            Value::from(requested.len()),
                            Value::from(p.from_check_no),
                            Value::from(p.to_check_no),
                            Value::from(audit_error),
                        ],
                    );
                }
                self.applied_checks(c, unit, p.rvc_no, &[p.from_check_no, p.to_check_no], &[], t)
            },
            ctx,
        )
    }

    /// Locks every check header the command touches in ascending check number (the
    /// deadlock-free order ADR 0112 §3 fixes) and fingerprints each one under its lock.
    fn lock_checks(
        unit: &mut dyn SiteCommandUnit,
        rvc_no: i32,
        checks: &[OrderCheckExpectation],
        ctx: &Ctx,
    ) -> Result<Vec<LockedCheck>> {
        let mut ordered: Vec<&OrderCheckExpectation> = checks.iter().collect();
        ordered.sort_by_key(|e| e.check_no);
        let mut locked = Vec::with_capacity(ordered.len());
        for e in ordered {
            let status = unit.tables().lock_check(rvc_no, e.check_no, ctx)?;
            let actual = match status {
                None => None,
                Some(_) => unit.check_fingerprint(e.check_no, ctx)?,
            };
            locked.push(LockedCheck {
                check_no: e.check_no,
                status,
                matches: CheckStateFingerprint::matches(Some(&e.expected), actual.as_deref()),
            });
        }
        Ok(locked)
    }

    fn changed_refusal(
        &self,
        c: &SyncCommand,
        locked: &[LockedCheck],
    ) -> Option<SyncCommandResult> {
        let changed: Vec<String> = locked
            .iter()
            .filter(|l| !l.matches)
            .map(|l| l.check_no.to_string())
            .collect();
        if changed.is_empty() {
            return None;
        }
        self.log.warn(
            "Site command {Id} ({Type}) refused: checks {Checks} changed on the site since the user saw them",
            &[
                Value::from(c.command_id),
                Value::from(c.type_.as_str()),
                Value::from(changed.join(",")),
            ],
        );
        Some(self.reject(c, reasons::CHECK_CHANGED))
    }

    /// Read-your-writes (ADR 0112 §4): every still-open touched check, read in the
    /// command's transaction.
    fn applied_checks(
        &self,
        c: &SyncCommand,
        unit: &mut dyn SiteCommandUnit,
        rvc_no: i32,
        open: &[i64],
        removed: &[i64],
        ctx: &Ctx,
    ) -> Result<SyncCommandResult> {
        let mut checks: Vec<OrderCheckResult> = Vec::with_capacity(open.len());
        for check_no in open {
            match unit.orders().read_check(rvc_no, *check_no, ctx)? {
                Some(check) => checks.push(check),
                None => {
                    return Err(CommandError::invalid(format!(
                        "Check {check_no} of RVC {rvc_no} vanished inside its own command."
                    )));
                }
            }
        }
        let result = OrderMultiCheckResult {
            checks,
            removed: removed.to_vec(),
        };
        Ok(self.applied(c, Some(result.to_element())))
    }
}

/// `string.Equals(a, b, StringComparison.OrdinalIgnoreCase)`.
fn eq_ignore_case(a: &str, b: &str) -> bool {
    crate::util::stj::eq_ignore_case(a, b)
}

impl CommandExecutor for SiteCommandExecutor {
    fn execute(&self, command: &SyncCommand, ctx: &Ctx) -> Result<SyncCommandResult> {
        SiteCommandExecutor::execute(self, command, ctx)
    }

    fn find_recorded(&self, command_id: Uuid, ctx: &Ctx) -> Result<Option<SyncCommandResult>> {
        SiteCommandExecutor::find_recorded(self, command_id, ctx)
    }
}
