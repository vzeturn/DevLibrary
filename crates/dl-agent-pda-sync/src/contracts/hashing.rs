//! Canonical values and row hashing (`Hashing/CanonicalValue.cs`, `RowHasher.cs`,
//! `RowKey.cs`).

use dl_decimal::Decimal;
use dl_hash::{Digest, Sha256};

use super::map::StrMap;
use super::time::{DateTime, TICKS_PER_SECOND, civil_from_ticks};

/// Ordinal (`StringComparer.Ordinal`) comparison: UTF-16 code unit order.
pub fn ordinal_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Canonical invariant string encodings the row hash is computed over.
pub mod canonical_value {
    use super::*;

    /// Invariant decimal string, trailing zeros trimmed, no exponent, "-0" as "0"
    /// (`value / 1.0000000000000000000000000000m`).
    pub fn decimal_string(value: Decimal) -> String {
        let n = value.normalize();
        if n.is_zero() {
            "0".to_owned()
        } else {
            n.to_string()
        }
    }

    /// `decimal.TryParse(text, NumberStyles.Float, InvariantCulture)` canonicalised;
    /// `None` on unparseable input or overflow.
    pub fn decimal_or_null(text: Option<&str>) -> Option<String> {
        parse_float_style(text?).map(decimal_string)
    }

    /// `NumberStyles.Float`: surrounding white space (U+0009..U+000D, U+0020), a leading
    /// sign, a decimal point and an exponent; no thousands separators.
    pub fn parse_float_style(text: &str) -> Option<Decimal> {
        let t = text.trim_matches(|c: char| c == ' ' || ('\u{9}'..='\u{d}').contains(&c));
        if t.is_empty() {
            return None;
        }
        crate::util::stj::parse_dotnet_decimal(t)
    }

    /// UTC instant as fixed-width ISO 8601 with 7 fraction digits and `Z`; the kind is
    /// ignored (a non-UTC value is taken as UTC, like `SpecifyKind`).
    pub fn utc(t: DateTime) -> String {
        utc_ticks(t.ticks())
    }

    /// [`utc`] of a tick count.
    pub fn utc_ticks(ticks: i64) -> String {
        let (y, mo, d, h, mi, s, f) = civil_from_ticks(ticks);
        debug_assert!(f < TICKS_PER_SECOND);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{f:07}Z")
    }

    /// `"true"` / `"false"`.
    pub fn bool_string(value: bool) -> &'static str {
        if value { "true" } else { "false" }
    }

    /// Standard Base64 with padding.
    pub fn base64(bytes: &[u8]) -> String {
        dl_encoding::base64::STANDARD.encode(bytes)
    }
}

/// Canonical row hash: SHA-256 over the columns sorted by ordinal name, each as
/// `u32be len(name) | name | u8 tag (0 = NULL, 1 = value) | [u32be len(value) | value]`,
/// lower-case hex.
pub mod row_hasher {
    use super::*;

    /// The hash of a column map (the map's order does not matter).
    pub fn hash(columns: &StrMap) -> String {
        hash_pairs(columns.iter().map(|(k, v)| (k, v.as_deref())))
    }

    /// The hash of `(name, value)` pairs (names must be distinct).
    pub fn hash_pairs<'a>(columns: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> String {
        let mut cols: Vec<(&str, Option<&str>)> = columns.into_iter().collect();
        cols.sort_by(|a, b| ordinal_cmp(a.0, b.0));
        let mut sha = Sha256::default();
        let append = |sha: &mut Sha256, s: &str| {
            sha.update(&(s.len() as u32).to_be_bytes());
            sha.update(s.as_bytes());
        };
        for (name, value) in cols {
            append(&mut sha, name);
            match value {
                None => sha.update(&[0]),
                Some(v) => {
                    sha.update(&[1]);
                    append(&mut sha, v);
                }
            }
        }
        dl_encoding::hex::encode(sha.finalize())
    }
}

/// Canonical string form of a row key (`sync.row_state.row_key`, manifest buckets).
pub mod row_key {
    use super::*;

    /// The ASCII unit separator between pairs.
    pub const SEPARATOR: char = '\u{1f}';

    /// `name=value` pairs sorted by ordinal name joined with U+001F; a null value is the
    /// bare name.
    pub fn of(key: &StrMap) -> String {
        let mut pairs: Vec<(&str, Option<&str>)> =
            key.iter().map(|(k, v)| (k, v.as_deref())).collect();
        pairs.sort_by(|a, b| ordinal_cmp(a.0, b.0));
        let mut out = String::new();
        for (i, (k, v)) in pairs.into_iter().enumerate() {
            if i > 0 {
                out.push(SEPARATOR);
            }
            out.push_str(k);
            if let Some(v) = v {
                out.push('=');
                out.push_str(v);
            }
        }
        out
    }

    /// The first byte of SHA-256 over the key, modulo `buckets` (default 256).
    pub fn bucket(row_key: &str, buckets: u32) -> u32 {
        let first = Sha256::digest(row_key.as_bytes()).as_ref()[0];
        u32::from(first) % buckets.max(1)
    }
}
