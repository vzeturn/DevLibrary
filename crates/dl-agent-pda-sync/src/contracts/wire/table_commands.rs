//! Table and multi-check verbs (`Wire/TableCommands.cs`, OW1e ADR 0112).

use super::order::{OrderCheckOpenPayload, OrderCheckResult, OrderCoversPayload};
use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Wire names of the table and multi-check verbs.
pub mod table_command_types {
    /// Set the table and guest count of an open check.
    pub const TABLE_CHANGE: &str = "order.table_change";
    /// Move an open check to another table.
    pub const TABLE_TRANSFER: &str = "order.table_transfer";
    /// Merge one open check into another and close the emptied one.
    pub const CHECK_MERGE: &str = "order.check_merge";
    /// Move chosen item lines to another open check.
    pub const ITEMS_MOVE: &str = "order.items_move";
    /// Close a stale empty check on the named table (ADR 0144, RT-0775): the agent
    /// re-checks emptiness, table, age and payment in its own transaction first.
    pub const CHECK_CANCEL_EMPTY: &str = "order.check_cancel_empty";
    /// Write the guest of an open check to the store's `TmpInvoice.PONumber` (RT-1413);
    /// an empty value clears it.
    pub const PO_NUMBER: &str = "order.po_number";
    /// All of them.
    pub const ALL: &[&str] = &[
        TABLE_CHANGE,
        TABLE_TRANSFER,
        CHECK_MERGE,
        ITEMS_MOVE,
        CHECK_CANCEL_EMPTY,
        PO_NUMBER,
    ];
    /// The verbs whose payload carries the expected state of every check they touch.
    pub const MULTI_CHECK: &[&str] = &[CHECK_MERGE, ITEMS_MOVE];
}

/// Rejection reasons of the table and multi-check verbs (ADR 0112).
pub mod table_command_reasons {
    /// A store column the legacy batch writes is missing in this build.
    pub const SCHEMA_UNSUPPORTED: &str = "schema_unsupported";
    /// A touched check carries a payment.
    pub const BILL_HAS_PAYMENT: &str = "bill_has_payment";
    /// A transfer to the table the check is already on.
    pub const SAME_TABLE: &str = "same_table";
    /// The move target is missing, not open, in another outlet or the source itself.
    pub const INVALID_MOVE_TARGET: &str = "invalid_move_target";
    /// The move source carries a discount line.
    pub const DISCOUNT_PRESENT: &str = "discount_present";
    /// The check is not on the table the command names.
    pub const TABLE_MISMATCH: &str = "table_mismatch";
    /// The check was opened at or after the command's cutoff.
    pub const BILL_NOT_STALE: &str = "bill_not_stale";
    /// The check carries at least one line.
    pub const BILL_NOT_EMPTY: &str = "bill_not_empty";
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Payload of [`table_command_types::TABLE_CHANGE`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderTableChangePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Table.
    pub table_no: String,
    /// Guests.
    pub num_guest: i32,
    /// Cashier.
    pub cashier: String,
}

impl OrderTableChangePayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if blank(&self.table_no) || utf16_len(&self.table_no) > OrderCheckOpenPayload::MAX_TABLE_NO
        {
            return Some("table_no must be 1..50 characters");
        }
        if !(0..=OrderCoversPayload::MAX_COUNT).contains(&self.num_guest) {
            return Some("num_guest must be 0..999");
        }
        if blank(&self.cashier) {
            Some("cashier is required")
        } else {
            None
        }
    }
}

impl WireJson for OrderTableChangePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("table_no").string(&self.table_no);
        w.key("num_guest").int(self.num_guest.into());
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderTableChangePayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            table_no: o.string("table_no")?,
            num_guest: o.int("num_guest")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// Payload of [`table_command_types::TABLE_TRANSFER`]; the store writes `to_table_no`
/// trimmed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderTableTransferPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Target table.
    pub to_table_no: String,
    /// Cashier.
    pub cashier: String,
}

impl OrderTableTransferPayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if blank(&self.to_table_no)
            || utf16_len(self.to_table_no.trim()) > OrderCheckOpenPayload::MAX_TABLE_NO
        {
            return Some("to_table_no must be 1..50 characters");
        }
        if blank(&self.cashier) {
            Some("cashier is required")
        } else {
            None
        }
    }
}

impl WireJson for OrderTableTransferPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("to_table_no").string(&self.to_table_no);
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderTableTransferPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            to_table_no: o.string("to_table_no")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// Payload of [`table_command_types::CHECK_CANCEL_EMPTY`]. `opened_before` is a
/// store-local wall-clock timestamp `yyyy-MM-ddTHH:mm:ss` (the start of the current
/// business day): the agent cancels only a check whose `OpenTime` is earlier.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCheckCancelEmptyPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// The table the check must be on.
    pub table_no: String,
    /// Recorded as the cancelling clerk.
    pub cashier: String,
    /// Cutoff, `yyyy-MM-ddTHH:mm:ss`.
    pub opened_before: String,
}

impl OrderCheckCancelEmptyPayload {
    /// Length of `yyyy-MM-ddTHH:mm:ss`.
    pub const OPENED_BEFORE_LENGTH: usize = 19;

    /// The store's `Cashier`/`CloseBy` columns are `varchar(10)` (ADR 0125).
    pub const MAX_CASHIER: usize = 10;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if blank(&self.table_no)
            || utf16_len(self.table_no.trim()) > OrderCheckOpenPayload::MAX_TABLE_NO
        {
            return Some("table_no must be 1..50 characters");
        }
        if blank(&self.cashier) || utf16_len(&self.cashier) > Self::MAX_CASHIER {
            return Some("cashier must be 1..10 characters");
        }
        if is_local_timestamp(&self.opened_before) {
            None
        } else {
            Some("opened_before must be yyyy-MM-ddTHH:mm:ss")
        }
    }
}

/// True for a well-formed `yyyy-MM-ddTHH:mm:ss` with a real calendar date and time
/// (`DateTime.TryParseExact` in the .NET contract).
pub fn is_local_timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() != OrderCheckCancelEmptyPayload::OPENED_BEFORE_LENGTH {
        return false;
    }
    let shape = b"dddd-dd-ddTdd:dd:dd";
    for (c, k) in b.iter().zip(shape) {
        let ok = if *k == b'd' {
            c.is_ascii_digit()
        } else {
            c == k
        };
        if !ok {
            return false;
        }
    }
    let n = |from: usize, to: usize| -> u32 { value[from..to].parse().unwrap_or(u32::MAX) };
    let (year, month, day) = (n(0, 4), n(5, 7), n(8, 10));
    let (hour, minute, second) = (n(11, 13), n(14, 16), n(17, 19));
    if year == 0 || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&day)
}

impl WireJson for OrderCheckCancelEmptyPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("table_no").string(&self.table_no);
        w.key("cashier").string(&self.cashier);
        w.key("opened_before").string(&self.opened_before);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckCancelEmptyPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            table_no: o.string("table_no")?,
            cashier: o.string("cashier")?,
            opened_before: o.string("opened_before")?,
        })
    }
}

/// Payload of [`table_command_types::PO_NUMBER`] (RT-1413): `"<room>-<guest>"`, composed
/// and capped by the cloud; empty clears the column. The agent writes it TCVN3-encoded and
/// cut to the store column's own length.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderPoNumberPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// The text for `TmpInvoice.PONumber`; `None` only when the member is missing or
    /// null, which is refused like the C# null.
    pub po_number: Option<String>,
}

impl OrderPoNumberPayload {
    /// The cloud's cap, in UTF-16 units like the .NET `string.Length`.
    pub const MAX_LENGTH: usize = 50;

    /// The aggregate the command is queued under: apart from `check:{rvc}:{check}`, so the
    /// guest write never waits behind the check's order verbs (and they never wait on it).
    pub fn aggregate_key(rvc_no: i32, check_no: i64) -> String {
        format!("checkguest:{rvc_no}:{check_no}")
    }

    /// The validation problem, `None` when valid (an empty value is valid: it clears).
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        let Some(text) = self.po_number.as_deref() else {
            return Some("po_number is required (empty clears it)");
        };
        if utf16_len(text) > Self::MAX_LENGTH {
            return Some("po_number must be at most 50 characters");
        }
        if text.chars().any(char::is_control) {
            return Some("po_number must not carry control characters");
        }
        None
    }
}

impl WireJson for OrderPoNumberPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        if let Some(text) = &self.po_number {
            w.key("po_number").string(text);
        }
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderPoNumberPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            po_number: o.opt_string("po_number")?,
        })
    }
}

/// One check a multi-check verb touches, with the fingerprint the user saw for it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCheckExpectation {
    /// Check.
    pub check_no: i64,
    /// Expected fingerprint.
    pub expected: String,
}

impl OrderCheckExpectation {
    /// Longest fingerprint.
    pub const MAX_EXPECTED: usize = 128;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.check_no <= 0 {
            return Some("checks[].check_no must be positive");
        }
        if blank(&self.expected) || utf16_len(&self.expected) > Self::MAX_EXPECTED {
            Some("checks[].expected must be 1..128 characters")
        } else {
            None
        }
    }

    /// The problem of a list that must name exactly `first` and `second`, once each.
    pub fn problem_of(
        checks: &[OrderCheckExpectation],
        first: i64,
        second: i64,
    ) -> Option<&'static str> {
        const TWO: &str = "checks must list the two checks the command touches";
        if checks.len() != 2 {
            return Some(TWO);
        }
        if let Some(p) = checks.iter().find_map(OrderCheckExpectation::problem) {
            return Some(p);
        }
        let mut named: Vec<i64> = checks.iter().map(|c| c.check_no).collect();
        named.sort_unstable();
        named.dedup();
        let mut want = vec![first, second];
        want.sort_unstable();
        want.dedup();
        if named == want { None } else { Some(TWO) }
    }
}

impl WireJson for OrderCheckExpectation {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("check_no").int(self.check_no);
        w.key("expected").string(&self.expected);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckExpectation {
            check_no: o.int("check_no")?,
            expected: o.string("expected")?,
        })
    }
}

/// Payload of [`table_command_types::CHECK_MERGE`]: every line of the source moves to
/// the target.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCheckMergePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Target check.
    pub target_check_no: i64,
    /// Source check.
    pub source_check_no: i64,
    /// Cashier.
    pub cashier: String,
    /// Expectations of both checks.
    pub checks: Vec<OrderCheckExpectation>,
}

impl OrderCheckMergePayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.target_check_no <= 0 || self.source_check_no <= 0 {
            return Some("target_check_no and source_check_no must be positive");
        }
        if self.target_check_no == self.source_check_no {
            return Some("target_check_no and source_check_no must differ");
        }
        if blank(&self.cashier) {
            return Some("cashier is required");
        }
        OrderCheckExpectation::problem_of(&self.checks, self.target_check_no, self.source_check_no)
    }
}

impl WireJson for OrderCheckMergePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("target_check_no").int(self.target_check_no);
        w.key("source_check_no").int(self.source_check_no);
        w.key("cashier").string(&self.cashier);
        w.key("checks");
        json::write_items(w, &self.checks);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckMergePayload {
            rvc_no: o.int("rvc_no")?,
            target_check_no: o.int("target_check_no")?,
            source_check_no: o.int("source_check_no")?,
            cashier: o.string("cashier")?,
            checks: o.list("checks", json::item)?,
        })
    }
}

/// Payload of [`table_command_types::ITEMS_MOVE`]: the lines `trn_seqs` move to the
/// target.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderItemsMovePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Source check.
    pub from_check_no: i64,
    /// Target check.
    pub to_check_no: i64,
    /// Lines.
    pub trn_seqs: Vec<i64>,
    /// Cashier.
    pub cashier: String,
    /// Expectations of both checks.
    pub checks: Vec<OrderCheckExpectation>,
}

impl OrderItemsMovePayload {
    /// Most lines per move.
    pub const MAX_LINES: usize = 200;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.from_check_no <= 0 || self.to_check_no <= 0 {
            return Some("from_check_no and to_check_no must be positive");
        }
        if self.trn_seqs.is_empty() || self.trn_seqs.len() > Self::MAX_LINES {
            return Some("trn_seqs must list 1..200 lines");
        }
        if self.trn_seqs.iter().any(|s| *s <= 0) {
            return Some("trn_seqs must be positive");
        }
        if blank(&self.cashier) {
            return Some("cashier is required");
        }
        // A move onto the source itself is a business refusal (invalid_move_target).
        if self.from_check_no == self.to_check_no {
            None
        } else {
            OrderCheckExpectation::problem_of(&self.checks, self.from_check_no, self.to_check_no)
        }
    }
}

impl WireJson for OrderItemsMovePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("from_check_no").int(self.from_check_no);
        w.key("to_check_no").int(self.to_check_no);
        w.key("trn_seqs");
        json::write_ints(w, &self.trn_seqs);
        w.key("cashier").string(&self.cashier);
        w.key("checks");
        json::write_items(w, &self.checks);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderItemsMovePayload {
            rvc_no: o.int("rvc_no")?,
            from_check_no: o.int("from_check_no")?,
            to_check_no: o.int("to_check_no")?,
            trn_seqs: o.list("trn_seqs", json::int_item)?,
            cashier: o.string("cashier")?,
            checks: o.list("checks", json::item)?,
        })
    }
}

/// Result of an applied multi-check verb: the still-open touched checks and the closed
/// ones.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderMultiCheckResult {
    /// Open checks after the command.
    pub checks: Vec<OrderCheckResult>,
    /// Checks the command closed.
    pub removed: Vec<i64>,
}

impl OrderMultiCheckResult {
    /// The state of `check_no` after the command, if open.
    pub fn check(&self, check_no: i64) -> Option<&OrderCheckResult> {
        self.checks.iter().find(|c| c.check_no == check_no)
    }

    /// `TableCommandJson.MultiResultFrom`.
    pub fn multi_result_from(element: Option<&dl_json::Value>) -> Option<OrderMultiCheckResult> {
        element.and_then(OrderMultiCheckResult::from_element)
    }
}

impl WireJson for OrderMultiCheckResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("checks");
        json::write_items(w, &self.checks);
        w.key("removed");
        json::write_ints(w, &self.removed);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderMultiCheckResult {
            checks: o.list("checks", json::item)?,
            removed: o.list("removed", json::int_item)?,
        })
    }
}

/// Aggregate keys of the multi-check verbs (ADR 0112 §3).
pub mod multi_check_aggregate {
    use crate::contracts::wire::check_aggregate;

    /// The key the command is queued under (the lowest check) and the keys of the other
    /// checks it serialises against.
    pub fn keys(rvc_no: i32, first: i64, second: i64) -> (String, Vec<String>) {
        let low = first.min(second);
        let high = first.max(second);
        let related = if low == high {
            Vec::new()
        } else {
            vec![check_aggregate::key(rvc_no, high)]
        };
        (check_aggregate::key(rvc_no, low), related)
    }
}
