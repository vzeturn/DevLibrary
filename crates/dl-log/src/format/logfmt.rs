//! logfmt lines.

use std::fmt::Write as _;

use super::{Format, write_maybe_quoted, write_text_key, write_text_value, write_timestamp};
use crate::Record;

/// [logfmt](https://brandur.org/logfmt) lines, which Grafana Loki, Heroku and many
/// command-line tools parse without configuration:
///
/// ```text
/// time=2026-11-15T10:20:30.123+07:00 level=info target=booking msg="booking confirmed" booking_id=42 nights=3
/// ```
///
/// Values are bare when they contain no whitespace, quotes, `=` or `\`, otherwise
/// double-quoted with `\"`, `\\`, `\n`, `\r`, `\t` and `\u{…}` escapes. Arrays and
/// objects are written as quoted compact JSON. A field named `time`, `level`, `target`,
/// `msg`, `file` or `line` (when that key is written) becomes `fields.<name>`.
///
/// ```
/// use dl_log::{Format, Level, LogfmtFormat, Record};
/// use dl_time::OffsetDateTime;
///
/// let time = OffsetDateTime::parse_rfc3339("2026-11-15T03:20:30.123Z")?;
/// let record = Record::new(time, Level::Info, "booking", "booking confirmed")
///     .with_field("booking_id", 42)
///     .with_field("tags", dl_json::json!(["vip", "late"]));
/// assert_eq!(
///     LogfmtFormat::new().format_to_string(&record),
///     r#"time=2026-11-15T03:20:30.123Z level=info target=booking msg="booking confirmed" booking_id=42 tags="[\"vip\",\"late\"]""#
/// );
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogfmtFormat {
    time: bool,
    target: bool,
    location: bool,
}

impl Default for LogfmtFormat {
    fn default() -> Self {
        LogfmtFormat::new()
    }
}

impl LogfmtFormat {
    /// Time, level, target, message and fields.
    pub const fn new() -> LogfmtFormat {
        LogfmtFormat {
            time: true,
            target: true,
            location: false,
        }
    }

    /// Writes `time=` (default `true`).
    pub const fn with_time(mut self, yes: bool) -> LogfmtFormat {
        self.time = yes;
        self
    }

    /// Writes `target=` (default `true`).
    pub const fn with_target(mut self, yes: bool) -> LogfmtFormat {
        self.target = yes;
        self
    }

    /// Writes `file=` and `line=` when known (default `false`).
    pub const fn with_location(mut self, yes: bool) -> LogfmtFormat {
        self.location = yes;
        self
    }
}

impl Format for LogfmtFormat {
    fn format(&self, record: &Record, out: &mut String) {
        if self.time {
            out.push_str("time=");
            write_timestamp(out, record.time);
            out.push(' ');
        }
        out.push_str("level=");
        out.push_str(record.level.as_lower_str());
        if self.target {
            out.push_str(" target=");
            write_maybe_quoted(out, &record.target);
        }
        out.push_str(" msg=");
        write_maybe_quoted(out, &record.message);
        let location = self.location && record.file.is_some();
        for (key, value) in record.all_fields() {
            out.push(' ');
            let reserved = match key {
                "level" | "msg" => true,
                "time" => self.time,
                "target" => self.target,
                "file" => location,
                "line" => location && record.line.is_some(),
                _ => false,
            };
            if reserved {
                out.push_str("fields.");
            }
            write_text_key(out, key);
            out.push('=');
            write_text_value(out, value, true);
        }
        if location && let Some(file) = record.file {
            out.push_str(" file=");
            write_maybe_quoted(out, file);
            if let Some(line) = record.line {
                let _ = write!(out, " line={line}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Level;
    use dl_time::OffsetDateTime;

    #[test]
    fn exact_lines() {
        let time = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.123+07:00").unwrap();
        let r = Record::new(time, Level::Warn, "dl_http::server", "slow")
            .with_field("path", "/api/bookings?id=1")
            .with_field("msg", "dup")
            .with_field("guest", "Lê Văn Tám")
            .with_location("src/server.rs", 9);
        assert_eq!(
            LogfmtFormat::new().format_to_string(&r),
            r#"time=2026-11-15T10:20:30.123+07:00 level=warn target=dl_http::server msg=slow path="/api/bookings?id=1" fields.msg=dup guest="Lê Văn Tám""#
        );
        assert_eq!(
            LogfmtFormat::new()
                .with_time(false)
                .with_target(false)
                .with_location(true)
                .format_to_string(&r),
            r#"level=warn msg=slow path="/api/bookings?id=1" fields.msg=dup guest="Lê Văn Tám" file=src/server.rs line=9"#
        );
        assert_eq!(LogfmtFormat::default(), LogfmtFormat::new());
    }
}
