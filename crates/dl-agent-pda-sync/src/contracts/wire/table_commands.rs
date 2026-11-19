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
    /// All of them.
    pub const ALL: &[&str] = &[TABLE_CHANGE, TABLE_TRANSFER, CHECK_MERGE, ITEMS_MOVE];
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
