//! The JSON wire format of .NET `System.Text.Json` with its default options, which is
//! what the .NET agent sends and what the hub hashes and compares.
//!
//! - Compact, properties in declaration order, `null` written.
//! - Strings use the default `JavaScriptEncoder`: printable ASCII stays as is except
//!   `" & ' + < > \``, which become `" & ' + < > ``;
//!   `\` is `\\`; `\b \t \n \f \r` keep their short form; every other control character,
//!   DEL and every non-ASCII character becomes `\uXXXX` (uppercase hex, UTF-16 surrogate
//!   pairs above U+FFFF). `/` is not escaped.
//! - `decimal` keeps its scale (`1.50`), integers are plain.
//! - `DateTime` is `yyyy-MM-ddTHH:mm:ss` plus the fraction of a second with trailing
//!   zeros removed (no fraction when it is zero); `DateTimeOffset` adds `+HH:MM`;
//!   `DateOnly` is `yyyy-MM-dd`; `Guid` is lowercase hyphenated.

use std::fmt::Write as _;

use dl_decimal::Decimal;
use dl_time::{Date, DateTime, OffsetDateTime};
use dl_uuid::Uuid;

/// A JSON value laid out exactly as `System.Text.Json` writes it.
#[derive(Debug, Clone, PartialEq)]
pub enum J {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A `decimal`, written with its scale.
    Dec(Decimal),
    /// A string, escaped by the default encoder.
    Str(String),
    /// Text written between quotes as is: dates and GUIDs, whose characters (`+`) STJ
    /// does not escape because it formats them directly.
    Lit(String),
    /// An array.
    Arr(Vec<J>),
    /// An object, properties in declaration order.
    Obj(Vec<(&'static str, J)>),
}

impl J {
    /// A string.
    pub fn str(s: &str) -> J {
        J::Str(s.to_owned())
    }

    /// A string or `null`.
    pub fn opt_str(s: Option<&str>) -> J {
        s.map_or(J::Null, J::str)
    }

    /// An integer or `null`.
    pub fn opt_int(v: Option<i64>) -> J {
        v.map_or(J::Null, J::Int)
    }

    /// A flag or `null`.
    pub fn opt_bool(v: Option<bool>) -> J {
        v.map_or(J::Null, J::Bool)
    }

    /// A decimal or `null`.
    pub fn opt_dec(v: Option<Decimal>) -> J {
        v.map_or(J::Null, J::Dec)
    }

    /// A `DateOnly`.
    pub fn date(d: Date) -> J {
        J::Lit(format_date(d))
    }

    /// A `DateOnly` or `null`.
    pub fn opt_date(d: Option<Date>) -> J {
        d.map_or(J::Null, J::date)
    }

    /// A `DateTime` (kind unspecified: no offset).
    pub fn datetime(dt: DateTime) -> J {
        J::Lit(format_datetime(dt))
    }

    /// A `DateTime` or `null`.
    pub fn opt_datetime(dt: Option<DateTime>) -> J {
        dt.map_or(J::Null, J::datetime)
    }

    /// A `DateTimeOffset`.
    pub fn dto(o: OffsetDateTime) -> J {
        J::Lit(format_dto(o))
    }

    /// A `DateTimeOffset` or `null`.
    pub fn opt_dto(o: Option<OffsetDateTime>) -> J {
        o.map_or(J::Null, J::dto)
    }

    /// A `Guid`.
    pub fn uuid(u: Uuid) -> J {
        J::Lit(u.to_string())
    }

    /// A `Guid` or `null`.
    pub fn opt_uuid(u: Option<Uuid>) -> J {
        u.map_or(J::Null, J::uuid)
    }

    /// An array of strings.
    pub fn strs(v: &[String]) -> J {
        J::Arr(v.iter().map(|s| J::str(s)).collect())
    }

    /// The compact JSON text.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => {
                let _ = write!(out, "{i}");
            }
            J::Dec(d) => {
                let _ = write!(out, "{d}");
            }
            J::Str(s) => {
                out.push('"');
                escape_into(out, s);
                out.push('"');
            }
            J::Lit(s) => {
                out.push('"');
                out.push_str(s);
                out.push('"');
            }
            J::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            J::Obj(props) => {
                out.push('{');
                for (i, (k, v)) in props.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    escape_into(out, k);
                    out.push_str("\":");
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Appends `s` escaped the way the default `JavaScriptEncoder` escapes it.
pub fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '"' | '&' | '\'' | '+' | '<' | '>' | '`' => push_u(out, c as u16),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    push_u(out, *u);
                }
            }
        }
    }
}

fn push_u(out: &mut String, unit: u16) {
    let _ = write!(out, "\\u{unit:04X}");
}

/// `yyyy-MM-dd`.
pub fn format_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month().number(), d.day())
}

/// `yyyy-MM-ddTHH:mm:ss[.fffffff]` with trailing zeros of the fraction removed.
pub fn format_datetime(dt: DateTime) -> String {
    let mut s = format!(
        "{}T{:02}:{:02}:{:02}",
        format_date(dt.date()),
        dt.hour(),
        dt.minute(),
        dt.second()
    );
    let ticks = dt.nanosecond() / 100;
    if ticks != 0 {
        let frac = format!("{ticks:07}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s
}

/// [`format_datetime`] of the local time plus `+HH:MM` / `-HH:MM`.
pub fn format_dto(o: OffsetDateTime) -> String {
    let mut s = format_datetime(o.datetime());
    let secs = o.offset().whole_seconds();
    let sign = if secs < 0 { '-' } else { '+' };
    let m = secs.unsigned_abs() / 60;
    let _ = write!(s, "{sign}{:02}:{:02}", m / 60, m % 60);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_time::Time;

    #[test]
    fn escapes_like_the_default_encoder() {
        let mut out = String::new();
        escape_into(&mut out, "a/\"<&>'+`\\\u{7f}\u{1}\tĐ😀");
        assert_eq!(
            out,
            "a/\\u0022\\u003C\\u0026\\u003E\\u0027\\u002B\\u0060\\\\\\u007F\\u0001\\t\\u0110\\uD83D\\uDE00"
        );
    }

    #[test]
    fn datetime_fraction_is_trimmed() {
        let d = Date::new(2026, 1, 2).unwrap();
        let t = |n| DateTime::new(d, Time::new(3, 4, 5, n).unwrap());
        assert_eq!(format_datetime(t(0)), "2026-01-02T03:04:05");
        assert_eq!(format_datetime(t(100_000_000)), "2026-01-02T03:04:05.1");
        assert_eq!(
            format_datetime(t(123_456_700)),
            "2026-01-02T03:04:05.1234567"
        );
    }

    #[test]
    fn objects_keep_declaration_order_and_nulls() {
        let j = J::Obj(vec![
            ("B", J::Int(1)),
            ("A", J::Null),
            ("C", J::Arr(vec![])),
        ]);
        assert_eq!(j.to_json(), r#"{"B":1,"A":null,"C":[]}"#);
    }
}
