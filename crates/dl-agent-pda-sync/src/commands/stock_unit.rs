//! The SMILE_BO stock seam of the five stock verbs (`ITxnStockUnit.cs`, ADR 0164, 0167, 0168,
//! 0169): the store map, the ledger probes and the vouchers the agent writes into SMILE_BO,
//! all inside the command transaction. A business refusal of the store is
//! [`StockError::Refused`] (the C# `TxnRefusedException`).

use dl_decimal::Decimal;
use dl_time::Date;
use dl_uuid::Uuid;

use super::error::CommandError;
use crate::util::ctx::Ctx;

/// Why a stock unit call did not return: a refusal with its reason (nothing was written),
/// or a failure that rolls the command back and is retried.
#[derive(Debug)]
pub enum StockError {
    /// `TxnRefusedException`: one of the `txn_stock_*` reasons.
    Refused(&'static str),
    /// A SQL or store failure.
    Command(CommandError),
}

impl From<CommandError> for StockError {
    fn from(e: CommandError) -> Self {
        StockError::Command(e)
    }
}

/// Result of the stock unit calls that may refuse.
pub type StockResult<T> = std::result::Result<T, StockError>;

/// One row of `dbo.Pda_TxnStockMap` (script 170): the BO database and store of a cloud store.
#[derive(Debug, Clone, PartialEq)]
pub struct StockStoreMap {
    /// The cloud store code.
    pub store_code: i32,
    /// The configured SMILE_BO database (`None`: the default `SMILE_BO`).
    pub database: Option<String>,
    /// The BO store (`RVC`) the ledger lines are written to.
    pub store_rvc: i32,
    /// The counter location (`WithRVC`).
    pub with_rvc: i32,
    /// The department of a count.
    pub dept_code: Option<String>,
    /// The stock voucher type (`JType`).
    pub j_type: Option<String>,
}

/// How many posted `INVTrn` lines of one `PApp` the site holds per code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockCodeCount {
    /// `ITrnCode`.
    pub trn_code: i32,
    /// `ITrnSub`.
    pub trn_sub: i32,
    /// Lines.
    pub lines: i64,
}

/// What a SMILE_BO database holds before its first depletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockLedgerProfile {
    /// The ledger objects exist.
    pub ready: bool,
    /// `IVoucherType` rows.
    pub voucher_types: i32,
    /// The SX code profile of `INVTrn`.
    pub sx_codes: Vec<StockCodeCount>,
}

/// `GlobPara` of SMILE_BO: cost method and the open periods (yyyyMM, `None` when unset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockSettings {
    /// `INVCostMethod`.
    pub cost_method: Option<i32>,
    /// `INVPeriod`.
    pub inv_period: Option<i32>,
    /// `GLPeriod`.
    pub gl_period: Option<i32>,
}

/// One line the bill holds out of SMILE_BO stock (`Pda_TxnStockHeld`); `day` is the business
/// date it was last moved on.
#[derive(Debug, Clone, PartialEq)]
pub struct StockHeldLine {
    /// `ICode`.
    pub item_code: String,
    /// The cloud store code.
    pub store_code: i32,
    /// Held quantity.
    pub qty: Decimal,
    /// Business date.
    pub day: Date,
}

/// One `tmpINVTrn` line, signed as the ledger keeps it (negative takes stock out).
#[derive(Debug, Clone, PartialEq)]
pub struct StockVoucherLine {
    /// `ICode`.
    pub item_code: String,
    /// Signed quantity.
    pub trn_qty: Decimal,
}

/// One SX voucher into SMILE_BO.
#[derive(Debug, Clone, PartialEq)]
pub struct StockVoucher {
    /// The BO database.
    pub database: String,
    /// Voucher date.
    pub day: Date,
    /// BO store.
    pub store_rvc: i32,
    /// Counter location.
    pub with_rvc: i32,
    /// Department.
    pub dept_code: Option<String>,
    /// Voucher type.
    pub j_type: Option<String>,
    /// `VRef`.
    pub reference: String,
    /// The user.
    pub user: String,
    /// Lines.
    pub lines: Vec<StockVoucherLine>,
    /// `ITrnSub`: 70 depletion, 50 issue.
    pub trn_sub: i32,
    /// `VDesc` when not the reference.
    pub description: Option<String>,
}

impl StockVoucher {
    /// `ITrnSub` of a depletion.
    pub const DEPLETION_SUB: i32 = 70;
}

/// One 200 / 50 line of a posted issue voucher.
#[derive(Debug, Clone, PartialEq)]
pub struct StockIssueLine {
    /// `ICode`.
    pub item_code: String,
    /// Signed quantity.
    pub trn_qty: Decimal,
    /// `RVC`.
    pub rvc: i32,
    /// `WithRVC`.
    pub with_rvc: i32,
}

/// A posted issue voucher read back.
#[derive(Debug, Clone, PartialEq)]
pub struct StockIssuePosted {
    /// `INVVoucher.VSeq`.
    pub vseq: i64,
    /// `VNo`.
    pub voucher_no: String,
    /// `VRef`.
    pub reference: Option<String>,
    /// Lines.
    pub lines: Vec<StockIssueLine>,
}

/// One counted item.
#[derive(Debug, Clone, PartialEq)]
pub struct StockCountLine {
    /// `ICode`.
    pub item_code: String,
    /// `ICount`.
    pub counted_qty: Decimal,
}

/// One physical count session into SMILE_BO.
#[derive(Debug, Clone, PartialEq)]
pub struct StockCountSession {
    /// The BO database.
    pub database: String,
    /// Count date.
    pub day: Date,
    /// `yyyyMM`.
    pub period: i32,
    /// BO store.
    pub store_rvc: i32,
    /// Counter location.
    pub with_rvc: i32,
    /// Department.
    pub dept_code: String,
    /// Voucher type.
    pub j_type: Option<String>,
    /// `InvDesc`.
    pub reference: String,
    /// The user.
    pub user: String,
    /// Lines.
    pub lines: Vec<StockCountLine>,
}

/// One counted line as the session holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct StockCountDifference {
    /// `ICode`.
    pub item_code: String,
    /// `Inventory.VEnd` of the period (no row = 0).
    pub system_qty: Decimal,
    /// `ICount`.
    pub counted_qty: Decimal,
}

impl StockCountDifference {
    /// The ledger difference: counted minus system.
    pub fn trn_qty(&self) -> Decimal {
        self.counted_qty - self.system_qty
    }
}

/// A count session as posted (or read back).
#[derive(Debug, Clone, PartialEq)]
pub struct StockCountPosted {
    /// `InvSession` key.
    pub session_seq: i64,
    /// The adjustment voucher number, when one was posted.
    pub voucher_no: Option<String>,
    /// Lines.
    pub lines: Vec<StockCountDifference>,
    /// BO store (read back only).
    pub store_rvc: Option<i32>,
    /// `InvDesc` (read back only).
    pub reference: Option<String>,
}

/// The receiving ledger of a SMILE_BO database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockReceiptLedger {
    /// The objects exist and `RCVUpdateGL` has the signature the agent binds.
    pub ready: bool,
    /// The `RCV` voucher type.
    pub voucher_type: Option<String>,
    /// The SR code profile of `INVTrn`.
    pub sr_codes: Vec<StockCodeCount>,
}

/// One received line.
#[derive(Debug, Clone, PartialEq)]
pub struct StockReceiptLine {
    /// `ICode`.
    pub item_code: String,
    /// Quantity.
    pub qty: Decimal,
    /// Price per stock unit.
    pub unit_price: Decimal,
}

/// One goods receipt into SMILE_BO.
#[derive(Debug, Clone, PartialEq)]
pub struct StockReceipt {
    /// The BO database.
    pub database: String,
    /// Receipt date.
    pub day: Date,
    /// `yyyyMM`.
    pub period: i32,
    /// BO store.
    pub store_rvc: i32,
    /// AP vendor.
    pub vendor_code: i64,
    /// `VRef` / `VDesc`.
    pub reference: String,
    /// The user.
    pub user: String,
    /// Lines.
    pub lines: Vec<StockReceiptLine>,
}

/// One line as the posted receipt holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct StockReceiptPostedLine {
    /// `ICode`.
    pub item_code: String,
    /// `TrnQty`.
    pub qty: Decimal,
    /// `subAmount`.
    pub amount: Decimal,
}

/// A posted receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct StockReceiptPosted {
    /// `ReceivingHeader` key.
    pub receipt_seq: i64,
    /// `VNo`.
    pub voucher_no: String,
    /// Lines.
    pub lines: Vec<StockReceiptPostedLine>,
    /// The single store of the lines (`None` when they differ).
    pub store_rvc: Option<i32>,
    /// `VRef`.
    pub reference: Option<String>,
    /// The vendor.
    pub vendor_code: Option<i64>,
}

/// What `dbo.Pda_TxnApplied` holds for an aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockApplied {
    /// The applied version.
    pub version: i64,
    /// The BO key of the voucher, session or receipt (script 145; `None` for a depletion
    /// recorded before the column or by `txn.snapshot`).
    pub store_key: Option<i64>,
}

/// The reads and writes of the stock verbs inside the transaction of
/// [`super::SiteCommandStore::run_once`]. The database name is the only interpolated text
/// (validated by [`resolve_database`](Self::resolve_database)); every value is a parameter.
pub trait TxnStockUnit {
    /// The applied state of `aggregate`, read with `UPDLOCK, HOLDLOCK`; `None` when never written.
    fn lock_applied(&mut self, aggregate: &str, ctx: &Ctx) -> super::Result<Option<StockApplied>>;

    /// Records `version`, the hash, the command id and the BO `store_key` for `aggregate`.
    fn save_applied(
        &mut self,
        aggregate: &str,
        version: i64,
        hash: &str,
        command_id: Uuid,
        store_key: Option<i64>,
        ctx: &Ctx,
    ) -> super::Result<()>;

    /// The map row of a cloud store code; `None` when the site has none.
    fn map(&mut self, store_code: i32, ctx: &Ctx) -> super::Result<Option<StockStoreMap>>;

    /// The validated SMILE_BO database name; `None` when not usable.
    fn resolve_database(
        &mut self,
        configured: Option<&str>,
        ctx: &Ctx,
    ) -> super::Result<Option<String>>;

    /// True when every one of `rvcs` is an RVC of SMILE_BO `DefRVCList`.
    fn stores_exist(&mut self, database: &str, rvcs: &[i32], ctx: &Ctx) -> super::Result<bool>;

    /// The ledger objects, voucher types and SX profile of a database.
    fn ledger_profile(&mut self, database: &str, ctx: &Ctx) -> super::Result<StockLedgerProfile>;

    /// The cost method and the open periods.
    fn settings(&mut self, database: &str, ctx: &Ctx) -> super::Result<StockSettings>;

    /// The codes of `item_codes` that `ItemDef` does not have.
    fn unknown_items(
        &mut self,
        database: &str,
        item_codes: &[String],
        ctx: &Ctx,
    ) -> super::Result<Vec<String>>;

    /// The lines the aggregate holds out of stock.
    fn held(&mut self, aggregate: &str, ctx: &Ctx) -> super::Result<Vec<StockHeldLine>>;

    /// Replaces the lines the aggregate holds.
    fn save_held(
        &mut self,
        aggregate: &str,
        lines: &[StockHeldLine],
        ctx: &Ctx,
    ) -> super::Result<()>;

    /// Writes and posts one voucher; its `VSeq`.
    fn post_voucher(&mut self, voucher: &StockVoucher, ctx: &Ctx) -> StockResult<i64>;

    /// True when the count objects exist in `database`.
    fn count_ledger_ready(&mut self, database: &str, ctx: &Ctx) -> super::Result<bool>;

    /// True when the store, counter location and department of a count are valid.
    fn count_target_valid(
        &mut self,
        database: &str,
        store_rvc: i32,
        post_loc: i32,
        dept: &str,
        ctx: &Ctx,
    ) -> super::Result<bool>;

    /// Writes the count session and posts its adjustment voucher.
    fn post_count(
        &mut self,
        session: &StockCountSession,
        ctx: &Ctx,
    ) -> StockResult<StockCountPosted>;

    /// The count session as written; `None` when it is gone.
    fn read_count(
        &mut self,
        database: &str,
        session_seq: i64,
        ctx: &Ctx,
    ) -> super::Result<Option<StockCountPosted>>;

    /// The receiving ledger of a database.
    fn receipt_ledger(&mut self, database: &str, ctx: &Ctx) -> super::Result<StockReceiptLedger>;

    /// True when the vendor, store and items of a receipt pass the guards of `RCVUpdateGL`.
    fn receipt_target_valid(
        &mut self,
        database: &str,
        store_rvc: i32,
        vendor_code: i64,
        item_codes: &[String],
        ctx: &Ctx,
    ) -> super::Result<bool>;

    /// Writes and posts a receipt; read back.
    fn post_receipt(
        &mut self,
        receipt: &StockReceipt,
        ctx: &Ctx,
    ) -> StockResult<StockReceiptPosted>;

    /// The receipt as posted; `None` when it is gone.
    fn read_receipt(
        &mut self,
        database: &str,
        receipt_seq: i64,
        ctx: &Ctx,
    ) -> super::Result<Option<StockReceiptPosted>>;

    /// The posted issue voucher `vseq` with its 200 / 50 lines; `None` when there is none.
    fn read_issue(
        &mut self,
        database: &str,
        vseq: i64,
        ctx: &Ctx,
    ) -> super::Result<Option<StockIssuePosted>>;

    /// `DefRVCList.RVCType` of `rvc` (0 stock store, 1 consumption, 2 asset).
    fn store_type(&mut self, database: &str, rvc: i32, ctx: &Ctx) -> super::Result<Option<i32>>;
}
