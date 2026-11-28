//! Stock issue voucher of the cloud into SMILE_BO (`Wire/TxnStockIssue.cs`, ADR 0168, RT-1112):
//! the verb `txn.stock_issue`, ITrnCode 200 / ITrnSub 50. One aggregate per voucher
//! (`stockissue:{IssueNo}`), apart from the bill stock aggregate `stock:{CheckNo}` and the count
//! aggregate `stockcount:{CountNo}`.
//!
//! The Rust agent runs this verb since RT-1427 (`commands::stock_issue`). The rejection reasons
//! are the ones of [`txn_stock_reasons`](super::txn_stock_reasons).

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::txn_commands::txn_scopes;
use crate::contracts::wire::txn_stock::{is_iso_date, utf16_len};
use crate::util::stj::{Obj, ReadError, Writer};

/// Command type of the issue verb (`TxnCommandTypes.StockIssue`).
pub mod txn_stock_issue_types {
    /// Books a stock issue voucher ([`TxnStockIssuePayload`](super::TxnStockIssuePayload)).
    pub const STOCK_ISSUE: &str = "txn.stock_issue";
}

/// `TxnStockIssueScope`: the stock issue aggregate of a voucher.
pub mod txn_stock_issue_scope {
    use super::txn_scopes;

    /// The scope name.
    pub const SCOPE: &str = "stockissue";
    /// Most lines one voucher carries.
    pub const MAX_LINES: usize = 500;

    /// The aggregate, e.g. `stockissue:12`.
    pub fn aggregate(issue_no: i64) -> String {
        txn_scopes::aggregate(SCOPE, issue_no)
    }

    /// Queue aggregate key, e.g. `txn:stockissue:12`.
    pub fn aggregate_key(issue_no: i64) -> String {
        txn_scopes::aggregate_key(SCOPE, issue_no)
    }
}

/// One issued item (`TxnStockIssueLine`): `qty` (positive) of `item_code` (SMILE_BO `ICode`) in
/// the item's stock unit. A JSON null line reads as the default line and is refused by
/// [`TxnStockIssuePayload::problem`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockIssueLine {
    /// SMILE_BO `ICode`.
    pub item_code: String,
    /// Quantity in the item's stock unit.
    pub qty: Decimal,
}

impl WireJson for TxnStockIssueLine {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("item_code").string(&self.item_code);
        w.key("qty").decimal(&self.qty);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockIssueLine {
            item_code: o.string("item_code")?,
            qty: o.decimal("qty")?,
        })
    }
}

/// Payload of [`txn_stock_issue_types::STOCK_ISSUE`] (`TxnStockIssuePayload`): cloud voucher
/// `issue_no` (also the idempotency key) issues `lines` out of cloud stock store `store_code` on
/// `business_date` (yyyy-MM-dd). `with_store_code` is the destination: absent or equal to
/// `store_code` is the store's consumption location, another code a store-to-store transfer.
/// `version` is the queue's `aggregate_seq`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockIssuePayload {
    /// Outlet of the voucher.
    pub rvc: i32,
    /// The voucher.
    pub issue_no: i64,
    /// Aggregate version.
    pub version: i64,
    /// Voucher date, yyyy-MM-dd.
    pub business_date: String,
    /// Cloud stock store the lines leave.
    pub store_code: i32,
    /// Destination store; absent = the consumption location of `store_code`.
    pub with_store_code: Option<i32>,
    /// One line per item.
    pub lines: Vec<TxnStockIssueLine>,
    /// Optional note kept on the voucher.
    pub reason: Option<String>,
    /// Who issued it.
    pub actor: String,
}

impl TxnStockIssuePayload {
    /// Longest reason (trimmed), in UTF-16 units: it is appended to the voucher note.
    pub const MAX_REASON_LENGTH: usize = 150;
    /// Largest `issue_no`: the voucher reference must fit `INVVoucher.VRef`.
    pub const MAX_ISSUE_NO: i64 = 999_999_999_999_999_999;

    /// True when the destination is the store's own consumption location (`ToConsumption`).
    pub fn to_consumption(&self) -> bool {
        self.with_store_code.is_none_or(|w| w == self.store_code)
    }

    /// Shape problems both sides check before any SQL runs; `None` when well-formed
    /// (`Problem`, same messages and order).
    pub fn problem(&self) -> Option<String> {
        if !(1..=255).contains(&self.rvc) {
            return Some("Rvc must be 1..255.".into());
        }
        if self.issue_no <= 0 || self.issue_no > Self::MAX_ISSUE_NO {
            return Some(format!("IssueNo must be 1..{}.", Self::MAX_ISSUE_NO));
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
        if self
            .with_store_code
            .is_some_and(|w| !(1..=255).contains(&w))
        {
            return Some("WithStoreCode must be 1..255 when given.".into());
        }
        if self.lines.is_empty() {
            return Some("Lines must not be empty.".into());
        }
        if self.lines.len() > txn_stock_issue_scope::MAX_LINES {
            return Some("Too many lines.".into());
        }
        let max_qty = Decimal::from(999_999_999_i64);
        let mut seen: Vec<&str> = Vec::new();
        for l in &self.lines {
            let code = l.item_code.trim();
            if code.is_empty() || utf16_len(code) > 20 {
                return Some("ItemCode must be 1..20 characters.".into());
            }
            if !l.qty.is_positive() || l.qty > max_qty || l.qty.round(4) != l.qty {
                return Some("Qty must be positive with at most 4 decimals.".into());
            }
            if seen.contains(&code) {
                return Some("An item is issued twice.".into());
            }
            seen.push(code);
        }
        if self
            .reason
            .as_deref()
            .is_some_and(|r| utf16_len(r.trim()) > Self::MAX_REASON_LENGTH)
        {
            return Some(format!(
                "Reason must be at most {} characters.",
                Self::MAX_REASON_LENGTH
            ));
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnStockIssuePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(i64::from(self.rvc));
        w.key("issue_no").int(self.issue_no);
        w.key("version").int(self.version);
        w.key("business_date").string(&self.business_date);
        w.key("store_code").int(i64::from(self.store_code));
        w.opt_int_member("with_store_code", self.with_store_code.map(i64::from));
        w.key("lines").begin_array();
        for l in &self.lines {
            l.write_json(w);
        }
        w.end_array();
        w.opt_str_member("reason", self.reason.as_deref());
        w.key("actor").string(&self.actor);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        let lines = o.list("lines", |v, opts, path| match Obj::of(v, opts, path)? {
            None => Ok(TxnStockIssueLine::default()),
            Some(l) => TxnStockIssueLine::read_json(l),
        })?;
        Ok(TxnStockIssuePayload {
            rvc: o.int("rvc")?,
            issue_no: o.int("issue_no")?,
            version: o.int("version")?,
            business_date: o.string("business_date")?,
            store_code: o.int("store_code")?,
            with_store_code: o.opt_int("with_store_code")?,
            lines,
            reason: o.opt_string("reason")?,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied or stale voucher (`TxnStockIssueResult`). `voucher_no` is the `VNo` of
/// the 200 / 50 voucher, `lines` its lines, `net_qty` the sum of their `TrnQty` (negative: stock
/// out), `hash` the SHA-256 (lower hex) over [`stock_issue_hash_line`] of every line, sorted
/// ordinally.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockIssueResult {
    /// The aggregate, e.g. `stockissue:12`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// `applied` or `stale`.
    pub outcome: String,
    /// Version that wrote the voucher.
    pub applied_version: i64,
    /// `VNo` of the voucher.
    pub voucher_no: String,
    /// Lines of the voucher.
    pub lines: i32,
    /// Sum of the `TrnQty` of the lines.
    pub net_qty: Decimal,
    /// SHA-256 (lower hex) of the lines.
    pub hash: String,
}

impl WireJson for TxnStockIssueResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("aggregate").string(&self.aggregate);
        w.key("version").int(self.version);
        w.key("outcome").string(&self.outcome);
        w.key("applied_version").int(self.applied_version);
        w.key("voucher_no").string(&self.voucher_no);
        w.key("lines").int(i64::from(self.lines));
        w.key("net_qty").decimal(&self.net_qty);
        w.key("hash").string(&self.hash);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockIssueResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            voucher_no: o.string("voucher_no")?,
            lines: o.int("lines")?,
            net_qty: o.decimal("net_qty")?,
            hash: o.string("hash")?,
        })
    }
}

/// One hash line of an issued line: `{ICode}|{StoreCode}|{WithStoreCode or StoreCode}|{TrnQty}\n`,
/// where `TrnQty` is minus the issued quantity written as .NET `"0.#####"` (no trailing zeros, at
/// most 5 decimals), so the cloud can recompute the hash from what it sent.
pub fn stock_issue_hash_line(
    item_code: &str,
    store_code: i32,
    with_store_code: Option<i32>,
    issued_qty: Decimal,
) -> String {
    let trn_qty = (-issued_qty).round(5).normalize();
    let text = if trn_qty.is_zero() {
        "0".to_string()
    } else {
        trn_qty.to_string()
    };
    let with = with_store_code.unwrap_or(store_code);
    format!("{item_code}|{store_code}|{with}|{text}\n")
}
