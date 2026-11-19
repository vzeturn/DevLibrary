//! The JSON-lines log format (port of `Reporting/JsonLogFormatter.cs`): one JSON object
//! per line with `ts`, `level`, `source`, `run_id`, `agent_id`, `tenant`, `code`, `msg`,
//! `exception`, then every other property in snake_case, written with the relaxed encoder.
//! Every string passes through [`scrub`](super::scrubber::scrub) first.
//!
//! Also the Serilog value rendering the message uses (`LogEventPropertyValue.Render` with
//! the invariant culture).

use super::log::{Event, Level, Value};
use super::scrubber::scrub;
use super::template::Token;
use crate::config::time::{Timestamp, civil_parts};
use crate::util::stj::{Escaping, Style, Writer, f64_text};

/// Fixed field names; a property that would collide with one is written as `prop_<name>`.
const FIXED: [&str; 9] = [
    "ts",
    "level",
    "source",
    "run_id",
    "agent_id",
    "tenant",
    "code",
    "msg",
    "exception",
];

/// Fields lifted from properties, with the property names tried in order.
const LIFTED: [(&str, &[&str]); 5] = [
    ("source", &["source"]),
    ("run_id", &["run_id", "RunId"]),
    ("agent_id", &["agent_id", "AgentId"]),
    ("tenant", &["tenant", "TenantId"]),
    ("code", &["code", "Code"]),
];

/// The level as the site events name it: `debug`, `info`, `warn`, `error`.
pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Verbose | Level::Debug => "debug",
        Level::Information => "info",
        Level::Warning => "warn",
        Level::Error | Level::Fatal => "error",
    }
}

/// Stable UPPER_SNAKE codes only (`^[A-Z][A-Z0-9_]*$`, at most 64 characters).
pub fn is_code(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// `JsonNamingPolicy.SnakeCaseLower.ConvertName`.
pub fn snake_case_lower(name: &str) -> String {
    #[derive(PartialEq)]
    enum State {
        NotStarted,
        Upper,
        LowerOrDigit,
        Space,
    }
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    let mut state = State::NotStarted;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            match state {
                State::LowerOrDigit | State::Space => out.push('_'),
                State::Upper => {
                    if chars.get(i + 1).is_some_and(|n| n.is_lowercase()) {
                        out.push('_');
                    }
                }
                State::NotStarted => {}
            }
            out.extend(c.to_lowercase());
            state = State::Upper;
        } else if c.is_lowercase() || c.is_ascii_digit() || (c.is_numeric() && !c.is_ascii()) {
            if state == State::Space {
                out.push('_');
            }
            out.push(c);
            state = State::LowerOrDigit;
        } else if c.is_whitespace() && c != '\t' && c != '\n' && c != '\r' {
            if state != State::NotStarted {
                state = State::Space;
            }
        } else {
            out.push(c);
            state = State::NotStarted;
        }
    }
    out
}

/// The JSON field of a property: `SourceContext` is `logger`; a name with `_` or without
/// upper-case letters is kept; anything else is snake_cased.
pub fn field_name(key: &str) -> String {
    if key == "SourceContext" {
        "logger".to_owned()
    } else if key.contains('_') || !key.chars().any(char::is_uppercase) {
        key.to_owned()
    } else {
        snake_case_lower(key)
    }
}

/// `double.ToString(null, InvariantCulture)`.
fn double_text(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_owned()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        f64_text(v).unwrap_or_default()
    }
}

/// `TimeSpan.ToString()` (the constant `c` format): `[-][d.]hh:mm:ss[.fffffff]`.
pub fn time_span_text(ticks: i64) -> String {
    let neg = ticks < 0;
    let t = ticks.unsigned_abs();
    let per_sec = 10_000_000u64;
    let days = t / (86_400 * per_sec);
    let rem = t % (86_400 * per_sec);
    let h = rem / (3600 * per_sec);
    let m = (rem / (60 * per_sec)) % 60;
    let s = (rem / per_sec) % 60;
    let frac = rem % per_sec;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if days > 0 {
        out.push_str(&format!("{days}."));
    }
    out.push_str(&format!("{h:02}:{m:02}:{s:02}"));
    if frac != 0 {
        out.push_str(&format!(".{frac:07}"));
    }
    out
}

/// `DateTime.ToString()` (invariant) of a UTC instant: `MM/dd/yyyy HH:mm:ss`.
fn date_time_general(unix_nanos: i128) -> String {
    let (y, mo, d, h, mi, s, _) = civil_parts(unix_nanos);
    format!("{mo:02}/{d:02}/{y:04} {h:02}:{mi:02}:{s:02}")
}

/// `DateTime.ToString("O")` of a UTC instant: `yyyy-MM-ddTHH:mm:ss.fffffffZ`.
fn date_time_round_trip(unix_nanos: i128) -> String {
    let mut s = crate::config::time::wall_text(unix_nanos, true);
    s.push('Z');
    s
}

/// Inserts `,` every three digits of an integer part.
fn group(int_part: &str) -> String {
    let mut out = String::with_capacity(int_part.len() + int_part.len() / 3);
    let n = int_part.len();
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Rounds a non-negative decimal digit string `int.frac` to `decimals` places, half away
/// from zero (how .NET formats integers and decimals).
fn round_digits(int_part: &str, frac: &str, decimals: usize) -> (String, String) {
    let mut digits: Vec<u8> = int_part.bytes().chain(frac.bytes()).collect();
    let keep = int_part.len() + decimals;
    while digits.len() < keep {
        digits.push(b'0');
    }
    let round_up = digits.get(keep).is_some_and(|d| *d >= b'5');
    digits.truncate(keep);
    let mut int_len = int_part.len();
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                int_len += 1;
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s = String::from_utf8(digits).unwrap_or_default();
    let (i, f) = s.split_at(int_len);
    let i = i.trim_start_matches('0');
    (
        if i.is_empty() {
            "0".to_owned()
        } else {
            i.to_owned()
        },
        f.to_owned(),
    )
}

/// The standard numeric formats the agent's templates use: `P<n>` (percent), `F<n>`,
/// `N<n>` and the custom `0`, `0.0…` patterns; `None` for anything else (rendered with
/// the default format then).
fn format_number(value: &Value, format: &str) -> Option<String> {
    let custom = format == "0"
        || format
            .strip_prefix("0.")
            .is_some_and(|f| !f.is_empty() && f.bytes().all(|b| b == b'0'));
    let (kind, decimals) = if custom {
        ('C', format.len().saturating_sub(2))
    } else {
        let mut cs = format.chars();
        let k = cs.next()?.to_ascii_uppercase();
        let rest = cs.as_str();
        let d = if rest.is_empty() {
            match k {
                'P' | 'F' | 'N' => 2,
                _ => return None,
            }
        } else {
            rest.parse::<usize>().ok().filter(|d| *d <= 99)?
        };
        if !matches!(k, 'P' | 'F' | 'N') {
            return None;
        }
        (k, d)
    };
    let shift = if kind == 'P' { 2 } else { 0 };
    let (neg, int_part, frac_part) = match value {
        Value::Float(f) if f.is_finite() => {
            let text = format!("{:.*}", decimals + shift, f.abs());
            let (i, fr) = text.split_once('.').unwrap_or((&text, ""));
            // Shift the decimal point right for percent.
            let all = format!("{i}{fr}");
            let int_len = i.len() + shift;
            let (i2, f2) = all.split_at(int_len.min(all.len()));
            let i2 = i2.trim_start_matches('0');
            (
                f.is_sign_negative(),
                if i2.is_empty() {
                    "0".to_owned()
                } else {
                    i2.to_owned()
                },
                f2.to_owned(),
            )
        }
        Value::Int(_) | Value::UInt(_) | Value::Decimal(_) => {
            let text = match value {
                Value::Int(n) => n.to_string(),
                Value::UInt(n) => n.to_string(),
                Value::Decimal(s) => s.clone(),
                _ => return None,
            };
            let neg = text.starts_with('-');
            let body = text.trim_start_matches(['-', '+']);
            let (i, f) = body.split_once('.').unwrap_or((body, ""));
            if !i.bytes().chain(f.bytes()).all(|b| b.is_ascii_digit()) {
                return None;
            }
            // Percent: move two digits from the fraction into the integer part.
            let mut f_owned = f.to_owned();
            while f_owned.len() < shift {
                f_owned.push('0');
            }
            let (moved, rest) = f_owned.split_at(shift);
            let int_digits = format!("{i}{moved}");
            let (ri, rf) = round_digits(&int_digits, rest, decimals);
            let is_zero = ri.bytes().chain(rf.bytes()).all(|b| b == b'0');
            (neg && !is_zero, ri, rf)
        }
        _ => return None,
    };
    let int_text = if matches!(kind, 'N' | 'P') {
        group(&int_part)
    } else {
        int_part
    };
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&int_text);
    if decimals > 0 {
        out.push('.');
        out.push_str(&frac_part);
    }
    if kind == 'P' {
        out.push_str(" %");
    }
    Some(out)
}

/// `LogEventPropertyValue.Render(output, format, InvariantCulture)`: strings quoted (with
/// `"` escaped as `\"`) unless the format is `l`, sequences as `[a, b]`, structures as
/// `{ A: 1 }`, dictionaries as `[("k": v)]`.
pub fn render_value(value: &Value, format: Option<&str>) -> String {
    let mut out = String::new();
    render_into(&mut out, value, format);
    out
}

fn render_into(out: &mut String, value: &Value, format: Option<&str>) {
    if let Some(f) = format
        && let Some(s) = format_number(value, f)
    {
        out.push_str(&s);
        return;
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "True" } else { "False" }),
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::UInt(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&double_text(*f)),
        Value::Decimal(s) => out.push_str(s),
        Value::Enum(name) => out.push_str(name),
        Value::Str(s) => {
            if format == Some("l") {
                out.push_str(s);
            } else {
                out.push('"');
                out.push_str(&s.replace('"', "\\\""));
                out.push('"');
            }
        }
        Value::DateTimeOffset(t) => match format {
            Some("O" | "o") => out.push_str(&t.to_round_trip()),
            _ => out.push_str(&t.to_invariant_general()),
        },
        Value::DateTimeUtc(n) => match format {
            Some("O" | "o") => out.push_str(&date_time_round_trip(*n)),
            _ => out.push_str(&date_time_general(*n)),
        },
        Value::TimeSpan(t) => out.push_str(&time_span_text(*t)),
        Value::Raw(json) => out.push_str(json),
        Value::Seq(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_into(out, item, format);
            }
            out.push(']');
        }
        Value::Structure(members) => {
            out.push_str("{ ");
            for (i, (name, v)) in members.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(name);
                out.push_str(": ");
                render_into(out, v, None);
            }
            out.push_str(" }");
        }
        Value::Dict(pairs) => {
            out.push('[');
            for (i, (k, v)) in pairs.iter().enumerate() {
                out.push_str(if i == 0 { "(" } else { ", (" });
                render_into(out, k, None);
                out.push_str(": ");
                render_into(out, v, None);
                out.push(')');
            }
            out.push(']');
        }
    }
}

/// `JsonLogFormatter.AsText`: strings as is, scalars with their invariant text, raw JSON
/// as written, anything else rendered.
pub fn as_text(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Int(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::Float(f) => double_text(*f),
        Value::Decimal(s) => s.clone(),
        Value::DateTimeOffset(t) => t.to_invariant_general(),
        Value::DateTimeUtc(n) => date_time_general(*n),
        Value::TimeSpan(t) => time_span_text(*t),
        Value::Raw(json) => json.clone(),
        other => render_value(other, None),
    }
}

/// The message with each property rendered plainly: strings without quotes, other values
/// rendered with the hole's format; a hole without a property stays `{Name}`.
pub fn render_message(e: &Event) -> String {
    let mut out = String::new();
    for token in e.template.tokens() {
        match token {
            Token::Text(t) => out.push_str(t),
            Token::Property(p) => match e.property(&p.name) {
                Some(Value::Str(s)) => out.push_str(s),
                Some(v) => render_into(&mut out, v, p.format.as_deref()),
                None => {
                    out.push('{');
                    out.push_str(&p.name);
                    out.push('}');
                }
            },
        }
    }
    out
}

fn liftable(field: &str, v: &Value) -> bool {
    v.is_scalar()
        && !matches!(v, Value::Null)
        && (field != "code" || matches!(v, Value::Str(s) if is_code(s)))
}

/// `yyyy-MM-ddTHH:mm:ss.fffZ` of the instant at UTC (milliseconds truncated).
fn ts_text(t: &Timestamp) -> String {
    let (y, mo, d, h, mi, s, ticks) = civil_parts(t.unix_nanos());
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{:03}Z",
        ticks / 10_000
    )
}

/// The line for `e`, without its newline.
pub fn render(e: &Event) -> String {
    let mut w = Writer::with_escaping(Style::Compact, Escaping::Relaxed);
    w.begin_object();
    w.key("ts").string(&ts_text(&e.timestamp));
    w.key("level").string(level_name(e.level));
    let mut used: Vec<&str> = Vec::new();
    for (field, names) in LIFTED {
        let found = names
            .iter()
            .find(|n| e.property(n).is_some_and(|v| liftable(field, v)));
        if let Some(name) = found
            && let Some(v) = e.property(name)
        {
            used.push(name);
            w.key(field).string(&scrub(&as_text(v)));
        }
    }
    w.key("msg").string(&scrub(&render_message(e)));
    if let Some(ex) = &e.exception {
        w.key("exception").string(&scrub(&ex.to_dotnet_string()));
    }
    let mut written: Vec<String> = FIXED.iter().map(|s| (*s).to_owned()).collect();
    for p in &e.properties {
        if used.contains(&p.name.as_str()) {
            continue;
        }
        let mut field = field_name(&p.name);
        if written.contains(&field) {
            field = format!("prop_{field}");
            if written.contains(&field) {
                continue;
            }
        }
        written.push(field.clone());
        w.key(&field);
        write_value(&mut w, &p.value);
    }
    w.end_object();
    w.finish()
}

/// The line for `e` with its `\n` (`ITextFormatter.Format`).
pub fn format_line(e: &Event) -> String {
    let mut s = render(e);
    s.push('\n');
    s
}

fn write_value(w: &mut Writer, value: &Value) {
    match value {
        Value::Raw(json) => {
            // WriteRawValue validates; the agent only makes raw values from JSON objects.
            if crate::util::stj::parse(json).is_ok() {
                w.raw(json);
            } else {
                w.string(&scrub(json));
            }
        }
        Value::Null => {
            w.null();
        }
        Value::Bool(b) => {
            w.bool(*b);
        }
        Value::Str(s) => {
            w.string(&scrub(s));
        }
        Value::Enum(name) => {
            w.string(&scrub(name));
        }
        Value::Int(n) => {
            w.int(*n);
        }
        Value::UInt(n) => {
            w.uint(*n);
        }
        Value::Float(f) if f.is_finite() => {
            w.f64(*f);
        }
        Value::Float(f) => {
            w.string(&scrub(&double_text(*f)));
        }
        Value::Decimal(s) => {
            w.raw(s);
        }
        Value::DateTimeOffset(t) => {
            w.string(&t.to_utc().to_round_trip());
        }
        Value::DateTimeUtc(n) => {
            w.string(&date_time_round_trip(*n));
        }
        Value::TimeSpan(t) => {
            w.string(&scrub(&time_span_text(*t)));
        }
        Value::Seq(items) => {
            w.begin_array();
            for item in items {
                write_value(w, item);
            }
            w.end_array();
        }
        Value::Structure(members) => {
            w.begin_object();
            for (name, v) in members {
                w.key(&field_name(name));
                write_value(w, v);
            }
            w.end_object();
        }
        Value::Dict(pairs) => {
            w.begin_object();
            for (k, v) in pairs {
                w.key(&scrub(&as_text(k)));
                write_value(w, v);
            }
            w.end_object();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::log::{Exception, Property};

    fn at(s: &str) -> Timestamp {
        Timestamp::parse_iso(s).unwrap()
    }

    fn event(ts: Timestamp, level: Level, template: &str, props: Vec<(&str, Value)>) -> Event {
        let mut e = Event::new(ts, level, template);
        e.properties = props
            .into_iter()
            .map(|(n, v)| Property::new(n, v))
            .collect();
        e
    }

    #[test]
    fn snake_case_matches_system_text_json() {
        for (a, b) in [
            ("SomeCamelCaseURLValue", "some_camel_case_url_value"),
            ("HTTPStatus", "http_status"),
            ("StreamName", "stream_name"),
            ("SHA512Hash", "sha512_hash"),
            ("X", "x"),
            ("ABC???def", "abc???def"),
            ("A B", "a_b"),
        ] {
            assert_eq!(snake_case_lower(a), b, "{a}");
        }
        assert_eq!(field_name("already_snake"), "already_snake");
        assert_eq!(field_name("lower"), "lower");
        assert_eq!(field_name("SourceContext"), "logger");
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn values_render_like_serilog() {
        assert_eq!(time_span_text(15_000_000), "00:00:01.5000000");
        assert_eq!(time_span_text(-1_296_000_000_000), "-1.12:00:00");
        assert_eq!(time_span_text(123_456_789_012_345), "142.21:21:18.9012345");
        assert_eq!(render_value(&Value::Float(0.853), Some("P0")), "85 %");
        assert_eq!(render_value(&Value::Float(0.12345), Some("P1")), "12.3 %");
        assert_eq!(render_value(&Value::Float(0.125), Some("P0")), "12 %");
        assert_eq!(render_value(&Value::Float(3.14159), Some("0.00")), "3.14");
        // .NET 10 (`sa-core\b2b\b2b.cs`, "FMT"): exact value, ties to even.
        assert_eq!(render_value(&Value::Float(12.345), Some("P0")), "1,235 %");
        assert_eq!(render_value(&Value::Float(0.875), Some("P0")), "88 %");
        assert_eq!(
            render_value(&Value::Float(-1234.56), Some("N1")),
            "-1,234.6"
        );
        assert_eq!(render_value(&Value::Float(-0.00001), Some("P2")), "-0.00 %");
        assert_eq!(render_value(&Value::Float(2.5), Some("F0")), "2");
        assert_eq!(render_value(&Value::Float(0.05), Some("0.0")), "0.1");
        assert_eq!(render_value(&Value::Int(1), Some("P0")), "100 %");
        assert_eq!(
            render_value(&Value::Decimal("2.345".into()), Some("F2")),
            "2.35"
        );
        for (v, s) in [
            (1e15, "1000000000000000"),
            (1e16, "10000000000000000"),
            (0.0001, "0.0001"),
            (1e-5, "1E-05"),
            (1e20, "1E+20"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(render_value(&Value::Float(v), None), s);
        }
        assert_eq!(
            render_value(
                &Value::Seq(vec![1.into(), "a".into(), Value::Null, true.into()]),
                None
            ),
            "[1, \"a\", null, True]"
        );
        assert_eq!(
            render_value(
                &Value::Dict(vec![("k".into(), "v".into()), (2.into(), 3.into())]),
                None
            ),
            "[(\"k\": \"v\"), (2: 3)]"
        );
        assert_eq!(render_value(&Value::Structure(vec![]), None), "{  }");
        let t = at("2026-11-04T10:00:00.12+07:00");
        assert_eq!(
            render_value(&Value::from(t), Some("O")),
            "2026-11-04T10:00:00.1200000+07:00"
        );
    }

    /// A line with Vietnamese text, an exception and properties; expected: the C#
    /// `JsonLogFormatter.Render` on .NET 10 (`%TEMP%\kx\sa-core\b2b\b2b.cs`, "LOGVN").
    #[test]
    fn vietnamese_line_with_exception_matches_the_csharp_formatter() {
        let mut e = event(
            at("2026-11-04T10:15:30.9876543+07:00"),
            Level::Error,
            "Đồng bộ bảng {Table} thất bại với {Code} sau {Attempts} lần ({Score:P0}, {Ratio}), chờ đến {Next:O}; khách {Guest}",
            vec![
                ("Table", "HóaĐơn".into()),
                ("Code", "SQL_BUSY".into()),
                ("Attempts", 3.into()),
                ("Score", 0.875.into()),
                ("Ratio", 12.5.into()),
                ("Next", at("2026-11-04T10:16:00+07:00").into()),
                ("Guest", "Nguyễn Văn A <vip> & co".into()),
                (
                    "SourceContext",
                    "MM.PdaOrder.SyncAgent.Capture.SqlCaptureRunner".into(),
                ),
                ("source", "agent".into()),
                ("run_id", "agent-20261104T031530Z-0a1b2c".into()),
                ("agent_id", "5f0c1b9e-0000-4000-8000-000000000001".into()),
                ("tenant", "tenant-1".into()),
            ],
        );
        e.exception = Some(
            Exception::new(
                "System.InvalidOperationException",
                "Không kết nối được \"SMILE_POS\" Password=bí-mật;",
            )
            .with_inner(Exception::new(
                "System.TimeoutException",
                "Hết thời gian chờ",
            )),
        );
        let nl = crate::util::stj::NEWLINE
            .replace('\r', "\\r")
            .replace('\n', "\\n");
        assert_eq!(
            render(&e),
            format!(
                r#"{{"ts":"2026-11-04T03:15:30.987Z","level":"error","source":"agent","run_id":"agent-20261104T031530Z-0a1b2c","agent_id":"5f0c1b9e-0000-4000-8000-000000000001","tenant":"tenant-1","code":"SQL_BUSY","msg":"Đồng bộ bảng HóaĐơn thất bại với SQL_BUSY sau 3 lần (88 %, 12.5), chờ đến 2026-11-04T10:16:00.0000000+07:00; khách Nguyễn Văn A <vip> & co","exception":"System.InvalidOperationException: Không kết nối được \"SMILE_POS\" Password=***;{nl} ---> System.TimeoutException: Hết thời gian chờ{nl}   --- End of inner exception stack trace ---","table":"HóaĐơn","attempts":3,"score":0.875,"ratio":12.5,"next":"2026-11-04T03:16:00.0000000+00:00","guest":"Nguyễn Văn A <vip> & co","logger":"MM.PdaOrder.SyncAgent.Capture.SqlCaptureRunner"}}"#
            )
        );
        let line = format_line(&e);
        assert!(line.ends_with('\n') && !line.contains('\r') && line.matches('\n').count() == 1);
    }

    /// Expected lines: .NET 10 + Serilog (`%TEMP%\kx\sa-core\check.cs`, "log" section).
    #[test]
    #[allow(clippy::approx_constant)]
    fn lines_match_the_csharp_formatter() {
        let nl = crate::util::stj::NEWLINE;
        let mut e = event(
            at("2026-11-04T10:00:00.1234567+07:00"),
            Level::Warning,
            "Đồng bộ {StreamName} lỗi {Code} sau {Rows} dòng, tỉ lệ {Ratio} {Ok} {At} {Missing} {Nothing} {Delay} {{lit}} {Big} {@Obj} {Id,8}|{Id,-8}|",
            vec![
                ("source", "agent".into()),
                ("run_id", "agent-20261104T030000Z-abc123".into()),
                ("agent_id", "5f0c1b9e-0000-4000-8000-000000000001".into()),
                ("tenant", "tenant-1".into()),
                ("StreamName", "hóa_đơn \"x\" <b> \\ \u{2028}".into()),
                ("Code", "SQL_BUSY".into()),
                ("Rows", 42.into()),
                ("Ratio", 0.25.into()),
                ("Ok", true.into()),
                ("At", at("2026-11-04T10:00:00.12+07:00").into()),
                ("Nothing", Value::Null),
                ("Delay", Value::TimeSpan(15_000_000)),
                ("Big", 12345678901234567890u64.into()),
                (
                    "Obj",
                    Value::Structure(vec![
                        ("InnerName".into(), 1.into()),
                        ("b".into(), Value::Seq(vec!["s".into(), 2.5.into()])),
                    ]),
                ),
                ("Id", "ab".into()),
                (
                    "SourceContext",
                    "MM.PdaOrder.SyncAgent.Capture.CaptureRunner".into(),
                ),
                ("HTTPStatus", 503.into()),
                ("msg", "collides".into()),
                ("prop_msg", "collides twice".into()),
                ("detail", Value::Raw("{\"rows\":3,\"s\":\"é<\"}".into())),
            ],
        );
        assert_eq!(
            render(&e),
            r#"{"ts":"2026-11-04T03:00:00.123Z","level":"warn","source":"agent","run_id":"agent-20261104T030000Z-abc123","agent_id":"5f0c1b9e-0000-4000-8000-000000000001","tenant":"tenant-1","code":"SQL_BUSY","msg":"Đồng bộ hóa_đơn \"x\" <b> \\ \u2028 lỗi SQL_BUSY sau 42 dòng, tỉ lệ 0.25 True 11/04/2026 10:00:00 +07:00 {Missing} null 00:00:01.5000000 {lit} 12345678901234567890 { InnerName: 1, b: [\"s\", 2.5] } ab|ab|","stream_name":"hóa_đơn \"x\" <b> \\ \u2028","rows":42,"ratio":0.25,"ok":true,"at":"2026-11-04T03:00:00.1200000+00:00","nothing":null,"delay":"00:00:01.5000000","big":12345678901234567890,"obj":{"inner_name":1,"b":["s",2.5]},"id":"ab","logger":"MM.PdaOrder.SyncAgent.Capture.CaptureRunner","http_status":503,"prop_msg":"collides","prop_prop_msg":"collides twice","detail":{"rows":3,"s":"é<"}}"#
        );
        e = event(
            at("2026-11-04T03:00:00.005Z"),
            Level::Error,
            "Plain {Code} {RunId}",
            vec![
                ("Code", "not a code".into()),
                ("RunId", 7.into()),
                ("run_id", Value::Null),
                ("TenantId", "t2".into()),
            ],
        );
        e.exception = Some(
            Exception::new("System.InvalidOperationException", "Lỗi \"x\"")
                .with_inner(Exception::new("System.TimeoutException", "bên trong")),
        );
        let nl_json = nl.replace('\r', "\\r").replace('\n', "\\n");
        assert_eq!(
            render(&e),
            format!(
                r#"{{"ts":"2026-11-04T03:00:00.005Z","level":"error","run_id":"7","tenant":"t2","msg":"Plain not a code 7","exception":"System.InvalidOperationException: Lỗi \"x\"{nl_json} ---> System.TimeoutException: bên trong{nl_json}   --- End of inner exception stack trace ---","prop_code":"not a code","prop_run_id":null}}"#
            )
        );
        e = event(
            at("2026-11-04T03:00:00Z"),
            Level::Debug,
            "{A}{B}{C}{D}{E} {F:0.00} {bad name} {G",
            vec![
                ("A", 1.0.into()),
                ("B", 1e20.into()),
                ("C", (-0.000012).into()),
                ("D", Value::TimeSpan(-1_296_000_000_000)),
                (
                    "E",
                    Value::DateTimeUtc(at("2026-01-02T03:04:05Z").unix_nanos()),
                ),
                ("F", 3.14159.into()),
                ("SomeCamelCaseURLValue", 1.into()),
                ("already_snake", 2.into()),
                ("lower", 3.into()),
                ("X", 4.into()),
                ("Dict", Value::Dict(vec![("k".into(), 1.into())])),
            ],
        );
        assert_eq!(
            render(&e),
            r#"{"ts":"2026-11-04T03:00:00.000Z","level":"debug","msg":"11E+20-1.2E-05-1.12:00:0001/02/2026 03:04:05 3.14 {bad name} {G","a":1,"b":1E+20,"c":-1.2E-05,"d":"-1.12:00:00","e":"2026-01-02T03:04:05.0000000Z","f":3.14159,"some_camel_case_url_value":1,"already_snake":2,"lower":3,"x":4,"dict":{"k":1}}"#
        );
        e = event(
            at("2026-11-04T03:00:00Z"),
            Level::Information,
            "{A} {B} {C} {D} {E} {F} {G:P0} {H:P1} {I:O} {J} {K} {L} {M} {N} {O} {P:P0} {Q} {R}",
            vec![
                ("A", 1e15.into()),
                ("B", 1e16.into()),
                ("C", 123456789012345.6.into()),
                ("D", 0.0001.into()),
                ("E", 0.00001.into()),
                ("F", 1e14.into()),
                ("G", 0.853.into()),
                ("H", 0.12345.into()),
                ("I", at("2026-11-04T10:00:00.12+07:00").into()),
                (
                    "J",
                    Value::Seq(vec![1.into(), "a".into(), Value::Null, true.into()]),
                ),
                (
                    "K",
                    Value::Dict(vec![("k".into(), "v".into()), (2.into(), 3.into())]),
                ),
                ("L", Value::Structure(vec![])),
                (
                    "M",
                    dl_uuid::Uuid::parse("5F0C1B9E-0000-4000-8000-000000000001")
                        .unwrap()
                        .into(),
                ),
                ("N", f64::NAN.into()),
                ("O", f64::NEG_INFINITY.into()),
                ("P", 0.125.into()),
                ("Q", Value::Decimal("1.5".into())),
                ("R", Value::TimeSpan(123_456_789_012_345)),
            ],
        );
        assert_eq!(
            render(&e),
            r#"{"ts":"2026-11-04T03:00:00.000Z","level":"info","msg":"1000000000000000 10000000000000000 123456789012345.6 0.0001 1E-05 100000000000000 85 % 12.3 % 2026-11-04T10:00:00.1200000+07:00 [1, \"a\", null, True] [(\"k\": \"v\"), (2: 3)] {  } 5f0c1b9e-0000-4000-8000-000000000001 NaN -Infinity 12 % 1.5 142.21:21:18.9012345","a":1000000000000000,"b":10000000000000000,"c":123456789012345.6,"d":0.0001,"e":1E-05,"f":100000000000000,"g":0.853,"h":0.12345,"i":"2026-11-04T03:00:00.1200000+00:00","j":[1,"a",null,true],"k":{"k":"v","2":3},"l":{},"m":"5f0c1b9e-0000-4000-8000-000000000001","n":"NaN","o":"-Infinity","p":0.125,"q":1.5,"r":"142.21:21:18.9012345"}"#
        );
    }
}

/// Ports of `JsonLogFormatterTests.cs`, through the logger as the C# tests go through
/// Serilog.
#[cfg(test)]
mod formatter_tests {
    use super::*;
    use crate::reporting::log::{Exception, Logger, LoggerConfig, MemorySink, Property};
    use std::sync::Arc;

    fn line(write: impl FnOnce(&Logger)) -> dl_json::Value {
        let sink = Arc::new(MemorySink::default());
        let log = Logger::new(LoggerConfig {
            minimum: Level::Debug,
            enrich: vec![
                Property::new("source", "agent"),
                Property::new("run_id", "agent-20261113T080000Z-abc123"),
                Property::new("agent_id", "5f0c1b9e-0000-4000-8000-000000000001"),
                Property::new("tenant", "tenant-1"),
            ],
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        write(&log);
        let events = sink.events();
        assert_eq!(events.len(), 1);
        let text = format_line(&events[0]);
        assert!(text.ends_with('\n') && !text.contains('\r'));
        assert!(
            !text.trim_end_matches('\n').contains('\n'),
            "one event is one line"
        );
        crate::util::stj::parse(text.trim_end()).unwrap()
    }

    fn s<'a>(v: &'a dl_json::Value, k: &str) -> Option<&'a str> {
        v.get(k).and_then(dl_json::Value::as_str)
    }

    #[test]
    fn writes_the_fixed_fields_then_other_properties_in_snake_case() {
        let l = line(|log| {
            log.for_category("MM.PdaOrder.SyncAgent.Capture.CaptureRunner")
                .error_ex(
                    Exception::new("System.InvalidOperationException", "boom"),
                    "Capture of {StreamName} stopped with {Code} after {Rows} rows",
                    &["orders".into(), "SQL_BUSY".into(), 42.into()],
                )
        });
        let ts = s(&l, "ts").unwrap();
        assert_eq!(ts.len(), 24);
        assert!(ts.ends_with('Z') && ts.as_bytes()[19] == b'.');
        assert_eq!(s(&l, "level"), Some("error"));
        assert_eq!(s(&l, "source"), Some("agent"));
        assert_eq!(s(&l, "run_id"), Some("agent-20261113T080000Z-abc123"));
        assert_eq!(
            s(&l, "agent_id"),
            Some("5f0c1b9e-0000-4000-8000-000000000001")
        );
        assert_eq!(s(&l, "tenant"), Some("tenant-1"));
        assert_eq!(s(&l, "code"), Some("SQL_BUSY"));
        assert_eq!(
            s(&l, "msg"),
            Some("Capture of orders stopped with SQL_BUSY after 42 rows")
        );
        assert!(
            s(&l, "exception")
                .unwrap()
                .contains("InvalidOperationException: boom")
        );
        assert_eq!(s(&l, "stream_name"), Some("orders"));
        assert_eq!(l.get("rows").and_then(dl_json::Value::as_i64), Some(42));
        assert_eq!(
            s(&l, "logger"),
            Some("MM.PdaOrder.SyncAgent.Capture.CaptureRunner")
        );
        assert!(
            l.get("Code").is_none(),
            "a lifted property is not written twice"
        );
    }

    #[test]
    fn levels_use_the_site_event_names() {
        for (level, name) in [
            (Level::Debug, "debug"),
            (Level::Information, "info"),
            (Level::Warning, "warn"),
            (Level::Fatal, "error"),
        ] {
            assert_eq!(level_name(level), name);
        }
    }

    #[test]
    fn a_property_that_is_not_a_code_does_not_become_the_code_field() {
        let l = line(|log| log.info("Mode {Code}", &["not a code".into()]));
        assert!(l.get("code").is_none());
        assert_eq!(s(&l, "prop_code"), Some("not a code"));
    }

    #[test]
    fn secrets_never_reach_the_line() {
        let token = format!("tok-{}", dl_uuid::Uuid::new_v4().simple());
        crate::reporting::scrubber::register(Some(&token));
        let l = line(|log| {
            log.warn_ex(
                Exception::new(
                    "System.InvalidOperationException",
                    format!("Password=hunter22; token {token}"),
                ),
                "Connecting with {ConnectionString} and {Token}",
                &["Server=.;Password=hunter22;".into(), token.as_str().into()],
            )
        });
        let mut w = crate::util::stj::Writer::new(crate::util::stj::Style::Compact);
        w.value(&l);
        let text = w.finish();
        assert!(
            !text.contains("hunter22") && !text.contains(&token),
            "{text}"
        );
        assert_eq!(s(&l, "token"), Some(crate::reporting::scrubber::MASK));
    }

    #[test]
    fn raw_json_detail_is_written_as_an_object() {
        let mut e = Event::new(Timestamp::now_utc(), Level::Information, "x");
        e.add_or_update(Property::new("detail", Value::Raw("{\"rows\":3}".into())));
        let l = crate::util::stj::parse(&render(&e)).unwrap();
        assert_eq!(
            l.get("detail")
                .and_then(|d| d.get("rows"))
                .and_then(dl_json::Value::as_i64),
            Some(3)
        );
    }
}
