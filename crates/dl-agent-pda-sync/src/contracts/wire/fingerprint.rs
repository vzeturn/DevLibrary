//! Expected check state (`Wire/CheckStateFingerprint.cs`, ADR 0100 §4).

use dl_decimal::{Decimal, RoundingMode};
use dl_hash::{Digest, Sha256};

/// The open-check header columns the fingerprint covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckStateHeader {
    /// Check number.
    pub check_no: i64,
    /// Table.
    pub table_no: Option<String>,
    /// Guests.
    pub guests: i32,
    /// Sub total.
    pub sub_total: Decimal,
    /// Paid so far.
    pub total_paid: Decimal,
}

/// One line the fingerprint covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckStateLine {
    /// `TrnSeq` / `line_seq`.
    pub line_seq: i64,
    /// `TrnCode` / `transaction_code`, invariant text.
    pub transaction_code: Option<String>,
    /// Item code.
    pub item_code: Option<String>,
    /// Quantity.
    pub qty: Decimal,
    /// Price.
    pub price: Decimal,
}

/// Expected check state of an order command: a stable hash of the header and lines.
pub struct CheckStateFingerprint;

impl CheckStateFingerprint {
    /// Prefix of the current algorithm; a different prefix is a contract change.
    pub const PREFIX: &'static str = "cs1:";

    /// The fingerprint: lines ordered by sequence (stable), decimals normalised
    /// (50000.0000 equals 50000), `cs1:` + 32 lowercase hex chars.
    pub fn compute<'a>(
        header: &CheckStateHeader,
        lines: impl IntoIterator<Item = &'a CheckStateLine>,
    ) -> String {
        let mut text = String::with_capacity(256);
        text.push_str(&header.check_no.to_string());
        text.push('|');
        text.push_str(&clean(header.table_no.as_deref()));
        text.push('|');
        text.push_str(&header.guests.to_string());
        text.push('|');
        text.push_str(&number(header.sub_total));
        text.push('|');
        text.push_str(&number(header.total_paid));
        let mut lines: Vec<&CheckStateLine> = lines.into_iter().collect();
        lines.sort_by_key(|l| l.line_seq);
        for line in lines {
            text.push('\n');
            text.push_str(&line.line_seq.to_string());
            text.push('|');
            text.push_str(&clean(line.transaction_code.as_deref()));
            text.push('|');
            text.push_str(&clean(line.item_code.as_deref()));
            text.push('|');
            text.push_str(&number(line.qty));
            text.push('|');
            text.push_str(&number(line.price));
        }
        let hash = Sha256::digest(text.as_bytes());
        format!(
            "{}{}",
            Self::PREFIX,
            dl_encoding::hex::encode(&hash.as_ref()[..16])
        )
    }

    /// Ordinal comparison; a fingerprint of another algorithm never matches.
    pub fn matches(expected: Option<&str>, actual: Option<&str>) -> bool {
        matches!((expected, actual), (Some(a), Some(b)) if a == b)
    }
}

/// Fixed-width CHAR columns come back space-padded from SQL Server and trimmed from the
/// mirror.
fn clean(value: Option<&str>) -> String {
    value.unwrap_or("").trim().replace('|', "/")
}

/// `decimal.ToString("0.############", InvariantCulture)`.
pub(crate) fn number(value: Decimal) -> String {
    let r = value.round_dp(12, RoundingMode::HalfUp).normalize();
    if r.is_zero() {
        "0".to_owned()
    } else {
        r.to_string()
    }
}

/// Aggregate keys of check commands (`check:{rvc}:{check}`).
pub mod check_aggregate {
    /// `check:{rvc}:{check}`.
    pub fn key(rvc_no: i32, check_no: i64) -> String {
        format!("check:{rvc_no}:{check_no}")
    }

    /// Parses a key; both numbers must be positive and plain digits.
    pub fn try_parse(key: Option<&str>) -> Option<(i32, i64)> {
        let parts: Vec<&str> = key.unwrap_or("").split(':').collect();
        if parts.len() != 3 || parts[0] != "check" {
            return None;
        }
        let rvc = digits::<i32>(parts[1]).filter(|r| *r > 0)?;
        let check = digits::<i64>(parts[2]).filter(|c| *c > 0)?;
        Some((rvc, check))
    }

    /// `NumberStyles.None`: digits only.
    fn digits<T: std::str::FromStr>(s: &str) -> Option<T> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
}
