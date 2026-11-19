//! JSON text the way System.Text.Json writes it, byte for byte.
//!
//! The .NET agent and cloud hash and compare the JSON they exchange, and the cloud stores
//! the rows the agent sends, so the module writes exactly what `JsonSerializer` would:
//! the default encoder (every non-ASCII character and `" & ' + < > \` \`` as `\uXXXX`,
//! upper-case hex), shortest round-trip numbers in .NET notation (`1E-05`, `1E+17`) and
//! .NET date/time text. [`num`] holds the number and date formats.

pub mod num;

use std::fmt::Write as _;

use dl_json::{Map, Number, Value};

pub use num::{
    format_datetime, format_datetime_o, format_dto, format_f32, format_f64, format_time,
};

/// Appends `s` as a quoted JSON string escaped like STJ's default encoder.
pub fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\\' => out.push_str("\\\\"),
            '"' | '&' | '\'' | '+' | '<' | '>' | '`' => push_unit(out, ch as u16),
            ' '..='~' => out.push(ch),
            _ => {
                let mut units = [0u16; 2];
                for u in ch.encode_utf16(&mut units) {
                    push_unit(out, *u);
                }
            }
        }
    }
    out.push('"');
}

fn push_unit(out: &mut String, unit: u16) {
    let _ = write!(out, "\\u{unit:04X}");
}

/// `s` as a quoted, escaped JSON string (`JsonSerializer.Serialize(s)`).
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    escape_into(&mut out, s);
    out
}

/// Compact JSON text of `v` (property order kept, nulls written).
pub fn to_string(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

/// Appends the compact JSON text of `v`.
pub fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => escape_into(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => write_object(out, map),
    }
}

fn write_object(out: &mut String, map: &Map) {
    out.push('{');
    for (i, (k, val)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        escape_into(out, k);
        out.push(':');
        write_value(out, val);
    }
    out.push('}');
}

/// A number as written: the raw text it was read with, else its integer or double form.
pub fn write_number(out: &mut String, n: &Number) {
    if let Some(raw) = n.as_raw_str() {
        out.push_str(raw);
    } else if let Some(i) = n.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = n.as_u64() {
        let _ = write!(out, "{u}");
    } else {
        out.push_str(&format_f64(n.as_f64().unwrap_or(0.0)));
    }
}

/// JSON text indented by two spaces (`WriteIndented = true`).
pub fn to_string_indented(v: &Value) -> String {
    let mut out = String::new();
    write_indented(&mut out, v, 0);
    out
}

fn write_indented(out: &mut String, v: &Value, depth: usize) {
    let pad = |out: &mut String, d: usize| {
        out.push('\n');
        for _ in 0..d {
            out.push_str("  ");
        }
    };
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                pad(out, depth + 1);
                write_indented(out, item, depth + 1);
            }
            pad(out, depth);
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                pad(out, depth + 1);
                escape_into(out, k);
                out.push_str(": ");
                write_indented(out, val, depth + 1);
            }
            pad(out, depth);
            out.push('}');
        }
        other => write_value(out, other),
    }
}

/// Parses JSON keeping every number's text (needed to hash and re-send it unchanged).
///
/// # Errors
/// The parser's message for malformed input.
pub fn parse(text: &str) -> Result<Value, String> {
    let opts = dl_json::ParseOptions {
        max_depth: 128,
        keep_raw_numbers: true,
    };
    dl_json::parse_with(text, opts).map_err(|e| e.to_string())
}

/// The value of property `name`, matched case-insensitively like STJ's web defaults
/// (the last matching property wins).
pub fn get_ci<'a>(map: &'a Map, name: &str) -> Option<&'a Value> {
    let mut found = None;
    for (k, v) in map.iter() {
        if k.eq_ignore_ascii_case(name) || (!k.is_ascii() && ci_eq(k, name)) {
            found = Some(v);
        }
    }
    found
}

/// Ordinal ignore-case equality (.NET `OrdinalIgnoreCase`: simple upper-case mapping).
pub fn ci_eq(a: &str, b: &str) -> bool {
    let mut x = a.chars().map(upper_char);
    let mut y = b.chars().map(upper_char);
    loop {
        match (x.next(), y.next()) {
            (None, None) => return true,
            (Some(p), Some(q)) if p == q => {}
            _ => return false,
        }
    }
}

/// Ordinal ignore-case ordering over UTF-16 code units (.NET `StringComparer.OrdinalIgnoreCase`).
pub fn ci_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let ua: String = a.chars().map(upper_char).collect();
    let ub: String = b.chars().map(upper_char).collect();
    ordinal_cmp(&ua, &ub)
}

/// Ordinal ordering over UTF-16 code units (.NET `StringComparer.Ordinal`).
pub fn ordinal_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `ToUpperInvariant` for one character (characters without a one-to-one mapping stay).
pub fn upper_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_uppercase();
    }
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => u,
        _ => c,
    }
}

/// `s.ToUpperInvariant()`.
pub fn upper(s: &str) -> String {
    s.chars().map(upper_char).collect()
}

/// A string property: `None` for null or absent.
///
/// # Errors
/// When the value is neither a string nor null.
pub fn opt_str(map: &Map, name: &str) -> Result<Option<String>, String> {
    match get_ci(map, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!("'{name}' is a {}, not a string", other.type_name())),
    }
}

/// An integer property (numbers may be quoted, like STJ's web defaults): `None` for null or absent.
///
/// # Errors
/// When the value is not an integer.
pub fn opt_i64(map: &Map, name: &str) -> Result<Option<i64>, String> {
    let bad = || format!("'{name}' is not an integer");
    match get_ci(map, name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_raw_str() {
            Some(raw) => raw.parse::<i64>().map(Some).map_err(|_| bad()),
            None => n.as_i64().map(Some).ok_or_else(bad),
        },
        Some(Value::String(s)) => s.parse::<i64>().map(Some).map_err(|_| bad()),
        Some(_) => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_the_default_encoder() {
        // `\x5C` is a backslash: the expected text spells each `\uXXXX` escape out.
        assert_eq!(
            quote("a+b<c>&'\"\\/~`"),
            "\"a\x5Cu002Bb\x5Cu003Cc\x5Cu003E\x5Cu0026\x5Cu0027\x5Cu0022\\\\/~\x5Cu0060\""
        );
        assert_eq!(
            quote("\u{1}\u{7f}\u{e9}\u{1f600}"),
            "\"\x5Cu0001\x5Cu007F\x5Cu00E9\x5CuD83D\x5CuDE00\""
        );
    }

    #[test]
    fn indents_like_write_indented() {
        let v = parse(r#"{"a":1,"b":[],"c":{"d":null}}"#).unwrap_or(Value::Null);
        assert_eq!(
            to_string_indented(&v),
            "{\n  \"a\": 1,\n  \"b\": [],\n  \"c\": {\n    \"d\": null\n  }\n}"
        );
    }

    #[test]
    fn reads_case_insensitively() {
        let v = parse(r#"{"RequestId":"x","n":"12"}"#).unwrap_or(Value::Null);
        let m = v.as_object().cloned().unwrap_or_default();
        assert_eq!(opt_str(&m, "requestId"), Ok(Some("x".into())));
        assert_eq!(opt_i64(&m, "N"), Ok(Some(12)));
        assert_eq!(opt_i64(&m, "missing"), Ok(None));
    }
}
