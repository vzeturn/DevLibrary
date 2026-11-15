//! One JSON object per line, plain or CLEF.

use std::fmt::Write as _;

use super::{Format, event_id, is_plain_template, write_json_str, write_timestamp};
use crate::{Level, Record};

/// Key names used by [`JsonFormat`] in plain mode.
///
/// The defaults are `time`, `level`, `target`, `message`, `file` and `line`. For the
/// Elastic Common Schema use `JsonKeys { time: "@timestamp", level: "log.level",
/// target: "log.logger", ..JsonKeys::default() }`; for pino-style output use
/// `message: "msg"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonKeys {
    /// Timestamp key.
    pub time: &'static str,
    /// Level key.
    pub level: &'static str,
    /// Target key.
    pub target: &'static str,
    /// Message key.
    pub message: &'static str,
    /// Source file key (only written with [`JsonFormat::with_location`]).
    pub file: &'static str,
    /// Source line key (only written with [`JsonFormat::with_location`]).
    pub line: &'static str,
}

impl Default for JsonKeys {
    fn default() -> Self {
        JsonKeys {
            time: "time",
            level: "level",
            target: "target",
            message: "message",
            file: "file",
            line: "line",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    Plain,
    Clef,
}

/// One JSON object per line (NDJSON), for log shippers such as Loki, Elasticsearch,
/// Vector, Fluent Bit or Seq.
///
/// **Plain** ([`JsonFormat::new`]):
///
/// ```text
/// {"time":"2026-11-15T10:20:30.123+07:00","level":"INFO","target":"booking","message":"booking confirmed","booking_id":42,"nights":3}
/// ```
///
/// Fields are written at the top level after the fixed keys. A field whose name equals
/// one of the fixed keys in use is written as `fields.<name>`, so every object has
/// unique keys.
///
/// **CLEF** ([`JsonFormat::clef`]), the [Compact Log Event Format](https://clef-json.org/)
/// written by Serilog's `CompactJsonFormatter` and read by Seq and Serilog tooling:
///
/// - `@t`: the timestamp (ISO 8601);
/// - `@mt`: the message template, when the message was logged from a string literal
///   without arguments (`info!("booking confirmed"; booking_id = 42)`);
/// - otherwise `@m`, the rendered message, plus `@i`, an event type id computed from the
///   format string (the same 32-bit Jenkins hash Serilog uses, 8 hex digits), so Seq
///   groups `info!("user {} logged in", u)` events together;
/// - `@l`: the Serilog level name (`Warning`, …), omitted for `Information` as the spec
///   allows;
/// - `SourceContext`: the target, Serilog's name for it;
/// - fields as top-level properties; a name starting with `@` is escaped as `@@`.
///
/// ```text
/// {"@t":"2026-11-15T10:20:30.123+07:00","@mt":"booking confirmed","SourceContext":"booking","booking_id":42}
/// {"@t":"2026-11-15T10:20:31.000+07:00","@m":"user An logged in","@i":"34e021cb","@l":"Warning","SourceContext":"auth"}
/// ```
///
/// Strings are escaped like `dl_json` (quotes, backslashes, control characters and
/// U+2028/U+2029); all other text, including Vietnamese, is written as UTF-8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonFormat {
    style: Style,
    keys: JsonKeys,
    target: bool,
    location: bool,
}

impl Default for JsonFormat {
    fn default() -> Self {
        JsonFormat::new()
    }
}

impl JsonFormat {
    /// Plain JSON with the default [`JsonKeys`], including the target.
    pub fn new() -> JsonFormat {
        JsonFormat {
            style: Style::Plain,
            keys: JsonKeys::default(),
            target: true,
            location: false,
        }
    }

    /// CLEF output (Serilog compact JSON), including `SourceContext`.
    pub fn clef() -> JsonFormat {
        JsonFormat {
            style: Style::Clef,
            ..JsonFormat::new()
        }
    }

    /// Writes the target (`target` / `SourceContext`, default `true`).
    pub fn with_target(mut self, yes: bool) -> JsonFormat {
        self.target = yes;
        self
    }

    /// Writes the source file and line when known (default `false`), under the
    /// [`JsonKeys::file`] and [`JsonKeys::line`] keys in both modes.
    pub fn with_location(mut self, yes: bool) -> JsonFormat {
        self.location = yes;
        self
    }

    /// Renames the fixed keys in plain mode. CLEF uses its own names and only takes the
    /// `file` and `line` keys from here.
    pub fn with_keys(mut self, keys: JsonKeys) -> JsonFormat {
        self.keys = keys;
        self
    }

    fn is_reserved(&self, key: &str, record: &Record) -> bool {
        let location = self.location && record.file.is_some();
        let k = &self.keys;
        let common = (location && (key == k.file || (record.line.is_some() && key == k.line)))
            || (self.target && key == self.target_key());
        match self.style {
            Style::Plain => common || key == k.time || key == k.level || key == k.message,
            Style::Clef => common,
        }
    }

    fn target_key(&self) -> &'static str {
        match self.style {
            Style::Plain => self.keys.target,
            Style::Clef => "SourceContext",
        }
    }
}

fn key(out: &mut String, name: &str) {
    write_json_str(out, name);
    out.push(':');
}

impl Format for JsonFormat {
    fn format(&self, record: &Record, out: &mut String) {
        out.push('{');
        match self.style {
            Style::Plain => {
                key(out, self.keys.time);
                out.push('"');
                write_timestamp(out, record.time);
                out.push_str("\",");
                key(out, self.keys.level);
                out.push('"');
                out.push_str(record.level.as_str());
                out.push_str("\",");
                if self.target {
                    key(out, self.keys.target);
                    write_json_str(out, &record.target);
                    out.push(',');
                }
                key(out, self.keys.message);
                write_json_str(out, &record.message);
            }
            Style::Clef => {
                out.push_str("\"@t\":\"");
                write_timestamp(out, record.time);
                out.push('"');
                match record.template {
                    Some(t) if is_plain_template(t) => {
                        out.push_str(",\"@mt\":");
                        write_json_str(out, t);
                    }
                    template => {
                        out.push_str(",\"@m\":");
                        write_json_str(out, &record.message);
                        if let Some(t) = template {
                            let _ = write!(out, ",\"@i\":\"{:08x}\"", event_id(t));
                        }
                    }
                }
                if record.level != Level::Info {
                    out.push_str(",\"@l\":\"");
                    out.push_str(record.level.serilog_name());
                    out.push('"');
                }
                if self.target {
                    out.push_str(",\"SourceContext\":");
                    write_json_str(out, &record.target);
                }
            }
        }
        for (name, value) in record.all_fields() {
            out.push(',');
            if self.style == Style::Clef && name.starts_with('@') {
                out.push_str("\"@");
                // Reuse the escaper for the rest of the name, without its opening quote.
                let start = out.len();
                write_json_str(out, name);
                out.remove(start);
                out.push(':');
            } else if self.is_reserved(name, record) {
                out.push_str("\"fields.");
                let start = out.len();
                write_json_str(out, name);
                out.remove(start);
                out.push(':');
            } else {
                key(out, name);
            }
            dl_json::write_to(value, out);
        }
        if self.location
            && let Some(file) = record.file
        {
            out.push(',');
            key(out, self.keys.file);
            write_json_str(out, file);
            if let Some(line) = record.line {
                out.push(',');
                key(out, self.keys.line);
                let _ = write!(out, "{line}");
            }
        }
        out.push('}');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::{Value, json};
    use dl_time::OffsetDateTime;

    fn time() -> OffsetDateTime {
        OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.123+07:00").unwrap()
    }

    fn record() -> Record {
        Record::new(time(), Level::Info, "booking", "booking confirmed")
            .with_template("booking confirmed")
            .with_field("booking_id", 42)
            .with_field("nights", 3)
    }

    #[test]
    fn plain_lines() {
        assert_eq!(
            JsonFormat::new().format_to_string(&record()),
            r#"{"time":"2026-11-15T10:20:30.123+07:00","level":"INFO","target":"booking","message":"booking confirmed","booking_id":42,"nights":3}"#
        );
        let r = record()
            .with_location("src/main.rs", 12)
            .with_field("message", "dup")
            .with_field("line", 1);
        assert_eq!(
            JsonFormat::new()
                .with_target(false)
                .with_location(true)
                .format_to_string(&r),
            r#"{"time":"2026-11-15T10:20:30.123+07:00","level":"INFO","message":"booking confirmed","booking_id":42,"nights":3,"fields.message":"dup","fields.line":1,"file":"src/main.rs","line":12}"#
        );
        // `line` is only reserved when it is written.
        let r = record().with_field("line", 1).with_field("target", "x");
        let line = JsonFormat::new().with_target(false).format_to_string(&r);
        assert!(
            line.ends_with(r#""nights":3,"line":1,"target":"x"}"#),
            "{line}"
        );
    }

    #[test]
    fn custom_keys() {
        let ecs = JsonKeys {
            time: "@timestamp",
            level: "log.level",
            target: "log.logger",
            ..JsonKeys::default()
        };
        let line = JsonFormat::new().with_keys(ecs).format_to_string(&record());
        assert!(line.starts_with(r#"{"@timestamp":"2026-11-15T10:20:30.123+07:00","log.level":"INFO","log.logger":"booking","message":"#));
    }

    #[test]
    fn clef_lines() {
        let clef = JsonFormat::clef();
        assert_eq!(
            clef.format_to_string(&record()),
            r#"{"@t":"2026-11-15T10:20:30.123+07:00","@mt":"booking confirmed","SourceContext":"booking","booking_id":42,"nights":3}"#
        );
        let mut r = Record::new(time(), Level::Warn, "auth", "user An logged in")
            .with_template("user {} logged in")
            .with_field("@t", "user field")
            .with_field("SourceContext", "dup");
        let id = format!("{:08x}", event_id("user {} logged in"));
        assert_eq!(
            clef.format_to_string(&r),
            format!(
                r#"{{"@t":"2026-11-15T10:20:30.123+07:00","@m":"user An logged in","@i":"{id}","@l":"Warning","SourceContext":"auth","@@t":"user field","fields.SourceContext":"dup"}}"#
            )
        );
        // No template at all: only @m. Braces in a plain template stay escaped.
        r.template = None;
        r.fields.clear();
        r.level = Level::Trace;
        assert_eq!(
            clef.clone().with_target(false).format_to_string(&r),
            r#"{"@t":"2026-11-15T10:20:30.123+07:00","@m":"user An logged in","@l":"Verbose"}"#
        );
        r.template = Some("set {{x}}");
        r.message = "set {x}".into();
        assert!(clef.format_to_string(&r).contains(r#""@mt":"set {{x}}""#));
        for level in Level::ALL {
            r.level = level;
            let v = dl_json::parse(&clef.format_to_string(&r)).unwrap();
            let l = v.get("@l").and_then(Value::as_str);
            assert_eq!(l, (level != Level::Info).then(|| level.serilog_name()));
        }
    }

    #[test]
    fn escaping_round_trips_through_a_parser() {
        let message = "Khách \"VIP\" đặt phòng\nghi chú:\ttab \\ \u{2028} \u{1}";
        let r = Record::new(time(), Level::Error, "booking::api", message)
            .with_field("guest", "Nguyễn Văn An")
            .with_field("note", "line1\nline2 \"quoted\"")
            .with_field("khóa có dấu", json!({"a": [1, null, "ư"]}))
            .with_field("@weird\"key", 1);
        for format in [JsonFormat::new(), JsonFormat::clef()] {
            let line = format.format_to_string(&r);
            assert!(!line.contains('\n'), "{line}");
            let v = dl_json::parse(&line).unwrap();
            let msg_key = if format.style == Style::Plain {
                "message"
            } else {
                "@m"
            };
            assert_eq!(v[msg_key], message);
            assert_eq!(v["guest"], "Nguyễn Văn An");
            assert_eq!(v["note"], "line1\nline2 \"quoted\"");
            assert_eq!(v["khóa có dấu"], json!({"a": [1, null, "ư"]}));
            let weird = if format.style == Style::Plain {
                "@weird\"key"
            } else {
                "@@weird\"key"
            };
            assert_eq!(v[weird], 1);
        }
        let line = JsonFormat::new().format_to_string(&r);
        assert!(
            line.contains(r#""message":"Khách \"VIP\" đặt phòng\nghi chú:\ttab \\ \u2028 \u0001""#),
            "{line}"
        );
    }
}
