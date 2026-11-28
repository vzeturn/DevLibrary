//! Stock count of the cloud into SMILE_BO (`Wire/TxnStock.cs`, ADR 0167, RT-1114): the verb
//! `txn.stock_count`. One aggregate per count (`stockcount:{CountNo}`), apart from the bill
//! stock aggregate `stock:{CheckNo}` and the issue aggregate `stockissue:{IssueNo}`. A count is a
//! one-shot document: once applied, every later version answers `stale` (a recount is a new
//! `CountNo`). The rejection reasons are the ones of
//! [`txn_stock_reasons`](super::txn_stock_reasons).

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::txn_commands::txn_scopes;
use crate::contracts::wire::txn_stock::{is_iso_date, utf16_len};
use crate::util::stj::{Obj, ReadError, Writer};

/// Command type of the count verb (`TxnCommandTypes.StockCount`).
pub mod txn_stock_count_types {
    /// A physical count of one cloud store ([`TxnStockCountPayload`](super::TxnStockCountPayload)).
    pub const STOCK_COUNT: &str = "txn.stock_count";
}

/// `TxnStockCountScope`: the aggregate of a count.
pub mod txn_stock_count_scope {
    use super::txn_scopes;

    /// The scope name.
    pub const SCOPE: &str = "stockcount";
    /// Most lines one command carries.
    pub const MAX_LINES: usize = 500;

    /// The aggregate, e.g. `stockcount:12`.
    pub fn aggregate(count_no: i64) -> String {
        txn_scopes::aggregate(SCOPE, count_no)
    }

    /// Queue aggregate key, e.g. `txn:stockcount:12`.
    pub fn aggregate_key(count_no: i64) -> String {
        txn_scopes::aggregate_key(SCOPE, count_no)
    }
}

/// One counted item (`TxnStockCountLine`): `item_code` (SMILE_BO `ICode`) counted at
/// `counted_qty` in its stock unit. A JSON null line reads as the default line and is refused
/// by [`TxnStockCountPayload::problem`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockCountLine {
    /// SMILE_BO `ICode`.
    pub item_code: String,
    /// Counted quantity in the item's stock unit (0 is valid).
    pub counted_qty: Decimal,
}

impl WireJson for TxnStockCountLine {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("item_code").string(&self.item_code);
        w.key("counted_qty").decimal(&self.counted_qty);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockCountLine {
            item_code: o.string("item_code")?,
            counted_qty: o.decimal("counted_qty")?,
        })
    }
}

/// Payload of [`txn_stock_count_types::STOCK_COUNT`] (`TxnStockCountPayload`): count
/// `count_no` of cloud store `store_code` on `business_date` (yyyy-MM-dd).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockCountPayload {
    /// Outlet that counted.
    pub rvc: i32,
    /// Cloud count number.
    pub count_no: i64,
    /// The queue's `aggregate_seq`.
    pub version: i64,
    /// Voucher date, yyyy-MM-dd.
    pub business_date: String,
    /// Cloud stock store code.
    pub store_code: i32,
    /// Counted items.
    pub lines: Vec<TxnStockCountLine>,
    /// Who counted.
    pub actor: String,
}

impl TxnStockCountPayload {
    /// `Problem()`: the first thing wrong with the payload, same text and order as .NET.
    pub fn problem(&self) -> Option<String> {
        if !(1..=255).contains(&self.rvc) {
            return Some("Rvc must be 1..255.".into());
        }
        if self.count_no <= 0 {
            return Some("CountNo must be positive.".into());
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
        if self.lines.is_empty() {
            return Some("Lines must not be empty.".into());
        }
        if self.lines.len() > txn_stock_count_scope::MAX_LINES {
            return Some("Too many lines.".into());
        }
        let max_qty = Decimal::from(999_999_999_i64);
        let mut seen: Vec<&str> = Vec::new();
        for l in &self.lines {
            let code = l.item_code.trim();
            if code.is_empty() || utf16_len(code) > 20 {
                return Some("ItemCode must be 1..20 characters.".into());
            }
            let q = l.counted_qty;
            if q.is_negative() || q > max_qty || q.round(4) != q {
                return Some("CountedQty must not be negative, with at most 4 decimals.".into());
            }
            if seen.contains(&code) {
                return Some("An item is counted twice.".into());
            }
            seen.push(code);
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnStockCountPayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(i64::from(self.rvc));
        w.key("count_no").int(self.count_no);
        w.key("version").int(self.version);
        w.key("business_date").string(&self.business_date);
        w.key("store_code").int(i64::from(self.store_code));
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
            None => Ok(TxnStockCountLine::default()),
            Some(l) => TxnStockCountLine::read_json(l),
        })?;
        Ok(TxnStockCountPayload {
            rvc: o.int("rvc")?,
            count_no: o.int("count_no")?,
            version: o.int("version")?,
            business_date: o.string("business_date")?,
            store_code: o.int("store_code")?,
            lines,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied count (`TxnStockCountResult`): `session_seq` is the
/// `InvSession.SSeq` written, `voucher_no` the adjustment voucher (absent when every line
/// equals the system quantity), `differences` the lines booked and `net_qty` their sum.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockCountResult {
    /// The aggregate, e.g. `stockcount:12`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// `applied` or `stale`.
    pub outcome: String,
    /// Version recorded after the command.
    pub applied_version: i64,
    /// `InvSession.SSeq` of the count.
    pub session_seq: i64,
    /// The adjustment voucher; `None` when nothing differed.
    pub voucher_no: Option<String>,
    /// Lines counted.
    pub lines: i32,
    /// Lines whose count differed from the system quantity.
    pub differences: i32,
    /// Sum of the booked differences.
    pub net_qty: Decimal,
    /// SHA-256 (lower hex) of the counted lines.
    pub hash: String,
}

impl WireJson for TxnStockCountResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("aggregate").string(&self.aggregate);
        w.key("version").int(self.version);
        w.key("outcome").string(&self.outcome);
        w.key("applied_version").int(self.applied_version);
        w.key("session_seq").int(self.session_seq);
        w.opt_str_member("voucher_no", self.voucher_no.as_deref());
        w.key("lines").int(i64::from(self.lines));
        w.key("differences").int(i64::from(self.differences));
        w.key("net_qty").decimal(&self.net_qty);
        w.key("hash").string(&self.hash);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockCountResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            session_seq: o.int("session_seq")?,
            voucher_no: o.opt_string("voucher_no")?,
            lines: o.int("lines")?,
            differences: o.int("differences")?,
            net_qty: o.decimal("net_qty")?,
            hash: o.string("hash")?,
        })
    }
}

/// One hash line of a counted difference: `{ICode}|{StoreCode}|{TrnQty}\n`, where `TrnQty` is
/// counted minus system written as .NET `"0.#####"` (no trailing zeros, at most 5 decimals),
/// so the cloud can recompute the hash from the system quantities it mirrors.
pub fn stock_count_hash_line(item_code: &str, store_code: i32, trn_qty: Decimal) -> String {
    format!(
        "{item_code}|{store_code}|{}\n",
        crate::contracts::wire::txn_stock::trimmed_text(trn_qty, 5)
    )
}
