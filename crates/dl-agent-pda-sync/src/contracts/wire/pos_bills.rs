//! `pos.bill_list` contract (`Wire/PosBillList.cs`, RT-0842): the POS WIN bills of one RVC
//! filed under one store business date, for the cloud's bill-by-bill reconciliation.

use dl_decimal::Decimal;

use super::pos_revenue::PosRevenueSummaryPayload;
use crate::contracts::json::{self, WireJson};
use crate::util::stj::{Obj, ReadError, Writer};

/// Payload of [`site_command_types::POS_BILL_LIST`](super::site_command_types::POS_BILL_LIST):
/// the bills of RVC `rvc_no` filed under `business_date` (`yyyy-MM-dd`, POS WIN's
/// `IYear/IMonth/IDay`). `reason` is a free-text note for the agent log.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosBillListPayload {
    /// RVC.
    pub rvc_no: i32,
    /// Business date, `yyyy-MM-dd`.
    pub business_date: String,
    /// Why the cloud asks.
    pub reason: Option<String>,
}

impl PosBillListPayload {
    /// Bills per answer; a busier day answers the first ones by bill number, truncated.
    pub const MAX_BILLS: usize = 2000;

    /// Aggregate key of this command: reads never order against a check.
    pub fn aggregate_key(rvc_no: i32) -> String {
        format!("pos-bills:{rvc_no}")
    }

    /// The validation problem, `None` when valid (`PosBillListPayload.Problem`).
    pub fn problem(&self) -> Option<&'static str> {
        if self.rvc_no <= 0 {
            return Some("rvc_no must be positive");
        }
        if PosRevenueSummaryPayload::try_day(&self.business_date).is_none() {
            return Some("business_date must be yyyy-MM-dd");
        }
        None
    }
}

impl WireJson for PosBillListPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("business_date").string(&self.business_date);
        json::opt_str(w, "reason", &self.reason);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosBillListPayload {
            rvc_no: o.int("rvc_no")?,
            business_date: o.string("business_date")?,
            reason: o.opt_string("reason")?,
        })
    }
}

/// Values of [`PosBill::status`] (`PosBillStatuses`).
pub mod pos_bill_statuses {
    /// A closed bill (`dbo.Invoice`).
    pub const CLOSED: &str = "closed";
    /// A cancelled bill (`dbo.InvoiceCxl`).
    pub const CANCELLED: &str = "cancelled";
}

/// The tender sum of one payment method of a bill (`PosBillPayment`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosBillPayment {
    /// Payment code (`Trn.ItemCode`, or the transaction code when blank).
    pub method: String,
    /// Tendered amount (positive).
    pub amount: Decimal,
}

impl WireJson for PosBillPayment {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("method").string(&self.method);
        w.key("amount").decimal(&self.amount);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosBillPayment {
            method: o.string("method")?,
            amount: o.decimal("amount")?,
        })
    }
}

/// One bill (`PosBill`): number, status, base total and tenders per method; no names.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosBill {
    /// Bill number.
    pub check_no: i64,
    /// A [`pos_bill_statuses`] value.
    pub status: String,
    /// The bill's base total (`TotalBase`).
    pub total: Decimal,
    /// Tenders per method (closed bills only).
    pub payments: Vec<PosBillPayment>,
}

impl WireJson for PosBill {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("check_no").int(self.check_no);
        w.key("status").string(&self.status);
        w.key("total").decimal(&self.total);
        w.key("payments");
        json::write_items(w, &self.payments);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosBill {
            check_no: o.int("check_no")?,
            status: o.string("status")?,
            total: o.decimal("total")?,
            payments: o.list("payments", json::item)?,
        })
    }
}

/// Result of `pos.bill_list` (`PosBillListResult`): the bills by number, ascending.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PosBillListResult {
    /// RVC.
    pub rvc_no: i32,
    /// Business date asked.
    pub business_date: String,
    /// The bills.
    pub bills: Vec<PosBill>,
    /// More than [`PosBillListPayload::MAX_BILLS`] bills: only the first ones are listed.
    pub truncated: bool,
}

impl WireJson for PosBillListResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc_no").int(self.rvc_no.into());
        w.key("business_date").string(&self.business_date);
        w.key("bills");
        json::write_items(w, &self.bills);
        w.key("truncated").bool(self.truncated);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(PosBillListResult {
            rvc_no: o.int("rvc_no")?,
            business_date: o.string("business_date")?,
            bills: o.list("bills", json::item)?,
            truncated: o.bool("truncated")?,
        })
    }
}
