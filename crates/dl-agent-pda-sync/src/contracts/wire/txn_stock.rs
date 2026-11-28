//! Recipe stock depletion of a cloud-authoritative bill into SMILE_BO (`Wire/TxnStock.cs`,
//! ADR 0164, RT-1022): the verbs `txn.stock_deplete` and `txn.stock_restore`. One aggregate
//! per bill (`stock:{CheckNo}`), apart from the bill's snapshot and its room charge so the
//! versions never collide.
//!
//! The Rust agent runs these verbs since RT-1427 (`commands::stock`), with the count, receipt
//! and issue verbs that share [`txn_stock_reasons`].

use dl_decimal::Decimal;

use crate::contracts::json::WireJson;
use crate::contracts::wire::txn_commands::txn_scopes;
use crate::util::stj::{Obj, ReadError, Writer, days_from_civil};

/// Command types of the stock verbs (`TxnCommandTypes.StockDeplete` / `StockRestore`).
pub mod txn_stock_types {
    /// Full recipe depletion of one bill ([`TxnStockDepletePayload`](super::TxnStockDepletePayload)).
    pub const DEPLETE: &str = "txn.stock_deplete";
    /// Puts back what the bill's depletion took ([`TxnStockRestorePayload`](super::TxnStockRestorePayload)).
    pub const RESTORE: &str = "txn.stock_restore";
}

/// `TxnStockScope`: the stock aggregate of a bill.
pub mod txn_stock_scope {
    use super::txn_scopes;

    /// The scope name.
    pub const SCOPE: &str = "stock";
    /// Most lines one command carries.
    pub const MAX_LINES: usize = 500;

    /// The aggregate, e.g. `stock:123456`.
    pub fn aggregate(check_no: i64) -> String {
        txn_scopes::aggregate(SCOPE, check_no)
    }

    /// Queue aggregate key, e.g. `txn:stock:123456`.
    pub fn aggregate_key(check_no: i64) -> String {
        txn_scopes::aggregate_key(SCOPE, check_no)
    }
}

/// Rejection reasons of the stock verbs, identical in the .NET and Rust agents
/// (`TxnStockReasons`); nothing is written when one is returned.
pub mod txn_stock_reasons {
    /// The cloud store code has no `dbo.Pda_TxnStockMap` row, or its BO store / location is not in `DefRVCList`.
    pub const STORE_UNMAPPED: &str = "txn_stock_store_unmapped";
    /// The configured SMILE_BO database name is invalid or the database does not exist.
    pub const DATABASE_INVALID: &str = "txn_stock_database_invalid";
    /// SMILE_BO lacks a ledger object the post needs.
    pub const LEDGER_MISSING: &str = "txn_stock_ledger_missing";
    /// `GlobPara INVCostMethod` is not 0 (weighted average): FIFO sites are refused.
    pub const COST_METHOD_UNSUPPORTED: &str = "txn_stock_cost_method_unsupported";
    /// The voucher period is before `INVPeriod` / `GLPeriod`, or `INVPeriod` is unset.
    pub const PERIOD_CLOSED: &str = "txn_stock_period_closed";
    /// A line names an `ICode` SMILE_BO `ItemDef` does not have.
    pub const ITEM_UNKNOWN: &str = "txn_stock_item_unknown";
    /// No stock voucher type (`JType` of `VoucherType` 'SX'), or its next number does not fit `VNo`.
    pub const VOUCHER_TYPE_UNKNOWN: &str = "txn_stock_voucher_type_unknown";
    /// The site's `INVUpdateGLAvg` takes parameters the agent cannot bind.
    pub const POST_UNSUPPORTED: &str = "txn_stock_post_unsupported";
    /// `INVUpdateGLAvg` posted nothing: a `StockInterface` account is not a detail `GLCOA` account.
    pub const ACCOUNT_INVALID: &str = "txn_stock_account_invalid";
    /// The count date's month is not open in `INVPeriod` (ADR 0167).
    pub const COUNT_PERIOD_NOT_OPEN: &str = "txn_stock_count_period_not_open";
    /// The BO store has an open count session (`InvSession` not posted).
    pub const COUNT_SESSION_OPEN: &str = "txn_stock_count_session_open";
    /// The count session the applied key points at is another count's.
    pub const COUNT_SESSION_MISMATCH: &str = "txn_stock_count_session_mismatch";
    /// The count session the applied key points at is gone.
    pub const COUNT_SESSION_MISSING: &str = "txn_stock_count_session_missing";
    /// The vendor, the store or an item account the receipt post needs is not valid (ADR 0169).
    pub const RECEIPT_TARGET_INVALID: &str = "txn_stock_receipt_target_invalid";
    /// The receipt date's month is not open for the receiving post.
    pub const RECEIPT_PERIOD_NOT_OPEN: &str = "txn_stock_receipt_period_not_open";
    /// The receiving voucher the applied key points at is gone.
    pub const RECEIPT_MISSING: &str = "txn_stock_receipt_missing";
    /// The receiving voucher the applied key points at is another receipt's.
    pub const RECEIPT_MISMATCH: &str = "txn_stock_receipt_mismatch";
    /// The issue voucher the applied key points at is gone (ADR 0168).
    pub const ISSUE_VOUCHER_MISSING: &str = "txn_stock_issue_voucher_missing";
    /// The issue voucher the applied key points at is another issue's.
    pub const ISSUE_VOUCHER_MISMATCH: &str = "txn_stock_issue_voucher_mismatch";
    /// The same version came again with another payload.
    pub const ISSUE_PAYLOAD_MISMATCH: &str = "txn_stock_issue_payload_mismatch";
}

/// One item of a depletion (`TxnStockLine`): `item_code` (SMILE_BO `ICode`) taken out of
/// `store_code` (the cloud stock store) by `qty` in the item's stock unit. A JSON null line
/// reads as the default line and is refused by [`TxnStockDepletePayload::problem`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockLine {
    /// SMILE_BO `ICode`.
    pub item_code: String,
    /// Cloud stock store code.
    pub store_code: i32,
    /// Quantity in the item's stock unit.
    pub qty: Decimal,
}

impl WireJson for TxnStockLine {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("item_code").string(&self.item_code);
        w.key("store_code").int(i64::from(self.store_code));
        w.key("qty").decimal(&self.qty);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockLine {
            item_code: o.string("item_code")?,
            store_code: o.int("store_code")?,
            qty: o.decimal("qty")?,
        })
    }
}

/// UTF-16 length, the `string.Length` the .NET side checks.
pub(super) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `DateTime.TryParseExact(s, "yyyy-MM-dd", InvariantCulture)`.
pub(super) fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| {
        b[r.clone()]
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| s[r].parse::<u32>().ok())
            .flatten()
    };
    match (digits(0..4), digits(5..7), digits(8..10)) {
        (Some(y), Some(m), Some(d)) => days_from_civil(i64::from(y), m, d).is_some(),
        _ => false,
    }
}

fn rvc_check_version_problem(rvc: i32, check_no: i64, version: i64) -> Option<String> {
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

/// Payload of [`txn_stock_types::DEPLETE`] (`TxnStockDepletePayload`): the full recipe
/// depletion of bill `check_no`. `business_date` (yyyy-MM-dd) is the voucher date;
/// `version` is the queue's `aggregate_seq` (a version at or below the recorded one
/// changes nothing).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockDepletePayload {
    /// Outlet of the bill.
    pub rvc: i32,
    /// The bill.
    pub check_no: i64,
    /// Aggregate version.
    pub version: i64,
    /// Voucher date, yyyy-MM-dd.
    pub business_date: String,
    /// One line per item and store.
    pub lines: Vec<TxnStockLine>,
    /// Who closed the bill.
    pub actor: String,
}

impl TxnStockDepletePayload {
    /// Shape problems both sides check before any SQL runs; `None` when well-formed
    /// (`Problem`, same messages).
    pub fn problem(&self) -> Option<String> {
        if let Some(p) = rvc_check_version_problem(self.rvc, self.check_no, self.version) {
            return Some(p);
        }
        if !is_iso_date(&self.business_date) {
            return Some("BusinessDate must be yyyy-MM-dd.".into());
        }
        if self.lines.is_empty() {
            return Some("Lines must not be empty.".into());
        }
        if self.lines.len() > txn_stock_scope::MAX_LINES {
            return Some("Too many lines.".into());
        }
        let max_qty = Decimal::from(999_999_999_i64);
        let mut seen: Vec<(&str, i32)> = Vec::new();
        for l in &self.lines {
            let code = l.item_code.trim();
            if code.is_empty() || utf16_len(code) > 20 {
                return Some("ItemCode must be 1..20 characters.".into());
            }
            if !(1..=255).contains(&l.store_code) {
                return Some("StoreCode must be 1..255.".into());
            }
            if !l.qty.is_positive() || l.qty > max_qty || l.qty.round(4) != l.qty {
                return Some("Qty must be positive with at most 4 decimals.".into());
            }
            if seen.contains(&(code, l.store_code)) {
                return Some("A line is repeated for the same item and store.".into());
            }
            seen.push((code, l.store_code));
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnStockDepletePayload {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("rvc").int(i64::from(self.rvc));
        w.key("check_no").int(self.check_no);
        w.key("version").int(self.version);
        w.key("business_date").string(&self.business_date);
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
            None => Ok(TxnStockLine::default()),
            Some(l) => TxnStockLine::read_json(l),
        })?;
        Ok(TxnStockDepletePayload {
            rvc: o.int("rvc")?,
            check_no: o.int("check_no")?,
            version: o.int("version")?,
            business_date: o.string("business_date")?,
            lines,
            actor: o.string("actor")?,
        })
    }
}

/// Payload of [`txn_stock_types::RESTORE`] (`TxnStockRestorePayload`): puts back what the
/// bill's depletion took (a reopened or voided bill).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockRestorePayload {
    /// Outlet of the bill.
    pub rvc: i32,
    /// The bill.
    pub check_no: i64,
    /// Aggregate version.
    pub version: i64,
    /// Why the stock goes back.
    pub reason: String,
    /// Who reopened the bill.
    pub actor: String,
}

impl TxnStockRestorePayload {
    /// Longest reason, in UTF-16 units.
    pub const MAX_REASON_LENGTH: usize = 200;

    /// Shape problems both sides check before any SQL runs (`Problem`, same messages).
    pub fn problem(&self) -> Option<String> {
        if let Some(p) = rvc_check_version_problem(self.rvc, self.check_no, self.version) {
            return Some(p);
        }
        if self.reason.trim().is_empty() || utf16_len(&self.reason) > Self::MAX_REASON_LENGTH {
            return Some(format!(
                "Reason must be 1..{} characters.",
                Self::MAX_REASON_LENGTH
            ));
        }
        if self.actor.trim().is_empty() {
            return Some("Actor is required.".into());
        }
        None
    }
}

impl WireJson for TxnStockRestorePayload {
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
        Ok(TxnStockRestorePayload {
            rvc: o.int("rvc")?,
            check_no: o.int("check_no")?,
            version: o.int("version")?,
            reason: o.string("reason")?,
            actor: o.string("actor")?,
        })
    }
}

/// Result of an applied depletion or restore (`TxnStockResult`). `lines` / `net_qty` are
/// what the bill holds out of stock after the command (a restore leaves 0 and 0); `hash`
/// is `TxnAggregateHash.Of` over [`stock_hash_line`] of every held line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TxnStockResult {
    /// The aggregate, e.g. `stock:123456`.
    pub aggregate: String,
    /// Version of the command.
    pub version: i64,
    /// `applied` or `stale`.
    pub outcome: String,
    /// Version recorded after the command.
    pub applied_version: i64,
    /// Lines held.
    pub lines: i32,
    /// Sum of the held quantities.
    pub net_qty: Decimal,
    /// SHA-256 (lower hex) of the held lines.
    pub hash: String,
}

impl WireJson for TxnStockResult {
    fn write_json(&self, w: &mut Writer) {
        w.begin_object();
        w.key("aggregate").string(&self.aggregate);
        w.key("version").int(self.version);
        w.key("outcome").string(&self.outcome);
        w.key("applied_version").int(self.applied_version);
        w.key("lines").int(i64::from(self.lines));
        w.key("net_qty").decimal(&self.net_qty);
        w.key("hash").string(&self.hash);
        w.end_object();
    }

    fn read_json(o: Obj<'_>) -> Result<Self, ReadError> {
        Ok(TxnStockResult {
            aggregate: o.string("aggregate")?,
            version: o.int("version")?,
            outcome: o.string("outcome")?,
            applied_version: o.int("applied_version")?,
            lines: o.int("lines")?,
            net_qty: o.decimal("net_qty")?,
            hash: o.string("hash")?,
        })
    }
}

/// `value.ToString("0.#####")` with `scale` optional decimals: no trailing zeros, zero is `0`.
pub(super) fn trimmed_text(value: Decimal, scale: u32) -> String {
    let v = value.round(scale).normalize();
    if v.is_zero() {
        "0".to_string()
    } else {
        v.to_string()
    }
}

/// One hash line of a held stock line: `{ICode}|{store}|{TrnQty}\n`, where `TrnQty` is minus
/// the held quantity written as .NET `"0.####"` (no trailing zeros, at most 4 decimals).
pub fn stock_hash_line(item_code: &str, store_code: i32, held_qty: Decimal) -> String {
    let trn_qty = (-held_qty).round(4).normalize();
    let text = if trn_qty.is_zero() {
        "0".to_string()
    } else {
        trn_qty.to_string()
    };
    format!("{item_code}|{store_code}|{text}\n")
}
