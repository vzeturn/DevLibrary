//! Turning records into lines: text, JSON (plain and CLEF) and logfmt.

use std::fmt::Write as _;

use dl_json::Value;
use dl_time::OffsetDateTime;

use crate::{Level, Record};

mod json;
mod logfmt;
mod text;

pub use json::{JsonFormat, JsonKeys};
pub use logfmt::LogfmtFormat;
pub use text::TextFormat;

/// Renders a [`Record`] as one line of text.
///
/// Implementations append to `out` without a trailing newline; sinks add the line
/// terminator and write the whole line with a single call, so lines from different
/// threads never interleave. Formatting must not panic.
///
/// Any `Fn(&Record, &mut String) + Send + Sync` closure is a `Format`:
///
/// ```
/// use dl_log::{Format, Level, Record};
/// use dl_time::OffsetDateTime;
///
/// let short = |r: &Record, out: &mut String| {
///     out.push_str(r.level.as_str());
///     out.push_str(": ");
///     out.push_str(&r.message);
/// };
/// let record = Record::new(OffsetDateTime::UNIX_EPOCH, Level::Warn, "app", "disk 91% full");
/// assert_eq!(short.format_to_string(&record), "WARN: disk 91% full");
/// ```
pub trait Format: Send + Sync {
    /// Appends the rendering of `record` to `out`, without a newline.
    fn format(&self, record: &Record, out: &mut String);

    /// Renders `record` into a new string.
    fn format_to_string(&self, record: &Record) -> String {
        let mut out = String::new();
        self.format(record, &mut out);
        out
    }
}

impl<F: Fn(&Record, &mut String) + Send + Sync> Format for F {
    fn format(&self, record: &Record, out: &mut String) {
        self(record, out)
    }
}

/// Writes `t` as RFC 3339 with exactly three fraction digits: `2026-11-15T10:20:30.123+07:00`,
/// or `…Z` at UTC.
pub(crate) fn write_timestamp(out: &mut String, t: OffsetDateTime) {
    let year = t.year();
    let offset = t.offset().whole_seconds();
    if (0..=9999).contains(&year) && offset % 60 == 0 {
        // Fast path: fill a fixed template.
        let mut b = *b"0000-00-00T00:00:00.000+00:00";
        put_digits(&mut b[0..4], year as u32);
        put_digits(&mut b[5..7], u32::from(t.month().number()));
        put_digits(&mut b[8..10], u32::from(t.day()));
        put_digits(&mut b[11..13], u32::from(t.hour()));
        put_digits(&mut b[14..16], u32::from(t.minute()));
        put_digits(&mut b[17..19], u32::from(t.second()));
        put_digits(&mut b[20..23], t.nanosecond() / 1_000_000);
        let len = if offset == 0 {
            b[23] = b'Z';
            24
        } else {
            if offset < 0 {
                b[23] = b'-';
            }
            let abs = offset.unsigned_abs();
            put_digits(&mut b[24..26], abs / 3600);
            put_digits(&mut b[27..29], abs / 60 % 60);
            29
        };
        // The template and digits are ASCII, so this never fails.
        out.push_str(std::str::from_utf8(&b[..len]).unwrap_or_default());
        return;
    }
    write_timestamp_slow(out, t);
}

/// Writes the low `buf.len()` decimal digits of `n`, zero-padded.
fn put_digits(buf: &mut [u8], mut n: u32) {
    for slot in buf.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
}

fn write_timestamp_slow(out: &mut String, t: OffsetDateTime) {
    let year = t.year();
    if (0..=9999).contains(&year) {
        push_digits(out, year as u32, 4);
    } else {
        let _ = write!(out, "{year:+05}");
    }
    out.push('-');
    push_digits(out, u32::from(t.month().number()), 2);
    out.push('-');
    push_digits(out, u32::from(t.day()), 2);
    out.push('T');
    push_digits(out, u32::from(t.hour()), 2);
    out.push(':');
    push_digits(out, u32::from(t.minute()), 2);
    out.push(':');
    push_digits(out, u32::from(t.second()), 2);
    out.push('.');
    push_digits(out, t.nanosecond() / 1_000_000, 3);
    let offset = t.offset().whole_seconds();
    if offset == 0 {
        out.push('Z');
        return;
    }
    out.push(if offset < 0 { '-' } else { '+' });
    let abs = offset.unsigned_abs();
    push_digits(out, abs / 3600, 2);
    out.push(':');
    push_digits(out, abs / 60 % 60, 2);
    if !abs.is_multiple_of(60) {
        out.push(':');
        push_digits(out, abs % 60, 2);
    }
}

fn push_digits(out: &mut String, mut n: u32, width: usize) {
    let mut buf = [b'0'; 10];
    let mut i = buf.len();
    while n > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let start = (buf.len() - width).min(i);
    for &b in &buf[start..] {
        out.push(char::from(b));
    }
}

/// Writes `s` as a JSON string with the same escaping as `dl_json`: `"`, `\`, control
/// characters and U+2028/U+2029 are escaped, everything else is copied as UTF-8.
pub(crate) fn write_json_str(out: &mut String, s: &str) {
    out.push('"');
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (escape, len): (&str, usize) = match b {
            b'"' => ("\\\"", 1),
            b'\\' => ("\\\\", 1),
            b'\n' => ("\\n", 1),
            b'\r' => ("\\r", 1),
            b'\t' => ("\\t", 1),
            0x08 => ("\\b", 1),
            0x0c => ("\\f", 1),
            0x00..=0x1f => ("", 1),
            0xe2 if bytes.get(i + 1) == Some(&0x80)
                && matches!(bytes.get(i + 2), Some(0xa8 | 0xa9)) =>
            {
                (
                    if bytes[i + 2] == 0xa8 {
                        "\\u2028"
                    } else {
                        "\\u2029"
                    },
                    3,
                )
            }
            _ => {
                i += 1;
                continue;
            }
        };
        out.push_str(&s[start..i]);
        if escape.is_empty() {
            let _ = write!(out, "\\u{b:04x}");
        } else {
            out.push_str(escape);
        }
        i += len;
        start = i;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

/// Writes a field value for human-readable formats: strings bare when safe, otherwise
/// quoted; other values as compact JSON.
pub(crate) fn write_text_value(out: &mut String, value: &Value, quote_json: bool) {
    match value {
        Value::String(s) => write_maybe_quoted(out, s),
        Value::Array(_) | Value::Object(_) if quote_json => {
            write_quoted(out, &dl_json::to_string(value));
        }
        other => dl_json::write_to(other, out),
    }
}

/// Writes `s` bare when it is non-empty and has no whitespace, quotes, `=`, `\` or
/// control characters; otherwise quoted with escapes.
pub(crate) fn write_maybe_quoted(out: &mut String, s: &str) {
    let needs_quotes = s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '=' | '\\'));
    if needs_quotes {
        write_quoted(out, s);
    } else {
        out.push_str(s);
    }
}

fn write_quoted(out: &mut String, s: &str) {
    out.push('"');
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' || c == '\\' || c.is_control() {
            out.push_str(&s[start..i]);
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c => push_control_escape(out, c),
            }
            start = i + c.len_utf8();
        }
    }
    out.push_str(&s[start..]);
    out.push('"');
}

/// Copies a message, escaping control characters other than tab so a record is always
/// one line and cannot inject terminal escape sequences.
pub(crate) fn write_escaped_message(out: &mut String, s: &str) {
    if !s.chars().any(|c| c.is_control() && c != '\t') {
        out.push_str(s);
        return;
    }
    for c in s.chars() {
        if c.is_control() && c != '\t' {
            push_control_escape(out, c);
        } else {
            out.push(c);
        }
    }
}

fn push_control_escape(out: &mut String, c: char) {
    match c {
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        c => {
            let _ = write!(out, "\\u{{{:x}}}", c as u32);
        }
    }
}

/// Writes a field name for text and logfmt output, replacing characters that would
/// break `key=value` parsing with `_`.
pub(crate) fn write_text_key(out: &mut String, key: &str) {
    if key.is_empty() {
        out.push('_');
        return;
    }
    let bad = |c: char| c.is_whitespace() || c.is_control() || matches!(c, '"' | '=');
    if !key.chars().any(bad) {
        out.push_str(key);
        return;
    }
    for c in key.chars() {
        if c.is_whitespace() || c.is_control() || matches!(c, '"' | '=') {
            out.push('_');
        } else {
            out.push(c);
        }
    }
}

/// The ANSI color sequence for a level.
pub(crate) fn level_color(level: Level) -> &'static str {
    match level {
        Level::Trace => "\x1b[35m",
        Level::Debug => "\x1b[34m",
        Level::Info => "\x1b[32m",
        Level::Warn => "\x1b[33m",
        Level::Error => "\x1b[31m",
        Level::Fatal => "\x1b[1;41;37m",
    }
}

pub(crate) const ANSI_DIM: &str = "\x1b[2m";
pub(crate) const ANSI_RESET: &str = "\x1b[0m";

/// 32-bit Jenkins one-at-a-time hash over the UTF-16 code units of a message template,
/// the event type id that Serilog's `RenderedCompactJsonFormatter` writes to `@i`.
pub(crate) fn event_id(template: &str) -> u32 {
    let mut hash: u32 = 0;
    for unit in template.encode_utf16() {
        hash = hash.wrapping_add(u32::from(unit));
        hash = hash.wrapping_add(hash << 10);
        hash ^= hash >> 6;
    }
    hash = hash.wrapping_add(hash << 3);
    hash ^= hash >> 11;
    hash.wrapping_add(hash << 15)
}

/// `true` when `template` has no `{…}` holes (only `{{` / `}}` escapes), so it is a valid
/// message template whose rendering is the message itself.
pub(crate) fn is_plain_template(template: &str) -> bool {
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b @ (b'{' | b'}') => {
                if bytes.get(i + 1) != Some(&b) {
                    return false;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_time::Offset;

    #[test]
    fn timestamps_have_millisecond_precision() {
        let cases = [
            (
                "2026-11-15T10:20:30.123456789+07:00",
                "2026-11-15T10:20:30.123+07:00",
            ),
            ("2026-11-15T03:20:30Z", "2026-11-15T03:20:30.000Z"),
            (
                "0001-01-01T00:00:00.999-05:30",
                "0001-01-01T00:00:00.999-05:30",
            ),
            (
                "9999-12-31T23:59:59.5+14:00",
                "9999-12-31T23:59:59.500+14:00",
            ),
        ];
        for (input, expected) in cases {
            let t = OffsetDateTime::parse_rfc3339(input).unwrap();
            let mut s = String::new();
            write_timestamp(&mut s, t);
            assert_eq!(s, expected);
        }
        let t = OffsetDateTime::UNIX_EPOCH.to_offset(Offset::from_hms(1, 2, 3).unwrap());
        let mut s = String::new();
        write_timestamp(&mut s, t);
        assert_eq!(s, "1970-01-01T01:02:03.000+01:02:03");
        let t = dl_time::Date::new(-44, 3, 15)
            .unwrap()
            .and_hms(12, 0, 0)
            .unwrap()
            .assume_utc();
        let mut s = String::new();
        write_timestamp(&mut s, t);
        assert_eq!(s, "-0044-03-15T12:00:00.000Z");
    }

    #[test]
    fn json_strings_match_dl_json() {
        let mut samples: Vec<String> = (0u32..0x80)
            .filter_map(char::from_u32)
            .map(String::from)
            .collect();
        samples.extend(
            [
                "",
                "plain",
                "Phòng \"VIP\"\nđã đặt",
                "tab\there\\back",
                "\u{2028}\u{2029}\u{2027}\u{202a}",
                "Nguyễn Văn An – 🏨",
                "\u{7f}\u{9b}\u{1b}[31m",
            ]
            .map(String::from),
        );
        for s in samples {
            let mut ours = String::new();
            write_json_str(&mut ours, &s);
            assert_eq!(ours, Value::from(s.as_str()).to_string(), "{s:?}");
        }
    }

    #[test]
    fn text_values_quote_when_needed() {
        let cases = [
            (Value::from("abc"), "abc"),
            (Value::from("Hà_Nội"), "Hà_Nội"),
            (Value::from(""), "\"\""),
            (Value::from("a b"), "\"a b\""),
            (Value::from("a=b"), "\"a=b\""),
            (Value::from("say \"hi\"\n"), "\"say \\\"hi\\\"\\n\""),
            (Value::from("c:\\temp"), "\"c:\\\\temp\""),
            (Value::from("\u{1b}[2J"), "\"\\u{1b}[2J\""),
            (Value::from(42), "42"),
            (Value::from(-1.5), "-1.5"),
            (Value::Null, "null"),
            (Value::from(true), "true"),
            (dl_json::json!({"a": [1, "x y"]}), "{\"a\":[1,\"x y\"]}"),
        ];
        for (value, expected) in cases {
            let mut s = String::new();
            write_text_value(&mut s, &value, false);
            assert_eq!(s, expected);
        }
        let mut s = String::new();
        write_text_value(&mut s, &dl_json::json!([1, 2]), true);
        assert_eq!(s, "\"[1,2]\"");
    }

    #[test]
    fn messages_and_keys_are_single_line() {
        let mut s = String::new();
        write_escaped_message(&mut s, "line1\nline2\r\tend\u{1b}[31m\u{85}");
        assert_eq!(s, "line1\\nline2\\r\tend\\u{1b}[31m\\u{85}");
        let mut s = String::new();
        write_text_key(&mut s, "http status=\"x\"");
        write_text_key(&mut s, "");
        assert_eq!(s, "http_status__x__");
    }

    #[test]
    fn jenkins_one_at_a_time_vectors() {
        // Test vectors from https://en.wikipedia.org/wiki/Jenkins_hash_function
        assert_eq!(event_id("a"), 0xca2e_9442);
        assert_eq!(
            event_id("The quick brown fox jumps over the lazy dog"),
            0x519e_91f5
        );
        assert_eq!(event_id(""), 0);
    }

    #[test]
    fn plain_templates() {
        assert!(is_plain_template("booking confirmed"));
        assert!(is_plain_template("set {{a}} literally"));
        assert!(is_plain_template(""));
        assert!(!is_plain_template("user {} logged in"));
        assert!(!is_plain_template("user {name}"));
        assert!(!is_plain_template("dangling {"));
        assert!(!is_plain_template("dangling }"));
        assert!(!is_plain_template("{{{x}}}"));
    }
}
