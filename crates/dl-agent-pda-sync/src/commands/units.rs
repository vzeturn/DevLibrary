//! The store seams of the site command executor (`ISiteCommandStore.cs`,
//! `IOrderCommandUnit.cs`, `ITableCommandUnit.cs`): every command runs in one SQL
//! transaction together with its `dbo.Pda_SyncCommandLog` row (sync spec §5
//! exactly-once), so either the business write and the log row both commit, or neither
//! does.

use dl_uuid::Uuid;

use super::error::Result;
use super::master::{MasterEntity, MasterKey};
use super::stock_unit::TxnStockUnit;
use super::txn::TxnCommandUnit;
use super::txn_counters::TxnCounterUnit;
use super::txn_room_charge::TxnRoomChargeUnit;
use crate::contracts::mapping::VendorRow;
use crate::contracts::wire::StaffClerk;
use crate::contracts::wire::{
    OrderCheckCancelEmptyPayload, OrderCheckClosePayload, OrderCheckCloseResult, OrderCheckLine,
    OrderCheckMergePayload, OrderCheckOpenPayload, OrderCheckResult, OrderComboPostPayload,
    OrderCoversPayload, OrderItemPostPayload, OrderItemVoidPayload, OrderItemsMovePayload,
    OrderPoNumberPayload, OrderRoomChargePayload, OrderSendPayload, OrderTableChangePayload,
    OrderTableTransferPayload, OrderTenderAddPayload, OrderTenderRemovePayload,
    ReservationSavePayload, SyncCommandResult,
};
use crate::util::ctx::Ctx;

/// The work of one command inside [`SiteCommandStore::run_once`].
pub type CommandWork<'a> =
    &'a mut dyn FnMut(&mut dyn SiteCommandUnit, &Ctx) -> Result<SyncCommandResult>;

/// Store database seam of the site command executor (sync S9, ADR 0029).
pub trait SiteCommandStore: Send + Sync {
    /// The outcome logged for `command_id`; `None` when the command never committed.
    fn find_recorded(&self, command_id: Uuid, ctx: &Ctx) -> Result<Option<SyncCommandResult>>;

    /// Locks the log key of `command_id`; when it is already logged returns that result
    /// without running `work`, otherwise runs it and commits its writes with the log row
    /// of the result it returns.
    fn run_once(
        &self,
        command_id: Uuid,
        work: CommandWork<'_>,
        ctx: &Ctx,
    ) -> Result<SyncCommandResult>;
}

/// The store writes a command may make, all inside the transaction of
/// [`SiteCommandStore::run_once`].
pub trait SiteCommandUnit {
    /// True when open check `check_no` (`dbo.TmpInvoice`) belongs to RVC `rvc_no`.
    fn open_check_belongs_to_rvc(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<bool>;

    /// `CheckStateFingerprint` of open check `check_no` read inside the command
    /// transaction, with the header row update-locked until commit (ADR 0100 §4); `None`
    /// when the check is not open.
    fn check_fingerprint(&mut self, check_no: i64, ctx: &Ctx) -> Result<Option<String>>;

    /// True when the additive KDS accept columns exist on `dbo.tmpTrnInfo`.
    fn kds_accept_columns_exist(&mut self, ctx: &Ctx) -> Result<bool>;

    /// Applies a `kds_line_actions` value to the lines; returns how many lines changed.
    fn apply_kds_line_state(
        &mut self,
        check_no: i64,
        line_seqs: &[i64],
        action: &str,
        cashier: &str,
        ctx: &Ctx,
    ) -> Result<i32>;

    /// Inserts the reservation and returns its site identity (`nID`).
    fn insert_reservation(
        &mut self,
        reservation: &ReservationSavePayload,
        ctx: &Ctx,
    ) -> Result<i32>;

    /// Deletes reservation `id` of RVC `rvc_no`; false when there is none.
    fn delete_reservation(&mut self, rvc_no: i32, id: i32, ctx: &Ctx) -> Result<bool>;

    /// The row of `entity` for `key` the way capture reads it (live column list minus
    /// `exclude`, the `bytes_columns` as raw bytes), update-locked until commit (for a
    /// create, under the per-table create lock with the range held); `None` when absent
    /// (`master.upsert`).
    fn read_master_row(
        &mut self,
        entity: &MasterEntity,
        key: &MasterKey,
        for_create: bool,
        exclude: &[String],
        bytes_columns: &[String],
        ctx: &Ctx,
    ) -> Result<Option<VendorRow>>;

    /// Runs the entity's update with the `(column, value)` pairs; whether a row changed.
    fn update_master_row(
        &mut self,
        entity: &MasterEntity,
        key: &MasterKey,
        values: &[(&str, &str)],
        ctx: &Ctx,
    ) -> Result<bool>;

    /// Runs the entity's insert (the caller checked the key is absent under the create
    /// lock); false when it inserted nothing (no template row, ADR 0152).
    fn insert_master_row(
        &mut self,
        entity: &MasterEntity,
        key: &MasterKey,
        values: &[(&str, &str)],
        ctx: &Ctx,
    ) -> Result<bool>;

    /// The order verbs' writes and reads (OW1b, ADR 0104) in the same transaction.
    fn orders(&mut self) -> &mut dyn OrderCommandUnit;

    /// The table and multi-check verbs' writes and reads (OW1e, ADR 0112) in the same
    /// transaction.
    fn tables(&mut self) -> &mut dyn TableCommandUnit;

    /// The `txn.*` verbs' reads and writes (ADR 0159) in the same transaction.
    fn txn(&mut self) -> &mut dyn TxnCommandUnit;

    /// The cashier reads and writes of `master.upsert` entity `cashier` (ADR 0150) in the
    /// same transaction.
    fn cashiers(&mut self) -> &mut dyn CashierCommandUnit;

    /// The `txn.counters` reads and writes (ADR 0161) in the same transaction.
    fn counters(&mut self) -> &mut dyn TxnCounterUnit;

    /// The Front Office reads and writes of the room-charge verbs (ADR 0162) in the same
    /// transaction.
    fn room_charge(&mut self) -> &mut dyn TxnRoomChargeUnit;

    /// The SMILE_BO reads and writes of the stock verbs (ADR 0164, 0167, 0168, 0169) in the
    /// same transaction.
    fn stock(&mut self) -> &mut dyn TxnStockUnit;
}

/// What one cashier write changes at the store (`CashierApply`, ADR 0150): the
/// `dbo.CashierDef` row (created when `create`), the full `dbo.CashierRVC` set and the
/// mapped `dbo.ClerkPermission` rows (`allowed` on, every other mapped id off). `pin` is
/// `None` when the PIN stays; it is never logged and `Debug` hides it.
#[derive(Clone, Copy)]
pub struct CashierApply<'a> {
    /// The store's spelling of the clerk key.
    pub key: &'a str,
    /// Insert instead of update.
    pub create: bool,
    /// Display name.
    pub name: &'a str,
    /// Active (`IsDisable = 0`).
    pub active: bool,
    /// Security level.
    pub security: i32,
    /// The new PIN (`TouchID`), `None` to keep the stored one.
    pub pin: Option<&'a str>,
    /// The full RVC set.
    pub rvcs: &'a [i32],
    /// The mapped function ids to allow.
    pub allowed: &'a [String],
}

impl std::fmt::Debug for CashierApply<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CashierApply {{ Key = {}, Create = {} }}",
            self.key, self.create
        )
    }
}

/// The cashier reads and writes of `master.upsert` entity `cashier`
/// (`ICashierCommandUnit`), inside the command transaction.
pub trait CashierCommandUnit {
    /// The clerk exactly as the staff sync projects it (`TouchID` hashed with `pin_key`,
    /// never returned), its `CashierDef`, `CashierRVC` and `ClerkPermission` rows
    /// update-locked until commit; `None` when there is no such clerk.
    fn read(&mut self, key: &str, pin_key: &[u8], ctx: &Ctx) -> Result<Option<StaffClerk>>;

    /// True when an active clerk other than `key` signs in with `pin` (RTRIM of `TouchID`).
    fn pin_taken(&mut self, key: &str, pin: &str, ctx: &Ctx) -> Result<bool>;

    /// Writes `apply`; a refusal reason when this store's `CashierDef` cannot hold it.
    fn apply(&mut self, apply: &CashierApply<'_>, ctx: &Ctx) -> Result<Option<String>>;
}

/// The SMILE_POS writes and reads of the order verbs (OW1b, ADR 0104). Every write is a
/// legacy PDA batch ([`super::batches`]) run as `pda_sync`.
pub trait OrderCommandUnit {
    /// `tmpInvoice.Status` of the check in the RVC; `None` when it is not in
    /// `dbo.tmpInvoice`.
    fn check_status(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Option<i32>>;

    /// True when `item_code` is in `dbo.ItemDef`.
    fn item_exists(&mut self, item_code: &str, ctx: &Ctx) -> Result<bool>;

    /// Opens a check (legacy `SqlOpenCheckNo`) and returns its number.
    fn open_check(&mut self, payload: &OrderCheckOpenPayload, ctx: &Ctx) -> Result<i64>;

    /// The line's `TrnCode` and whether the kitchen has it; `None` when it is not on the
    /// check.
    fn find_line(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        trn_seq: i64,
        ctx: &Ctx,
    ) -> Result<Option<OrderLineFacts>>;

    /// Posts one item (legacy `SqlPostItem`).
    fn post_item(&mut self, payload: &OrderItemPostPayload, ctx: &Ctx) -> Result<OrderPostOutcome>;

    /// The unsent item lines a send fires (legacy `SqlPendingSendSeqs`).
    fn pending_send_seqs(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Vec<i64>>;

    /// Marks the unsent lines as sent to the kitchen (legacy `SqlSendOrder`).
    fn send(&mut self, payload: &OrderSendPayload, ctx: &Ctx) -> Result<()>;

    /// Removes an unsent line with its group (legacy `SqlVoidItem`).
    fn void_unsent(&mut self, payload: &OrderItemVoidPayload, ctx: &Ctx) -> Result<()>;

    /// Voids a sent line with a reason (legacy `SqlVoid`).
    fn void_sent(&mut self, payload: &OrderItemVoidPayload, ctx: &Ctx) -> Result<()>;

    /// Sets the covers of an open check (legacy `SqlSetCovers`, OW1c). False when the
    /// batch refused without writing (check missing or no longer open).
    fn set_covers(&mut self, payload: &OrderCoversPayload, ctx: &Ctx) -> Result<bool>;

    /// Posts a combo (legacy `SqlPostItemCombo` with the ADR 0110 fixes, OW1d).
    fn post_combo(
        &mut self,
        payload: &OrderComboPostPayload,
        ctx: &Ctx,
    ) -> Result<OrderPostOutcome>;

    /// The check right now, inside the transaction (read-your-writes); `None` when it is
    /// not open.
    fn read_check(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        ctx: &Ctx,
    ) -> Result<Option<OrderCheckResult>>;

    /// Adds one tender (`SqlOrderCommandUnit.AddTenderAsync`, ADR 0174): the tender line,
    /// or the `money_command_reasons` / `order_command_reasons` refusal.
    fn add_tender(
        &mut self,
        payload: &OrderTenderAddPayload,
        ctx: &Ctx,
    ) -> Result<OrderPostOutcome>;

    /// Charges the check to the folio of an in-house room (`AddRoomChargeAsync`, ADR 0179):
    /// the tender line and the header only; the folio is posted at the close.
    fn add_room_charge(
        &mut self,
        payload: &OrderRoomChargePayload,
        ctx: &Ctx,
    ) -> Result<OrderPostOutcome>;

    /// Removes one tender line; `None` when removed, otherwise the refusal.
    fn remove_tender(
        &mut self,
        payload: &OrderTenderRemovePayload,
        ctx: &Ctx,
    ) -> Result<Option<String>>;

    /// Closes a balanced check, or answers the bill an earlier close made.
    fn close_check(
        &mut self,
        payload: &OrderCheckClosePayload,
        ctx: &Ctx,
    ) -> Result<OrderCloseOutcome>;
}

/// The SMILE_POS writes and reads of the table and multi-check verbs (OW1e, ADR 0112).
pub trait TableCommandUnit {
    /// The `columns` that are not in this store's `dbo` schema, as `table.column`.
    fn missing_columns(&mut self, columns: &[StoreColumn], ctx: &Ctx) -> Result<Vec<String>>;

    /// Update-locks the check header until commit and returns its `Status`; `None` when
    /// the check is not in the RVC.
    fn lock_check(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Option<i32>>;

    /// Whether the check carries a payment or a discount line.
    fn money_facts(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<CheckMoneyFacts>;

    /// Sets the table and guest count of the check (legacy `SqlTableInfo`).
    fn change_table(&mut self, payload: &OrderTableChangePayload, ctx: &Ctx) -> Result<()>;

    /// Moves the check to another table (legacy transfer: header, then line info).
    fn transfer(&mut self, payload: &OrderTableTransferPayload, ctx: &Ctx) -> Result<()>;

    /// Merges the source into the target; fails when a recalculation fails, so nothing
    /// commits.
    fn merge(&mut self, payload: &OrderCheckMergePayload, ctx: &Ctx) -> Result<()>;

    /// What the empty-bill cleanup re-checks under the header lock (ADR 0144): table,
    /// the cutoff test and the line count; `None` when the check is not in the RVC.
    fn empty_bill_facts(
        &mut self,
        rvc_no: i32,
        check_no: i64,
        opened_before: &str,
        ctx: &Ctx,
    ) -> Result<Option<EmptyBillFacts>>;

    /// Cancels the empty check with the legacy cancel batch.
    fn cancel_empty(&mut self, payload: &OrderCheckCancelEmptyPayload, ctx: &Ctx) -> Result<()>;

    /// Sets `TmpInvoice.PONumber` of the check (RT-1413) and nothing else: the batch
    /// encodes the text and cuts it to the column; empty clears it.
    fn set_po_number(&mut self, payload: &OrderPoNumberPayload, ctx: &Ctx) -> Result<()>;

    /// The item lines (TrnCode 100) of the check, update-locked until commit.
    fn lock_item_lines(&mut self, rvc_no: i32, check_no: i64, ctx: &Ctx) -> Result<Vec<i64>>;

    /// Moves the item lines `trn_seqs`; returns the SQL error number of a MOVE audit the
    /// store refused (the move stands), `None` when the audit was written.
    fn move_items(
        &mut self,
        payload: &OrderItemsMovePayload,
        trn_seqs: &[i64],
        ctx: &Ctx,
    ) -> Result<Option<i32>>;
}

/// What the void needs to know about a line before it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderLineFacts {
    /// `TrnCode`, invariant text.
    pub transaction_code: Option<String>,
    /// Whether the kitchen has the line.
    pub sent: bool,
}

impl OrderLineFacts {
    /// Whether the line is an item line (`TrnCode` 100).
    pub fn is_item(&self) -> bool {
        self.transaction_code.as_deref().map(str::trim)
            == Some(OrderCheckLine::ITEM_TRANSACTION_CODE)
    }
}

/// The posted line, or the `order_command_reasons` value the batch refused it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderPostOutcome {
    /// The posted line (0 when refused).
    pub trn_seq: i64,
    /// The refusal.
    pub reason: Option<String>,
}

impl OrderPostOutcome {
    /// A posted line.
    pub fn posted(trn_seq: i64) -> OrderPostOutcome {
        OrderPostOutcome {
            trn_seq,
            reason: None,
        }
    }

    /// A refusal.
    pub fn refused(reason: &str) -> OrderPostOutcome {
        OrderPostOutcome {
            trn_seq: 0,
            reason: Some(reason.to_owned()),
        }
    }
}

/// The closed bill, or the reason the close batch refused it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderCloseOutcome {
    /// The closed bill.
    pub closed: Option<OrderCheckCloseResult>,
    /// The refusal.
    pub reason: Option<String>,
}

impl OrderCloseOutcome {
    /// A closed (or already closed) bill.
    pub fn done(closed: OrderCheckCloseResult) -> OrderCloseOutcome {
        OrderCloseOutcome {
            closed: Some(closed),
            reason: None,
        }
    }

    /// A refusal.
    pub fn refused(reason: &str) -> OrderCloseOutcome {
        OrderCloseOutcome {
            closed: None,
            reason: Some(reason.to_owned()),
        }
    }
}

/// A `dbo` column a verb's legacy batch reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoreColumn {
    /// Table.
    pub table: &'static str,
    /// Column.
    pub column: &'static str,
}

impl StoreColumn {
    /// `table.column`.
    pub const fn new(table: &'static str, column: &'static str) -> StoreColumn {
        StoreColumn { table, column }
    }
}

impl std::fmt::Display for StoreColumn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.table, self.column)
    }
}

/// What the multi-check guard needs to know about a check's money.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckMoneyFacts {
    /// A tender line or a paid amount.
    pub has_payment: bool,
    /// A discount line.
    pub has_discount: bool,
}

/// The facts the empty-bill cleanup guards on (ADR 0144).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyBillFacts {
    /// The check's table, trimmed.
    pub table_no: String,
    /// Whether `OpenTime` is earlier than the cutoff.
    pub opened_before_cutoff: bool,
    /// How many `TmpTrn` lines the check carries (any code).
    pub line_count: i32,
}
