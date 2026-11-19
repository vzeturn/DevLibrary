//! `CanonicalHasher`: content hashes the cloud compares with the agent's to decide
//! "not modified" and to check a merged delta. Every byte must match .NET.

use dl_json::{Number, Value};

use super::row::PmsRow;
use crate::{crypto, netdec, stj};

/// ASCII unit separator between fields.
pub const SEP: char = '\u{1f}';

/// Hash of one row: columns ordered ignoring case, `UPPER(name)=canonical` each.
pub fn hash_row(row: &PmsRow) -> String {
    let mut cols: Vec<(&str, &Value)> = row.iter().collect();
    cols.sort_by(|a, b| stj::ci_cmp(a.0, b.0));
    let mut sb = String::new();
    for (k, v) in cols {
        sb.push_str(&stj::upper(k));
        sb.push('=');
        append_canonical(&mut sb, v);
        sb.push(SEP);
    }
    crypto::sha256_hex(sb.as_bytes())
}

/// Hash of a result set: the row hashes in order.
pub fn hash_rows<'a>(rows: impl IntoIterator<Item = &'a PmsRow>) -> String {
    let mut sb = String::new();
    for row in rows {
        sb.push_str(&hash_row(row));
        sb.push(SEP);
    }
    crypto::sha256_hex(sb.as_bytes())
}

/// Hash of request parameters: names ordered ignoring case, null as `"\0null"`.
pub fn hash_parameters<'a>(params: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> String {
    let mut list: Vec<(&str, Option<&str>)> = params.into_iter().collect();
    list.sort_by(|a, b| stj::ci_cmp(a.0, b.0));
    let mut sb = String::new();
    for (k, v) in list {
        sb.push_str(&stj::upper(k));
        sb.push('=');
        sb.push_str(v.unwrap_or("\0null"));
        sb.push(SEP);
    }
    crypto::sha256_hex(sb.as_bytes())
}

/// ETag of a response: the set hashes joined by the separator.
pub fn combine_etag<'a>(hashes: impl IntoIterator<Item = &'a str>) -> String {
    let mut sb = String::new();
    for (i, h) in hashes.into_iter().enumerate() {
        if i > 0 {
            sb.push(SEP);
        }
        sb.push_str(h);
    }
    crypto::sha256_hex(sb.as_bytes())
}

/// Delta key of a row: the canonical key column values joined by the separator (a
/// missing column contributes nothing).
pub fn row_key(row: &PmsRow, key_columns: &[String]) -> String {
    let mut sb = String::new();
    for (i, k) in key_columns.iter().enumerate() {
        if i > 0 {
            sb.push(SEP);
        }
        if let Some(v) = row.get(k) {
            append_canonical(&mut sb, v);
        }
    }
    sb
}

fn number_text(n: &Number) -> String {
    let mut s = String::new();
    stj::write_number(&mut s, n);
    s
}

/// Canonical text of a value: numbers through `decimal` (`G29`) when they fit, object
/// properties sorted ordinally, strings re-serialised.
pub fn append_canonical(sb: &mut String, v: &Value) {
    match v {
        Value::Null => sb.push_str("null"),
        Value::Number(n) => {
            let raw = number_text(n);
            match netdec::parse_json(&raw) {
                Some(d) => sb.push_str(&d.g29()),
                None => sb.push_str(&raw),
            }
        }
        Value::Object(map) => {
            let mut props: Vec<(&String, &Value)> = map.iter().collect();
            props.sort_by(|a, b| stj::ordinal_cmp(a.0, b.0));
            sb.push('{');
            for (k, val) in props {
                stj::escape_into(sb, k);
                sb.push(':');
                append_canonical(sb, val);
                sb.push(',');
            }
            sb.push('}');
        }
        Value::Array(items) => {
            sb.push('[');
            for item in items {
                append_canonical(sb, item);
                sb.push(',');
            }
            sb.push(']');
        }
        Value::String(s) => stj::escape_into(sb, s),
        Value::Bool(b) => sb.push_str(if *b { "true" } else { "false" }),
    }
}
