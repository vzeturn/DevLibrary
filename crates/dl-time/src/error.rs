use core::fmt;

/// Error returned by constructors, arithmetic, parsers and formatters in this crate.
///
/// Byte offsets in [`Error::Parse`] and [`Error::InvalidPattern`] are zero-based offsets into
/// the input string and the pattern string respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Error {
    /// A component is outside its valid range, for example month 13, February 30,
    /// second 60 or an offset beyond ±18:00.
    InvalidComponent {
        /// Name of the component: `"year"`, `"month"`, `"day"`, `"hour"`, `"minute"`,
        /// `"second"`, `"nanosecond"`, `"ordinal"`, `"week"`, `"offset"`, ...
        name: &'static str,
        /// The rejected value.
        value: i64,
    },
    /// The input does not match the expected syntax.
    Parse {
        /// Byte offset in the input where parsing failed.
        offset: usize,
        /// What the parser expected at that offset.
        expected: &'static str,
    },
    /// A format pattern is malformed or uses a field the value does not have
    /// (for example `HH` on a [`Date`](crate::Date)).
    InvalidPattern {
        /// Byte offset in the pattern.
        offset: usize,
        /// Why the pattern was rejected.
        reason: &'static str,
    },
    /// The result is outside the supported range (years -9999..=9999 for dates,
    /// ±(`i64::MAX` seconds + 999 999 999 ns) for durations).
    Overflow,
}

impl Error {
    pub(crate) const fn component(name: &'static str, value: i64) -> Self {
        Error::InvalidComponent { name, value }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::InvalidComponent { name, value } => write!(f, "invalid {name}: {value}"),
            Error::Parse { offset, expected } => {
                write!(f, "parse error at byte {offset}: expected {expected}")
            }
            Error::InvalidPattern { offset, reason } => {
                write!(f, "invalid format pattern at byte {offset}: {reason}")
            }
            Error::Overflow => f.write_str("value out of supported range"),
        }
    }
}

impl core::error::Error for Error {}
