//! Order verbs (`Wire/OrderCommands.cs`, `Wire/OrderComboCommands.cs`; OW1b ADR 0104,
//! OW1c ADR 0107, OW1d ADR 0110).

use dl_decimal::Decimal;
use dl_uuid::Uuid;

use super::fingerprint::{CheckStateFingerprint, CheckStateHeader, CheckStateLine};
use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Wire names of the order verbs.
pub mod order_command_types {
    /// Open a check on a table.
    pub const CHECK_OPEN: &str = "order.check_open";
    /// Post one item line.
    pub const ITEM_POST: &str = "order.item_post";
    /// Send the unsent item lines to the kitchen.
    pub const SEND: &str = "order.send";
    /// Void one item line.
    pub const ITEM_VOID: &str = "order.item_void";
    /// Set the covers of an open check.
    pub const COVERS: &str = "order.covers";
    /// Post a combo.
    pub const COMBO_POST: &str = "order.combo.post";
    /// All order verbs.
    pub const ALL: &[&str] = &[CHECK_OPEN, ITEM_POST, SEND, ITEM_VOID, COVERS, COMBO_POST];
}

/// Rejection reasons of the order verbs both sides key on (ADR 0104).
pub mod order_command_reasons {
    /// The check is not open in SMILE_POS.
    pub const CHECK_NOT_FOUND: &str = "check_not_found";
    /// The line to void is not on the check.
    pub const LINE_NOT_FOUND: &str = "line_not_found";
    /// The line to void is not an item line (`TrnCode` 100).
    pub const NOT_AN_ITEM_LINE: &str = "not_an_item_line";
    /// An unsent-line removal was asked for a line the kitchen already has.
    pub const ITEM_ALREADY_SENT: &str = "item_already_sent";
    /// An open-name item was posted without a name.
    pub const OPEN_NAME_REQUIRED: &str = "open_name_required";
    /// An open-price item was posted without an accepted price.
    pub const OPEN_PRICE_REQUIRED: &str = "open_price_required";
    /// The item code is not in `dbo.ItemDef`.
    pub const ITEM_NOT_FOUND: &str = "item_not_found";
    /// The store login lacks the grants the order verbs need (SQL 229/230/262).
    pub const SQL_GRANTS_MISSING: &str = "SQL_GRANTS_MISSING";
    /// A void on a check whose `tmpInvoice.Status` is not open.
    pub const BILL_NOT_OPEN: &str = "bill_not_open";
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Payload of [`order_command_types::CHECK_OPEN`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCheckOpenPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Table.
    pub table_no: String,
    /// Guests.
    pub num_guest: i32,
    /// Cashier.
    pub cashier: String,
}

impl OrderCheckOpenPayload {
    /// Longest table number.
    pub const MAX_TABLE_NO: usize = 50;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if blank(&self.table_no) || utf16_len(&self.table_no) > Self::MAX_TABLE_NO {
            return Some("table_no must be 1..50 characters");
        }
        if !(0..=999).contains(&self.num_guest) {
            return Some("num_guest must be 0..999");
        }
        if blank(&self.cashier) {
            Some("cashier is required")
        } else {
            None
        }
    }
}

impl WireJson for OrderCheckOpenPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("table_no").string(&self.table_no);
        w.key("num_guest").int(self.num_guest.into());
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckOpenPayload {
            rvc_no: o.int("rvc_no")?,
            table_no: o.string("table_no")?,
            num_guest: o.int("num_guest")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// Payload of [`order_command_types::ITEM_POST`]. An `open_name` longer than 30
/// characters is cut to 30 by the store.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderItemPostPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Cashier.
    pub cashier: String,
    /// Item code.
    pub item_code: String,
    /// Quantity.
    pub qty: Decimal,
    /// Seat.
    pub seat_no: i32,
    /// Price.
    pub price: Decimal,
    /// Kitchen note.
    pub note: Option<String>,
    /// Name of an open-name item.
    pub open_name: Option<String>,
    /// Accept 0 as the price of an open-price item.
    pub allow_zero_open_price: bool,
}

impl OrderItemPostPayload {
    /// Longest item code.
    pub const MAX_ITEM_CODE: usize = 30;
    /// Longest note.
    pub const MAX_NOTE: usize = 250;
    /// Longest open name.
    pub const MAX_OPEN_NAME: usize = 200;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if blank(&self.cashier) {
            return Some("cashier is required");
        }
        if blank(&self.item_code) || utf16_len(&self.item_code) > Self::MAX_ITEM_CODE {
            return Some("item_code must be 1..30 characters");
        }
        if self.qty <= Decimal::ZERO || self.qty > Decimal::from(9999) {
            return Some("qty must be in (0, 9999]");
        }
        if !(0..=999).contains(&self.seat_no) {
            return Some("seat_no must be 0..999");
        }
        if self.price < Decimal::ZERO {
            return Some("price must be >= 0");
        }
        if self
            .note
            .as_deref()
            .is_some_and(|n| utf16_len(n) > Self::MAX_NOTE)
        {
            return Some("note must be at most 250 characters");
        }
        if self
            .open_name
            .as_deref()
            .is_some_and(|n| utf16_len(n) > Self::MAX_OPEN_NAME)
        {
            Some("open_name must be at most 200 characters")
        } else {
            None
        }
    }
}

impl WireJson for OrderItemPostPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("cashier").string(&self.cashier);
        w.key("item_code").string(&self.item_code);
        w.key("qty").decimal(&self.qty);
        w.key("seat_no").int(self.seat_no.into());
        w.key("price").decimal(&self.price);
        json::opt_str(w, "note", &self.note);
        json::opt_str(w, "open_name", &self.open_name);
        w.key("allow_zero_open_price")
            .bool(self.allow_zero_open_price);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderItemPostPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            cashier: o.string("cashier")?,
            item_code: o.string("item_code")?,
            qty: o.decimal("qty")?,
            seat_no: o.int("seat_no")?,
            price: o.decimal("price")?,
            note: o.opt_string("note")?,
            open_name: o.opt_string("open_name")?,
            allow_zero_open_price: o.bool("allow_zero_open_price")?,
        })
    }
}

/// Payload of [`order_command_types::SEND`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderSendPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Table.
    pub table_no: Option<String>,
    /// Cashier.
    pub cashier: String,
    /// The cloud printed the kitchen ticket: flag the `PDASendOrder` row as printed (RT-1371).
    pub pda_handled: Option<bool>,
}

impl OrderSendPayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if blank(&self.cashier) {
            Some("cashier is required")
        } else {
            None
        }
    }
}

impl WireJson for OrderSendPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        json::opt_str(w, "table_no", &self.table_no);
        w.key("cashier").string(&self.cashier);
        json::opt_bool(w, "pda_handled", self.pda_handled);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderSendPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            table_no: o.opt_string("table_no")?,
            cashier: o.string("cashier")?,
            pda_handled: o.opt_bool("pda_handled")?,
        })
    }
}

/// Payload of [`order_command_types::ITEM_VOID`]: `sent` false removes an unsent line,
/// true voids a sent line with `reason`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderItemVoidPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Line.
    pub trn_seq: i64,
    /// Cashier.
    pub cashier: String,
    /// Whether the line was sent.
    pub sent: bool,
    /// Void reason.
    pub reason: Option<String>,
    /// Who authorised.
    pub author_cashier: Option<String>,
}

impl OrderItemVoidPayload {
    /// Longest reason.
    pub const MAX_REASON: usize = 200;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if self.trn_seq <= 0 {
            return Some("trn_seq must be positive");
        }
        if blank(&self.cashier) {
            return Some("cashier is required");
        }
        if self.sent && self.reason.as_deref().is_none_or(blank) {
            return Some("reason is required to void a sent line");
        }
        if self
            .reason
            .as_deref()
            .is_some_and(|r| utf16_len(r) > Self::MAX_REASON)
        {
            Some("reason must be at most 200 characters")
        } else {
            None
        }
    }
}

impl WireJson for OrderItemVoidPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("trn_seq").int(self.trn_seq);
        w.key("cashier").string(&self.cashier);
        w.key("sent").bool(self.sent);
        json::opt_str(w, "reason", &self.reason);
        json::opt_str(w, "author_cashier", &self.author_cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderItemVoidPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            trn_seq: o.int("trn_seq")?,
            cashier: o.string("cashier")?,
            sent: o.bool("sent")?,
            reason: o.opt_string("reason")?,
            author_cashier: o.opt_string("author_cashier")?,
        })
    }
}

/// Payload of [`order_command_types::COVERS`]: the new guest and child count.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCoversPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Guests.
    pub num_guest: i32,
    /// Children.
    pub num_child: i32,
    /// Cashier.
    pub cashier: String,
}

impl OrderCoversPayload {
    /// Largest count.
    pub const MAX_COUNT: i32 = 999;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if !(0..=Self::MAX_COUNT).contains(&self.num_guest) {
            return Some("num_guest must be 0..999");
        }
        if !(0..=Self::MAX_COUNT).contains(&self.num_child) {
            return Some("num_child must be 0..999");
        }
        if blank(&self.cashier) {
            Some("cashier is required")
        } else {
            None
        }
    }
}

impl WireJson for OrderCoversPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("num_guest").int(self.num_guest.into());
        w.key("num_child").int(self.num_child.into());
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCoversPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            num_guest: o.int("num_guest")?,
            num_child: o.int("num_child")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// One line of a check as SMILE_POS holds it right after an order command. The JSON
/// also carries the computed `is_item` (System.Text.Json writes public getters).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCheckLine {
    /// Line sequence.
    pub line_seq: i64,
    /// `TrnCode`, invariant text.
    pub transaction_code: Option<String>,
    /// Item code.
    pub item_code: Option<String>,
    /// Item name.
    pub item_name: Option<String>,
    /// Quantity.
    pub qty: Decimal,
    /// Price.
    pub price: Decimal,
    /// Amount.
    pub amount: Decimal,
    /// Seat.
    pub seat_no: i32,
    /// Cashier.
    pub cashier: Option<String>,
    /// Sent to the kitchen.
    pub sent: bool,
    /// Note.
    pub note: Option<String>,
}

impl OrderCheckLine {
    /// `TrnCode` of an item line.
    pub const ITEM_TRANSACTION_CODE: &'static str = "100";

    /// Whether this is an item line.
    pub fn is_item(&self) -> bool {
        self.transaction_code.as_deref().map(str::trim) == Some(Self::ITEM_TRANSACTION_CODE)
    }

    /// The fingerprint line.
    pub fn to_state_line(&self) -> CheckStateLine {
        CheckStateLine {
            line_seq: self.line_seq,
            transaction_code: self.transaction_code.clone(),
            item_code: self.item_code.clone(),
            qty: self.qty,
            price: self.price,
        }
    }
}

impl WireJson for OrderCheckLine {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("line_seq").int(self.line_seq);
        json::opt_str(w, "transaction_code", &self.transaction_code);
        json::opt_str(w, "item_code", &self.item_code);
        json::opt_str(w, "item_name", &self.item_name);
        w.key("qty").decimal(&self.qty);
        w.key("price").decimal(&self.price);
        w.key("amount").decimal(&self.amount);
        w.key("seat_no").int(self.seat_no.into());
        json::opt_str(w, "cashier", &self.cashier);
        w.key("sent").bool(self.sent);
        json::opt_str(w, "note", &self.note);
        w.key("is_item").bool(self.is_item());
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckLine {
            line_seq: o.int("line_seq")?,
            transaction_code: o.opt_string("transaction_code")?,
            item_code: o.opt_string("item_code")?,
            item_name: o.opt_string("item_name")?,
            qty: o.decimal("qty")?,
            price: o.decimal("price")?,
            amount: o.decimal("amount")?,
            seat_no: o.int("seat_no")?,
            cashier: o.opt_string("cashier")?,
            sent: o.bool("sent")?,
            note: o.opt_string("note")?,
        })
    }
}

/// Result of an applied order command: the check after the command and its fingerprint.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderCheckResult {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Table.
    pub table_no: Option<String>,
    /// Guests.
    pub guests: i32,
    /// Sub total.
    pub sub_total: Decimal,
    /// Paid.
    pub total_paid: Decimal,
    /// Lines.
    pub lines: Vec<OrderCheckLine>,
    /// Fingerprint after the command.
    pub fingerprint_after: String,
    /// The posted line (item post).
    pub trn_seq: Option<i64>,
    /// The lines this send fired.
    pub sent_seqs: Option<Vec<i64>>,
}

impl OrderCheckResult {
    /// The fingerprint header.
    pub fn to_state_header(&self) -> CheckStateHeader {
        CheckStateHeader {
            check_no: self.check_no,
            table_no: self.table_no.clone(),
            guests: self.guests,
            sub_total: self.sub_total,
            total_paid: self.total_paid,
        }
    }

    /// The fingerprint of a check read from SMILE_POS (header + every line).
    pub fn fingerprint(header: &CheckStateHeader, lines: &[OrderCheckLine]) -> String {
        let state: Vec<CheckStateLine> = lines.iter().map(OrderCheckLine::to_state_line).collect();
        CheckStateFingerprint::compute(header, &state)
    }

    /// `OrderCommandJson.ResultFrom`: the applied result in a command result, if it is one.
    pub fn result_from(element: Option<&dl_json::Value>) -> Option<OrderCheckResult> {
        element.and_then(OrderCheckResult::from_element)
    }
}

impl WireJson for OrderCheckResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        json::opt_str(w, "table_no", &self.table_no);
        w.key("guests").int(self.guests.into());
        w.key("sub_total").decimal(&self.sub_total);
        w.key("total_paid").decimal(&self.total_paid);
        w.key("lines");
        json::write_items(w, &self.lines);
        w.key("fingerprint_after").string(&self.fingerprint_after);
        json::opt_int(w, "trn_seq", self.trn_seq);
        if let Some(s) = &self.sent_seqs {
            w.key("sent_seqs");
            json::write_ints(w, s);
        }
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckResult {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            table_no: o.opt_string("table_no")?,
            guests: o.int("guests")?,
            sub_total: o.decimal("sub_total")?,
            total_paid: o.decimal("total_paid")?,
            lines: o.list("lines", json::item)?,
            fingerprint_after: o.string("fingerprint_after")?,
            trn_seq: o.opt_int("trn_seq")?,
            sent_seqs: o.opt_list("sent_seqs", json::int_item)?,
        })
    }
}

/// Aggregate keys of the order verbs.
pub mod order_aggregate {
    use super::Uuid;

    /// A new check has no number yet: its own aggregate (`check-open:{rvc}:{id:N}`).
    pub fn open(rvc_no: i32, id: Uuid) -> String {
        format!("check-open:{rvc_no}:{}", id.simple())
    }
}

/// Payload of [`order_command_types::COMBO_POST`] (OW1d, ADR 0110).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderComboPostPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Cashier.
    pub cashier: String,
    /// Combo code.
    pub combo_code: String,
    /// Quantity.
    pub qty: Decimal,
    /// Seat.
    pub seat_no: i32,
    /// Price of a combo without components.
    pub price: Decimal,
    /// Note.
    pub note: Option<String>,
    /// Components to keep (non-empty), each a component of the combo.
    pub selected_components: Option<Vec<String>>,
}

impl OrderComboPostPayload {
    /// Longest combo/component code.
    pub const MAX_COMBO_CODE: usize = 30;
    /// Longest note.
    pub const MAX_NOTE: usize = 250;
    /// Most selected components.
    pub const MAX_SELECTED: usize = 50;
    /// Delimiter of the selection the store receives.
    pub const SELECTED_DELIMITER: char = '|';

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if blank(&self.cashier) {
            return Some("cashier is required");
        }
        if blank(&self.combo_code) || utf16_len(&self.combo_code) > Self::MAX_COMBO_CODE {
            return Some("combo_code must be 1..30 characters");
        }
        if self.qty <= Decimal::ZERO || self.qty > Decimal::from(9999) {
            return Some("qty must be in (0, 9999]");
        }
        if !(0..=999).contains(&self.seat_no) {
            return Some("seat_no must be 0..999");
        }
        if self.price < Decimal::ZERO {
            return Some("price must be >= 0");
        }
        if self
            .note
            .as_deref()
            .is_some_and(|n| utf16_len(n) > Self::MAX_NOTE)
        {
            return Some("note must be at most 250 characters");
        }
        let Some(sel) = &self.selected_components else {
            return None;
        };
        if sel.len() > Self::MAX_SELECTED {
            return Some("selected_components must hold at most 50 codes");
        }
        if sel.iter().any(|c| {
            blank(c) || utf16_len(c) > Self::MAX_COMBO_CODE || c.contains(Self::SELECTED_DELIMITER)
        }) {
            Some("selected_components must be codes of 1..30 characters without '|'")
        } else {
            None
        }
    }

    /// The selected codes, trimmed and distinct (ordinal); empty for the whole combo.
    pub fn selected_codes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in self.selected_components.iter().flatten() {
            let t = c.trim();
            if !out.iter().any(|x| x == t) {
                out.push(t.to_owned());
            }
        }
        out
    }
}

impl WireJson for OrderComboPostPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("cashier").string(&self.cashier);
        w.key("combo_code").string(&self.combo_code);
        w.key("qty").decimal(&self.qty);
        w.key("seat_no").int(self.seat_no.into());
        w.key("price").decimal(&self.price);
        json::opt_str(w, "note", &self.note);
        if let Some(s) = &self.selected_components {
            w.key("selected_components");
            json::write_strings(w, s);
        }
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderComboPostPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            cashier: o.string("cashier")?,
            combo_code: o.string("combo_code")?,
            qty: o.decimal("qty")?,
            seat_no: o.int("seat_no")?,
            price: o.decimal("price")?,
            note: o.opt_string("note")?,
            selected_components: o.opt_list("selected_components", json::string_item)?,
        })
    }
}
