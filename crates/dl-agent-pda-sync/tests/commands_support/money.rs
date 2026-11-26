//! The money verbs over the fake book (port of the C# `FakeOrderCommandUnit.Money`,
//! ADR 0174): a tender is a non-item line (transaction code [`TENDER_CODE`]), the close
//! checks the balance like the batch, marks the check closed and drops its lines, and a
//! repeated close answers already closed.

use dl_agent_pda_sync::commands::{OrderCloseOutcome, OrderPostOutcome};
use dl_agent_pda_sync::contracts::wire::{
    OrderCheckClosePayload, OrderCheckCloseResult, OrderCheckLine, OrderTenderAddPayload,
    OrderTenderRemovePayload, money_command_reasons as money, order_command_reasons as reasons,
};
use dl_decimal::Decimal;

use super::Unit;

/// The transaction code of a fake tender line.
pub const TENDER_CODE: &str = "300";

impl Unit<'_> {
    fn is_open(&self, rvc_no: i32, check_no: i64) -> bool {
        self.s
            .orders
            .checks
            .get(&(rvc_no, check_no))
            .is_some_and(|c| c.status == 1)
    }

    pub(super) fn fake_add_tender(&mut self, p: &OrderTenderAddPayload) -> OrderPostOutcome {
        self.owner.call(format!(
            "tender {} {} {}",
            p.check_no, p.payment_type, p.amount
        ));
        if !self.is_open(p.rvc_no, p.check_no) {
            return OrderPostOutcome::refused(reasons::BILL_NOT_OPEN);
        }
        let seq = self.s.orders.next_trn_seq;
        self.s.orders.next_trn_seq += 1;
        self.s.orders.lines.insert(
            (p.check_no, seq),
            OrderCheckLine {
                line_seq: seq,
                transaction_code: Some(TENDER_CODE.to_owned()),
                item_code: Some(p.payment_type.clone()),
                item_name: Some(p.payment_type.clone()),
                qty: Decimal::ONE,
                price: p.amount,
                amount: p.amount,
                seat_no: 0,
                cashier: Some(p.cashier.clone()),
                sent: true,
                note: p.reference.clone(),
            },
        );
        self.touch(p.rvc_no, p.check_no);
        OrderPostOutcome::posted(seq)
    }

    pub(super) fn fake_remove_tender(&mut self, p: &OrderTenderRemovePayload) -> Option<String> {
        self.owner
            .call(format!("untender {} {}", p.check_no, p.trn_seq));
        if !self.is_open(p.rvc_no, p.check_no) {
            return Some(reasons::BILL_NOT_OPEN.to_owned());
        }
        let key = (p.check_no, p.trn_seq);
        let is_tender = self
            .s
            .orders
            .lines
            .get(&key)
            .is_some_and(|l| l.transaction_code.as_deref() == Some(TENDER_CODE));
        if !is_tender {
            return Some(money::TENDER_NOT_FOUND.to_owned());
        }
        self.s.orders.lines.remove(&key);
        self.touch(p.rvc_no, p.check_no);
        None
    }

    pub(super) fn fake_close_check(&mut self, p: &OrderCheckClosePayload) -> OrderCloseOutcome {
        self.owner.call(format!("close {}", p.check_no));
        let key = (p.rvc_no, p.check_no);
        let Some(status) = self.s.orders.checks.get(&key).map(|c| c.status) else {
            return OrderCloseOutcome::refused(reasons::BILL_NOT_OPEN);
        };
        let bill = |sub_total, total_paid, already_closed| OrderCheckCloseResult {
            rvc_no: p.rvc_no,
            check_no: p.check_no,
            close_no: p.check_no,
            sub_total,
            total_paid,
            already_closed,
        };
        if status == 2 {
            return OrderCloseOutcome::done(bill(Decimal::ZERO, Decimal::ZERO, true));
        }
        let lines = self
            .s
            .orders
            .lines
            .iter()
            .filter(|(k, _)| k.0 == p.check_no);
        let (mut sub_total, mut paid) = (Decimal::ZERO, Decimal::ZERO);
        for (_, l) in lines {
            if l.is_item() {
                sub_total += l.amount;
            }
            if l.transaction_code.as_deref() == Some(TENDER_CODE) {
                paid += l.amount;
            }
        }
        if paid < sub_total {
            return OrderCloseOutcome::refused(money::UNDERPAID);
        }
        if paid > sub_total {
            return OrderCloseOutcome::refused(money::OVERPAID);
        }
        if let Some(c) = self.s.orders.checks.get_mut(&key) {
            c.status = 2;
        }
        self.s.orders.lines.retain(|k, _| k.0 != p.check_no);
        OrderCloseOutcome::done(bill(sub_total, paid, false))
    }
}
