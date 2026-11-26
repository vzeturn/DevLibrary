//! Money verbs (`Wire/MoneyCommands.cs`; ADR 0174, RT-1297): tenders and the close of a
//! check on a site-authoritative tenant, written with the legacy PDA batches.

use dl_decimal::Decimal;

use crate::contracts::json::{self, WireJson};
use crate::contracts::utf16_len;
use crate::util::stj::{Obj, ReadError, Writer};

/// Wire names of the money verbs.
pub mod money_command_types {
    /// Add one base-currency tender.
    pub const TENDER_ADD: &str = "order.tender_add";
    /// Remove one tender line of an open check, with its `dbo.tmpPayment` rows.
    pub const TENDER_REMOVE: &str = "order.tender_remove";
    /// Close a balanced check.
    pub const CHECK_CLOSE: &str = "order.check_close";
    /// All money verbs.
    pub const ALL: &[&str] = &[TENDER_ADD, TENDER_REMOVE, CHECK_CLOSE];
}

/// Rejection reasons of the money verbs, on top of the order reasons.
pub mod money_command_reasons {
    /// A non cash/card tender is already on the check.
    pub const TENDER_LOCKED: &str = "tender_locked";
    /// The payment type has no `dbo.PaymentMode` row with a transaction code.
    pub const TENDER_NOT_CONFIGURED: &str = "tender_not_configured";
    /// A credit-card tender (TrnCode 400) came without a card type.
    pub const INVALID_CARD_TYPE: &str = "invalid_card_type";
    /// The site requires a bill number and none, or one of the wrong length, came.
    pub const BILL_NUMBER_INVALID: &str = "bill_number_invalid";
    /// The bill number is already on a closed bill.
    pub const BILL_NUMBER_IN_USE: &str = "bill_number_in_use";
    /// The tender line to remove is not a tender of the check.
    pub const TENDER_NOT_FOUND: &str = "tender_not_found";
    /// The close found the items above the tenders.
    pub const UNDERPAID: &str = "not_balanced_underpaid";
    /// The close found the tenders above the items.
    pub const OVERPAID: &str = "not_balanced_overpaid";
}

/// `string.IsNullOrWhiteSpace(s) || s.Length > max`.
fn bad_text(s: &str, max: usize) -> bool {
    s.trim().is_empty() || utf16_len(s) > max
}

/// Payload of [`money_command_types::TENDER_ADD`]: `amount` is what the tender settles,
/// `tendered` what the guest handed over, both in the base currency; `payment_type` is
/// `dbo.PaymentMode.PType`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderTenderAddPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Cashier.
    pub cashier: String,
    /// `dbo.PaymentMode.PType` (e.g. CASH, C.CARD).
    pub payment_type: String,
    /// What the tender settles.
    pub amount: Decimal,
    /// What the guest handed over.
    pub tendered: Decimal,
    /// Reference.
    pub reference: Option<String>,
    /// Card type of a credit-card tender.
    pub card_type: Option<i32>,
    /// Bill number.
    pub bill_no: Option<String>,
    /// The tender code of the cloud's payment-mode config (RT-1297); wins over
    /// `dbo.PaymentMode.TrnCode` when it is a settlement code of the store.
    pub trn_code: Option<i32>,
    /// Its sub code.
    pub trn_sub: Option<i32>,
}

impl OrderTenderAddPayload {
    /// Longest cashier.
    pub const MAX_CASHIER: usize = 10;
    /// Longest payment type.
    pub const MAX_PAYMENT_TYPE: usize = 20;
    /// Longest reference.
    pub const MAX_REFERENCE: usize = 50;
    /// Longest bill number.
    pub const MAX_BILL_NO: usize = 30;

    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        if bad_text(&self.cashier, Self::MAX_CASHIER) {
            return Some("cashier must be 1..10 characters");
        }
        if bad_text(&self.payment_type, Self::MAX_PAYMENT_TYPE) {
            return Some("payment_type must be 1..20 characters");
        }
        if self.payment_type.trim().to_uppercase() == "TIP" {
            return Some("a tip is not a tender");
        }
        if self.amount <= Decimal::ZERO || self.amount > Decimal::from(9_999_999_999_i64) {
            return Some("amount must be in (0, 9999999999]");
        }
        if self.tendered < self.amount {
            return Some("tendered must be >= amount");
        }
        if self
            .reference
            .as_deref()
            .is_some_and(|r| utf16_len(r) > Self::MAX_REFERENCE)
        {
            return Some("reference must be at most 50 characters");
        }
        if self
            .bill_no
            .as_deref()
            .is_some_and(|b| utf16_len(b) > Self::MAX_BILL_NO)
        {
            return Some("bill_no must be at most 30 characters");
        }
        if self.trn_code.is_some_and(|c| !(200..=9999).contains(&c)) {
            return Some("trn_code must be in 200..9999");
        }
        if self.trn_sub.is_some_and(|s| !(0..=9999).contains(&s)) {
            Some("trn_sub must be in 0..9999")
        } else {
            None
        }
    }
}

impl WireJson for OrderTenderAddPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("cashier").string(&self.cashier);
        w.key("payment_type").string(&self.payment_type);
        w.key("amount").decimal(&self.amount);
        w.key("tendered").decimal(&self.tendered);
        json::opt_str(w, "reference", &self.reference);
        json::opt_int(w, "card_type", self.card_type.map(i64::from));
        json::opt_str(w, "bill_no", &self.bill_no);
        json::opt_int(w, "trn_code", self.trn_code.map(i64::from));
        json::opt_int(w, "trn_sub", self.trn_sub.map(i64::from));
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderTenderAddPayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            cashier: o.string("cashier")?,
            payment_type: o.string("payment_type")?,
            amount: o.decimal("amount")?,
            tendered: o.decimal("tendered")?,
            reference: o.opt_string("reference")?,
            card_type: o.opt_int("card_type")?,
            bill_no: o.opt_string("bill_no")?,
            trn_code: o.opt_int("trn_code")?,
            trn_sub: o.opt_int("trn_sub")?,
        })
    }
}

/// Payload of [`money_command_types::TENDER_REMOVE`]: the tender line (`tmpTrn.TrnSeq`)
/// to take off an open check.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrderTenderRemovePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// The tender line.
    pub trn_seq: i64,
    /// Cashier.
    pub cashier: String,
}

impl OrderTenderRemovePayload {
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
        bad_text(&self.cashier, OrderTenderAddPayload::MAX_CASHIER)
            .then_some("cashier must be 1..10 characters")
    }
}

impl WireJson for OrderTenderRemovePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("trn_seq").int(self.trn_seq);
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderTenderRemovePayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            trn_seq: o.int("trn_seq")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// Payload of [`money_command_types::CHECK_CLOSE`]. The applied result is an
/// [`OrderCheckCloseResult`]: the check has left the open checks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrderCheckClosePayload {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// Cashier.
    pub cashier: String,
}

impl OrderCheckClosePayload {
    /// The validation problem, `None` when valid.
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if self.check_no <= 0 {
            return Some("check_no must be positive");
        }
        bad_text(&self.cashier, OrderTenderAddPayload::MAX_CASHIER)
            .then_some("cashier must be 1..10 characters")
    }
}

impl WireJson for OrderCheckClosePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("cashier").string(&self.cashier);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckClosePayload {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            cashier: o.string("cashier")?,
        })
    }
}

/// Applied result of [`money_command_types::CHECK_CLOSE`]: the closed bill as
/// `dbo.Invoice` holds it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OrderCheckCloseResult {
    /// RVC.
    pub rvc_no: i32,
    /// Check.
    pub check_no: i64,
    /// `CloseNumber`.
    pub close_no: i64,
    /// Items total.
    pub sub_total: Decimal,
    /// Tenders total.
    pub total_paid: Decimal,
    /// The check was closed before this command.
    pub already_closed: bool,
}

impl OrderCheckCloseResult {
    /// `MoneyCommandJson.CloseResultFrom`: the applied result of a close, if it is one.
    pub fn close_result_from(element: Option<&dl_json::Value>) -> Option<OrderCheckCloseResult> {
        element.and_then(OrderCheckCloseResult::from_element)
    }
}

impl WireJson for OrderCheckCloseResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("check_no").int(self.check_no);
        w.key("close_no").int(self.close_no);
        w.key("sub_total").decimal(&self.sub_total);
        w.key("total_paid").decimal(&self.total_paid);
        w.key("already_closed").bool(self.already_closed);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(OrderCheckCloseResult {
            rvc_no: o.int("rvc_no")?,
            check_no: o.int("check_no")?,
            close_no: o.int("close_no")?,
            sub_total: o.decimal("sub_total")?,
            total_paid: o.decimal("total_paid")?,
            already_closed: o.bool("already_closed")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tender(payment_type: &str, amount: i64) -> OrderTenderAddPayload {
        OrderTenderAddPayload {
            rvc_no: 3,
            check_no: 7001,
            cashier: "clerk-7".into(),
            payment_type: payment_type.into(),
            amount: Decimal::from(amount),
            tendered: Decimal::from(amount),
            reference: None,
            card_type: None,
            bill_no: None,
            trn_code: None,
            trn_sub: None,
        }
    }

    #[test]
    fn a_configured_code_is_a_settlement_code_range() {
        let ok = OrderTenderAddPayload {
            trn_code: Some(400),
            trn_sub: Some(1),
            ..tender("CASH", 10_000)
        };
        assert_eq!(ok.problem(), None);
        let low = OrderTenderAddPayload {
            trn_code: Some(199),
            ..tender("CASH", 10_000)
        };
        assert_eq!(low.problem(), Some("trn_code must be in 200..9999"));
        let sub = OrderTenderAddPayload {
            trn_code: Some(400),
            trn_sub: Some(-1),
            ..tender("CASH", 10_000)
        };
        assert_eq!(sub.problem(), Some("trn_sub must be in 0..9999"));
    }

    #[test]
    fn the_tender_rules_are_the_csharp_ones_in_order() {
        assert_eq!(tender("CASH", 10_000).problem(), None);
        assert_eq!(
            tender(" tip ", 10_000).problem(),
            Some("a tip is not a tender")
        );
        assert_eq!(
            tender("CASH", 0).problem(),
            Some("amount must be in (0, 9999999999]")
        );
        assert_eq!(
            tender("CASH", 10_000_000_000).problem(),
            Some("amount must be in (0, 9999999999]")
        );
        assert_eq!(tender("CASH", 9_999_999_999).problem(), None);
        let mut p = tender("CASH", 100);
        p.tendered = Decimal::from(99);
        assert_eq!(p.problem(), Some("tendered must be >= amount"));
        let mut p = tender("CASH", 100);
        p.cashier = "12345678901".into();
        assert_eq!(p.problem(), Some("cashier must be 1..10 characters"));
        let mut p = tender("CASH", 100);
        p.bill_no = Some("x".repeat(31));
        assert_eq!(p.problem(), Some("bill_no must be at most 30 characters"));
        assert_eq!(
            tender(&"C".repeat(21), 100).problem(),
            Some("payment_type must be 1..20 characters")
        );
    }

    #[test]
    fn the_wire_json_is_snake_case_with_nulls_omitted() {
        assert_eq!(
            tender("CASH", 250_000).to_json(),
            r#"{"rvc_no":3,"check_no":7001,"cashier":"clerk-7","payment_type":"CASH","amount":250000,"tendered":250000}"#
        );
        let closed = OrderCheckCloseResult {
            rvc_no: 3,
            check_no: 7001,
            close_no: 42,
            sub_total: Decimal::from(250_000),
            total_paid: Decimal::from(250_000),
            already_closed: false,
        };
        let json = closed.to_json();
        assert_eq!(
            json,
            r#"{"rvc_no":3,"check_no":7001,"close_no":42,"sub_total":250000,"total_paid":250000,"already_closed":false}"#
        );
        assert_eq!(OrderCheckCloseResult::from_json(&json).unwrap(), closed);
    }
}
