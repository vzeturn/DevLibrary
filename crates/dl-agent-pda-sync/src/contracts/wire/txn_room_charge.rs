//! Room charge of a cloud-authoritative bill into the Front Office folio (`Wire/TxnRoomCharge.cs`,
//! ADR 0162, RT-0908): the verbs `txn.room_charge` and `txn.room_charge_void`. One aggregate per
//! bill (`room_charge:{CheckNo}`, kept apart from the bill's snapshot so the versions never
//! collide).

use dl_decimal::Decimal;

use crate::contracts::DateTime;
use crate::contracts::json::WireJson;
use crate::contracts::utf16_len;
use crate::contracts::wire::txn_commands::txn_scopes;
use crate::util::stj::{Obj, ReadError, Writer};

/// `TxnRoomChargeScope`: the folio posting aggregate of a bill.
pub mod txn_room_charge_scope {
    use super::txn_scopes;

    /// The scope name; the key is the bill's check number.
    pub const SCOPE: &str = "room_charge";
    /// Highest room number length the folio holds (`Folio.RoomCode` is `varchar(10)`).
    pub const MAX_ROOM_LENGTH: usize = 10;

    /// The aggregate, e.g. `room_charge:70001`.
    pub fn aggregate(check_no: i64) -> String {
        txn_scopes::aggregate(SCOPE, check_no)
    }

    /// Queue aggregate key, e.g. `txn:room_charge:70001`.
    pub fn aggregate_key(check_no: i64) -> String {
        txn_scopes::aggregate_key(SCOPE, check_no)
    }
}

/// Payload of `txn.room_charge` (`TxnRoomChargePayload`): the full state of the room charge of
/// bill `check_no`, as POS WIN's `PostInterfaceTransactionToFolio` would post it. `folio_num` 0
/// means the agent finds the room's one in-house, postable folio. `trn_code` / `trn_sub` is the
/// POS tender code of the ROOM CHARGE mode, mapped through `TrnLinkByRVC`. Times are store-local
/// ISO without offset.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnRoomChargePayload {
    /// Outlet of the bill.
    pub rvc: i32,
    /// The bill.
    pub check_no: i64,
    /// Aggregate version.
    pub version: i64,
    /// Room number.
    pub room_no: String,
    /// Folio to post to; 0 = the room's one in-house folio.
    pub folio_num: i64,
    /// POS tender code of the ROOM CHARGE mode.
    pub trn_code: i32,
    /// POS tender sub-code.
    pub trn_sub: i32,
    /// Total charged to the room.
    pub amount: Decimal,
    /// Meal period.
    pub meal_period: i32,
    /// Optional comment.
    pub comment: Option<String>,
    /// Cashier of the bill.
    pub cashier: String,
    /// Store-local moment of the charge.
    pub at: String,
    /// Who sent it.
    pub actor: String,
}

impl TxnRoomChargePayload {
    /// Longest comment (and void reason).
    pub const MAX_COMMENT_LENGTH: usize = 200;

    /// Shape problems both sides check before any SQL runs; `None` when well-formed
    /// (`Problem`, same messages and order).
    pub fn problem(&self) -> Option<String> {
        if let Some(p) = bill_problem(self.rvc, self.check_no, self.version) {
            return Some(p);
        }
        if self.room_no.trim().is_empty()
            || utf16_len(self.room_no.trim()) > txn_room_charge_scope::MAX_ROOM_LENGTH
        {
            return Some(format!(
                "RoomNo must be 1..{} characters.",
                txn_room_charge_scope::MAX_ROOM_LENGTH
            ));
        }
        if self.folio_num < 0 {
            return Some("FolioNum must not be negative.".into());
        }
        if self.trn_code <= 0 || self.trn_sub < 0 {
            return Some("TrnCode and TrnSub must identify the room charge tender.".into());
        }
        if !self.amount.is_positive()
            || self.amount > Decimal::from(99_999_999_999_i64)
            || self.amount.round(2) != self.amount
        {
            return Some("Amount must be positive with at most 2 decimals.".into());
        }
        if !(0..=99).contains(&self.meal_period) {
            return Some("MealPeriod must be 0..99.".into());
        }
        if self
            .comment
            .as_deref()
            .is_some_and(|c| utf16_len(c) > Self::MAX_COMMENT_LENGTH)
        {
            return Some("Comment is too long.".into());
        }
        if self.cashier.trim().is_empty() {
            return Some("Cashier is required.".into());
        }
        if DateTime::try_parse_invariant(&self.at).is_none() {
            return Some("At must be a local date-time.".into());
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnRoomChargePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(i64::from(self.rvc));
        w.key("check_no").int(self.check_no);
        w.key("version").int(self.version);
        w.key("room_no").string(&self.room_no);
        w.key("folio_num").int(self.folio_num);
        w.key("trn_code").int(i64::from(self.trn_code));
        w.key("trn_sub").int(i64::from(self.trn_sub));
        w.key("amount").decimal(&self.amount);
        w.key("meal_period").int(i64::from(self.meal_period));
        w.opt_str_member("comment", self.comment.as_deref());
        w.key("cashier").string(&self.cashier);
        w.key("at").string(&self.at);
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnRoomChargePayload {
            rvc: o.int("rvc")?,
            check_no: o.int("check_no")?,
            version: o.int("version")?,
            room_no: o.string("room_no")?,
            folio_num: o.int("folio_num")?,
            trn_code: o.int("trn_code")?,
            trn_sub: o.int("trn_sub")?,
            amount: o.decimal("amount")?,
            meal_period: o.int("meal_period")?,
            comment: o.opt_string("comment")?,
            cashier: o.string("cashier")?,
            at: o.string("at")?,
            actor: o.string("actor")?,
        })
    }
}

/// Payload of `txn.room_charge_void` (`TxnRoomChargeVoidPayload`): reverses every active folio
/// row the bill's room charge posted. A void before any post applies as a no-op that makes an
/// older post stale.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TxnRoomChargeVoidPayload {
    /// Outlet of the bill.
    pub rvc: i32,
    /// The bill.
    pub check_no: i64,
    /// Aggregate version.
    pub version: i64,
    /// Why the charge is voided.
    pub reason: String,
    /// Who sent it.
    pub actor: String,
}

impl TxnRoomChargeVoidPayload {
    /// Shape problems both sides check before any SQL runs; `None` when well-formed.
    pub fn problem(&self) -> Option<String> {
        if let Some(p) = bill_problem(self.rvc, self.check_no, self.version) {
            return Some(p);
        }
        if self.reason.trim().is_empty()
            || utf16_len(&self.reason) > TxnRoomChargePayload::MAX_COMMENT_LENGTH
        {
            return Some(format!(
                "Reason must be 1..{} characters.",
                TxnRoomChargePayload::MAX_COMMENT_LENGTH
            ));
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnRoomChargeVoidPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(i64::from(self.rvc));
        w.key("check_no").int(self.check_no);
        w.key("version").int(self.version);
        w.key("reason").string(&self.reason);
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnRoomChargeVoidPayload {
            rvc: o.int("rvc")?,
            check_no: o.int("check_no")?,
            version: o.int("version")?,
            reason: o.string("reason")?,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied room charge or void (`TxnRoomChargeResult`). `folio_num` is the folio
/// posted to (0 when the bill never posted); `active_amount` / `active_rows` the bill's active
/// (`'A'`) folio rows after the command; `hash` the SHA-256 (lower hex) over the sorted lines
/// [`room_charge_hash_line`] of those rows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnRoomChargeResult {
    /// The aggregate, e.g. `room_charge:70001`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// `applied` or `stale`.
    pub outcome: String,
    /// Version recorded after the command.
    pub applied_version: i64,
    /// Folio posted to.
    pub folio_num: i64,
    /// Sum of the active rows.
    pub active_amount: Decimal,
    /// Number of active rows.
    pub active_rows: i32,
    /// Hash of the active rows.
    pub hash: String,
}

impl WireJson for TxnRoomChargeResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("aggregate").string(&self.aggregate);
        w.key("version").int(self.version);
        w.key("outcome").string(&self.outcome);
        w.key("applied_version").int(self.applied_version);
        w.key("folio_num").int(self.folio_num);
        w.key("active_amount").decimal(&self.active_amount);
        w.key("active_rows").int(i64::from(self.active_rows));
        w.key("hash").string(&self.hash);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnRoomChargeResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            folio_num: o.int("folio_num")?,
            active_amount: o.decimal("active_amount")?,
            active_rows: o.int("active_rows")?,
            hash: o.string("hash")?,
        })
    }
}

/// One hash line of an active folio row: `{TransactionID}|{TransactionAmount:0.00}\n`
/// (invariant culture, two decimals, midpoint away from zero as .NET formats).
pub fn room_charge_hash_line(transaction_id: i64, amount: Decimal) -> String {
    format!("{transaction_id}|{amount:.2}\n")
}

/// The first three checks both payloads share (`Rvc`, `CheckNo`, `Version`).
fn bill_problem(rvc: i32, check_no: i64, version: i64) -> Option<String> {
    if !(1..=255).contains(&rvc) {
        return Some("Rvc must be 1..255.".into());
    }
    if check_no <= 0 {
        return Some("CheckNo must be positive.".into());
    }
    if version <= 0 {
        return Some("Version must be positive.".into());
    }
    None
}
