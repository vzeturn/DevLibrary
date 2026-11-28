//! The Front Office seam of `txn.room_charge` / `txn.room_charge_void` (ADR 0162, RT-0908;
//! `ITxnRoomChargeUnit.cs`); the verbs run in [`crate::commands::SiteCommandExecutor`]
//! (`executor/txn_room_charge.rs`).

use dl_decimal::Decimal;

use super::error::Result;
use crate::contracts::DateTime;
use crate::util::ctx::Ctx;

/// `RoomChargeClerk`: the `ClerkID` POS WIN stamps on an interface posting (the vendor's
/// automatic-posting clerk).
pub const ROOM_CHARGE_CLERK: &str = "$AM";

/// One in-house folio of the Front Office (`FoFolio`); `room_code` is trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoFolio {
    /// `Folio.FolioNum`.
    pub folio_num: i64,
    /// `Folio.RoomCode`, trimmed.
    pub room_code: String,
}

/// One active (`FolioBalanceCode = 'A'`) folio row of a bill (`FoActiveRow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoActiveRow {
    /// `FolioTransaction.TransactionID`.
    pub transaction_id: i64,
    /// `FolioTransaction.Amount`.
    pub amount: Decimal,
}

/// One posting through `PostInterfaceTransactionToFolio` (`FoPostLine`).
#[derive(Debug, Clone, PartialEq)]
pub struct FoPostLine {
    /// The folio posted to.
    pub folio_num: i64,
    /// The folio transaction code (`TrnLinkByRVC` code / 100).
    pub transaction_code: i32,
    /// The folio transaction sub-code (`TrnLinkByRVC` code % 100).
    pub transaction_sub_code: i32,
    /// The folio reference: the cloud bill.
    pub ref_number: String,
    /// The amount posted.
    pub amount: Decimal,
    /// The POS meal period.
    pub meal_period: i32,
    /// The posting comment.
    pub comment: String,
    /// The posting clerk ([`ROOM_CHARGE_CLERK`]).
    pub clerk_id: String,
    /// The posting time.
    pub transaction_time: DateTime,
    /// The vendor `StsCode8` field: the RVC number as text.
    pub sts_code8: String,
}

/// The Front Office reads and writes of the room-charge verbs (`ITxnRoomChargeUnit`) inside
/// the transaction of [`super::SiteCommandStore::run_once`]. The database name is the only
/// interpolated text (checked, bracketed); every value is a parameter.
pub trait TxnRoomChargeUnit {
    /// The RVC's Front Office database (`Para InterfaceDB`, else `SMILE_FO`); `None` when it
    /// is not a plain name or the login has no access to it.
    fn resolve_database(&mut self, rvc: i32, ctx: &Ctx) -> Result<Option<String>>;

    /// The folio code (`TrnCode * 100 + TrnSub`) the POS tender maps to
    /// (`dbo.TrnLinkByRVC`); `None` when unmapped.
    fn link_code(
        &mut self,
        rvc: i32,
        trn_code: i32,
        trn_sub: i32,
        ctx: &Ctx,
    ) -> Result<Option<i32>>;

    /// The postable in-house folios of the room, or the named folio when `folio_num > 0`.
    fn in_house_folios(
        &mut self,
        database: &str,
        room: &str,
        folio_num: i64,
        ctx: &Ctx,
    ) -> Result<Vec<FoFolio>>;

    /// The bill's active rows on the folio, update-locked until commit, in id order.
    fn lock_active(
        &mut self,
        database: &str,
        folio_num: i64,
        ref_number: &str,
        ctx: &Ctx,
    ) -> Result<Vec<FoActiveRow>>;

    /// Marks the rows voided and appends their negated mirrors (as the FO void does).
    fn reverse(
        &mut self,
        database: &str,
        transaction_ids: &[i64],
        comment: &str,
        ctx: &Ctx,
    ) -> Result<()>;

    /// Posts one line through the vendor procedure.
    fn post(&mut self, database: &str, line: &FoPostLine, ctx: &Ctx) -> Result<()>;
}
