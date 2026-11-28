//! Stock receipt of the cloud into SMILE_BO (`Wire/TxnStockReceipt.cs`, ADR 0169, RT-1113): the
//! verb `txn.stock_receipt`, posted by the vendor `RCVUpdateGL`. One aggregate per receipt
//! (`stockreceipt:{ReceiptNo}`); once applied, every later version answers `stale` (a
//! correction is a new `ReceiptNo`). The rejection reasons are the ones of
//! [`txn_stock_reasons`](super::txn_stock_reasons).

use dl_decimal::{Decimal, RoundingMode};

use crate::contracts::json::WireJson;
use crate::contracts::wire::txn_commands::txn_scopes;
use crate::contracts::wire::txn_stock::{is_iso_date, utf16_len};
use crate::util::stj::{Obj, ReadError, Writer};

/// Command type of the receipt verb (`TxnCommandTypes.StockReceipt`).
pub mod txn_stock_receipt_types {
    /// A goods receipt from a vendor ([`TxnStockReceiptPayload`](super::TxnStockReceiptPayload)).
    pub const STOCK_RECEIPT: &str = "txn.stock_receipt";
}

/// `TxnStockReceiptScope`: the aggregate of a receipt.
pub mod txn_stock_receipt_scope {
    use super::txn_scopes;

    /// The scope name.
    pub const SCOPE: &str = "stockreceipt";
    /// Most lines one command carries.
    pub const MAX_LINES: usize = 500;

    /// The aggregate, e.g. `stockreceipt:12`.
    pub fn aggregate(receipt_no: i64) -> String {
        txn_scopes::aggregate(SCOPE, receipt_no)
    }

    /// Queue aggregate key, e.g. `txn:stockreceipt:12`.
    pub fn aggregate_key(receipt_no: i64) -> String {
        txn_scopes::aggregate_key(SCOPE, receipt_no)
    }
}

/// One received item (`TxnStockReceiptLine`): `qty` of `item_code` at `unit_price`. A JSON
/// null line reads as the default line and is refused by [`TxnStockReceiptPayload::problem`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockReceiptLine {
    /// SMILE_BO `ICode`.
    pub item_code: String,
    /// Quantity in the item's stock unit.
    pub qty: Decimal,
    /// Price of one unit.
    pub unit_price: Decimal,
}

impl WireJson for TxnStockReceiptLine {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("item_code").string(&self.item_code);
        w.key("qty").decimal(&self.qty);
        w.key("unit_price").decimal(&self.unit_price);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockReceiptLine {
            item_code: o.string("item_code")?,
            qty: o.decimal("qty")?,
            unit_price: o.decimal("unit_price")?,
        })
    }
}

/// Payload of [`txn_stock_receipt_types::STOCK_RECEIPT`] (`TxnStockReceiptPayload`): receipt
/// `receipt_no` into cloud store `store_code` from vendor `vendor_code` (SMILE_BO `CLIENT`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockReceiptPayload {
    /// Outlet that received.
    pub rvc: i32,
    /// Cloud receipt number.
    pub receipt_no: i64,
    /// The queue's `aggregate_seq`.
    pub version: i64,
    /// Voucher date, yyyy-MM-dd.
    pub business_date: String,
    /// Cloud stock store code.
    pub store_code: i32,
    /// SMILE_BO `CLIENT` code of the vendor.
    pub vendor_code: i64,
    /// Received items.
    pub lines: Vec<TxnStockReceiptLine>,
    /// Who received.
    pub actor: String,
}

impl TxnStockReceiptPayload {
    /// `MaxReceiptNo`.
    pub const MAX_RECEIPT_NO: i64 = 9_999_999_999_999_999;

    /// `MaxUnitPrice`.
    pub fn max_unit_price() -> Decimal {
        Decimal::from(999_999_999_i64)
    }

    /// `Problem()`: the first thing wrong with the payload, same text and order as .NET.
    pub fn problem(&self) -> Option<String> {
        if !(1..=255).contains(&self.rvc) {
            return Some("Rvc must be 1..255.".into());
        }
        if self.receipt_no <= 0 || self.receipt_no > Self::MAX_RECEIPT_NO {
            return Some(format!("ReceiptNo must be 1..{}.", Self::MAX_RECEIPT_NO));
        }
        if self.version <= 0 {
            return Some("Version must be positive.".into());
        }
        if !is_iso_date(&self.business_date) {
            return Some("BusinessDate must be yyyy-MM-dd.".into());
        }
        if !(1..=255).contains(&self.store_code) {
            return Some("StoreCode must be 1..255.".into());
        }
        if self.vendor_code <= 0 {
            return Some("VendorCode must be positive.".into());
        }
        if self.lines.is_empty() {
            return Some("Lines must not be empty.".into());
        }
        if self.lines.len() > txn_stock_receipt_scope::MAX_LINES {
            return Some("Too many lines.".into());
        }
        let max_qty = Decimal::from(999_999_999_i64);
        let max_amount = Decimal::from(9_999_999_999_999_i64);
        let mut seen: Vec<&str> = Vec::new();
        for l in &self.lines {
            let code = l.item_code.trim();
            if code.is_empty() || utf16_len(code) > 20 {
                return Some("ItemCode must be 1..20 characters.".into());
            }
            if !l.qty.is_positive() || l.qty > max_qty || l.qty.round(4) != l.qty {
                return Some("Qty must be positive with at most 4 decimals.".into());
            }
            let p = l.unit_price;
            if p.is_negative() || p > Self::max_unit_price() || p.round(4) != p {
                return Some("UnitPrice must not be negative, with at most 4 decimals.".into());
            }
            if (l.qty * p).round_dp(2, RoundingMode::HalfEven) > max_amount {
                return Some("A line amount is too large.".into());
            }
            if seen.contains(&code) {
                return Some("An item is received twice.".into());
            }
            seen.push(code);
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnStockReceiptPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(i64::from(self.rvc));
        w.key("receipt_no").int(self.receipt_no);
        w.key("version").int(self.version);
        w.key("business_date").string(&self.business_date);
        w.key("store_code").int(i64::from(self.store_code));
        w.key("vendor_code").int(self.vendor_code);
        w.key("lines").begin_array();
        for l in &self.lines {
            l.write_json(w);
        }
        w.end_array();
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let lines = o.list("lines", |v, opts, path| match Obj::of(v, opts, path)? {
            None => Ok(TxnStockReceiptLine::default()),
            Some(l) => TxnStockReceiptLine::read_json(l),
        })?;
        Ok(TxnStockReceiptPayload {
            rvc: o.int("rvc")?,
            receipt_no: o.int("receipt_no")?,
            version: o.int("version")?,
            business_date: o.string("business_date")?,
            store_code: o.int("store_code")?,
            vendor_code: o.int("vendor_code")?,
            lines,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied receipt (`TxnStockReceiptResult`): `receipt_seq` is the
/// `ReceivingHeader` sequence the post wrote, `voucher_no` its `PNK` number.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockReceiptResult {
    /// The aggregate, e.g. `stockreceipt:12`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// `applied` or `stale`.
    pub outcome: String,
    /// Version recorded after the command.
    pub applied_version: i64,
    /// Receiving sequence of the posted voucher.
    pub receipt_seq: i64,
    /// The receiving voucher number.
    pub voucher_no: String,
    /// Lines received.
    pub lines: i32,
    /// Sum of the received quantities.
    pub net_qty: Decimal,
    /// Sum of the line amounts.
    pub amount: Decimal,
    /// SHA-256 (lower hex) of the received lines.
    pub hash: String,
}

impl WireJson for TxnStockReceiptResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("aggregate").string(&self.aggregate);
        w.key("version").int(self.version);
        w.key("outcome").string(&self.outcome);
        w.key("applied_version").int(self.applied_version);
        w.key("receipt_seq").int(self.receipt_seq);
        w.key("voucher_no").string(&self.voucher_no);
        w.key("lines").int(i64::from(self.lines));
        w.key("net_qty").decimal(&self.net_qty);
        w.key("amount").decimal(&self.amount);
        w.key("hash").string(&self.hash);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockReceiptResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            receipt_seq: o.int("receipt_seq")?,
            voucher_no: o.string("voucher_no")?,
            lines: o.int("lines")?,
            net_qty: o.decimal("net_qty")?,
            amount: o.decimal("amount")?,
            hash: o.string("hash")?,
        })
    }
}

/// One hash line of a received line: `{ICode}|{StoreCode}|{Qty}|{Amount}\n`, both numbers
/// written as .NET `"0.#####"` (no trailing zeros, at most 5 decimals).
pub fn stock_receipt_hash_line(
    item_code: &str,
    store_code: i32,
    qty: Decimal,
    amount: Decimal,
) -> String {
    use crate::contracts::wire::txn_stock::trimmed_text;
    format!(
        "{item_code}|{store_code}|{}|{}\n",
        trimmed_text(qty, 5),
        trimmed_text(amount, 5)
    )
}
