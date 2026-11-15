//! Human-readable text lines.

use std::fmt::Write as _;

use super::{
    ANSI_DIM, ANSI_RESET, Format, level_color, write_escaped_message, write_text_key,
    write_text_value, write_timestamp,
};
use crate::{Level, Record};

/// Human-readable lines for terminals and plain log files:
///
/// ```text
/// 2026-11-15T10:20:30.123+07:00 INFO  booking confirmed booking_id=42 nights=3 guest="Nguyễn Văn An"
/// ```
///
/// The line is the timestamp (RFC 3339, milliseconds, the logger's offset), the level
/// padded to five characters, optionally the target and source location, the message,
/// then `key=value` fields. String values are bare when they contain no whitespace,
/// quotes, `=` or `\`, otherwise quoted with escapes; arrays and objects are compact
/// JSON. Control characters in the message (other than tab) are escaped as `\n`, `\r`
/// or `\u{1b}`, so one record is always one line and log data cannot inject terminal
/// escape sequences.
///
/// ```
/// use dl_log::{Format, Level, Record, TextFormat};
/// use dl_time::OffsetDateTime;
///
/// let time = OffsetDateTime::parse_rfc3339("2026-11-15T03:20:30.5Z")?;
/// let record = Record::new(time, Level::Warn, "booking::api", "slow query\nretrying")
///     .with_field("ms", 1830)
///     .with_location("src/api.rs", 88);
///
/// let full = TextFormat::new().with_target(true).with_location(true);
/// assert_eq!(
///     full.format_to_string(&record),
///     "2026-11-15T03:20:30.500Z WARN  booking::api src/api.rs:88: slow query\\nretrying ms=1830"
/// );
/// // For journald / docker, which add their own timestamps:
/// let bare = TextFormat::new().with_time(false);
/// assert_eq!(bare.format_to_string(&record), "WARN  slow query\\nretrying ms=1830");
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextFormat {
    time: bool,
    target: bool,
    location: bool,
    ansi: bool,
}

impl Default for TextFormat {
    fn default() -> Self {
        TextFormat::new()
    }
}

impl TextFormat {
    /// Timestamp, level, message and fields; no target, location or colors.
    pub const fn new() -> TextFormat {
        TextFormat {
            time: true,
            target: false,
            location: false,
            ansi: false,
        }
    }

    /// Writes the timestamp (default `true`).
    pub const fn with_time(mut self, yes: bool) -> TextFormat {
        self.time = yes;
        self
    }

    /// Writes the target before the message (default `false`).
    pub const fn with_target(mut self, yes: bool) -> TextFormat {
        self.target = yes;
        self
    }

    /// Writes `file:line` before the message when known (default `false`).
    pub const fn with_location(mut self, yes: bool) -> TextFormat {
        self.location = yes;
        self
    }

    /// Colors the level and dims the timestamp and field names with ANSI escape
    /// sequences (default `false`). Enable it only for terminals, for example with
    /// `std::io::IsTerminal::is_terminal(&std::io::stdout())`.
    pub const fn with_ansi(mut self, yes: bool) -> TextFormat {
        self.ansi = yes;
        self
    }
}

impl Format for TextFormat {
    fn format(&self, record: &Record, out: &mut String) {
        if self.time {
            if self.ansi {
                out.push_str(ANSI_DIM);
            }
            write_timestamp(out, record.time);
            if self.ansi {
                out.push_str(ANSI_RESET);
            }
            out.push(' ');
        }
        if self.ansi {
            out.push_str(level_color(record.level));
        }
        out.push_str(match record.level {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        });
        if self.ansi {
            out.push_str(ANSI_RESET);
        }
        out.push(' ');
        let location = match (self.location, record.file) {
            (true, Some(file)) => Some((file, record.line)),
            _ => None,
        };
        if self.target || location.is_some() {
            if self.target {
                write_escaped_message(out, &record.target);
            }
            if let Some((file, line)) = location {
                if self.target {
                    out.push(' ');
                }
                write_escaped_message(out, file);
                if let Some(line) = line {
                    let _ = write!(out, ":{line}");
                }
            }
            out.push_str(": ");
        }
        write_escaped_message(out, &record.message);
        for (key, value) in record.all_fields() {
            out.push(' ');
            if self.ansi {
                out.push_str(ANSI_DIM);
                write_text_key(out, key);
                out.push('=');
                out.push_str(ANSI_RESET);
            } else {
                write_text_key(out, key);
                out.push('=');
            }
            write_text_value(out, value, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_json::json;
    use dl_time::OffsetDateTime;

    fn record() -> Record {
        let time = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.123+07:00").unwrap();
        Record::new(time, Level::Info, "booking", "booking confirmed")
            .with_field("booking_id", 42)
            .with_field("nights", 3)
    }

    #[test]
    fn exact_lines() {
        let f = TextFormat::new();
        assert_eq!(
            f.format_to_string(&record()),
            "2026-11-15T10:20:30.123+07:00 INFO  booking confirmed booking_id=42 nights=3"
        );
        let r = record()
            .with_field("guest", "Trần Thị Bình")
            .with_field("rooms", json!([1205, 1206]))
            .with_field("note", "")
            .with_field("paid", false);
        assert_eq!(
            f.with_time(false).format_to_string(&r),
            "INFO  booking confirmed booking_id=42 nights=3 guest=\"Trần Thị Bình\" rooms=[1205,1206] note=\"\" paid=false"
        );
        let mut r = record().with_location("src/booking.rs", 7);
        r.level = Level::Error;
        r.fields.clear();
        assert_eq!(
            f.with_time(false).with_target(true).format_to_string(&r),
            "ERROR booking: booking confirmed"
        );
        assert_eq!(
            f.with_time(false).with_location(true).format_to_string(&r),
            "ERROR src/booking.rs:7: booking confirmed"
        );
        // Location requested but unknown: nothing extra.
        r.file = None;
        assert_eq!(
            f.with_time(false).with_location(true).format_to_string(&r),
            "ERROR booking confirmed"
        );
    }

    #[test]
    fn ansi_only_when_enabled() {
        let plain = TextFormat::new().format_to_string(&record());
        assert!(!plain.contains('\x1b'));
        let colored = TextFormat::new()
            .with_ansi(true)
            .format_to_string(&record());
        assert_eq!(
            colored,
            "\x1b[2m2026-11-15T10:20:30.123+07:00\x1b[0m \x1b[32mINFO \x1b[0m booking confirmed \
             \x1b[2mbooking_id=\x1b[0m42 \x1b[2mnights=\x1b[0m3"
        );
        assert_eq!(TextFormat::default(), TextFormat::new());
    }

    #[test]
    fn message_injection_is_escaped() {
        let mut r = record();
        r.message =
            "user logged in\n2026-11-15T10:20:31.000+07:00 INFO  admin granted\u{1b}[0m".into();
        r.fields.clear();
        let line = TextFormat::new().with_time(false).format_to_string(&r);
        assert_eq!(
            line,
            "INFO  user logged in\\n2026-11-15T10:20:31.000+07:00 INFO  admin granted\\u{1b}[0m"
        );
        assert!(!line.contains('\n'));
    }
}
