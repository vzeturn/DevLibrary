//! Severity levels.

use core::fmt;
use core::str::FromStr;

use crate::Error;

/// The severity of a log record, from the most verbose ([`Trace`](Level::Trace)) to the
/// most severe ([`Fatal`](Level::Fatal)).
///
/// Levels are ordered by severity: `Level::Trace < Level::Info < Level::Fatal`.
///
/// `FromStr` ignores ASCII case and accepts the Serilog and Microsoft.Extensions.Logging
/// names too: `verbose` (Trace), `information` (Info), `warning` (Warn) and `critical`
/// (Fatal). `Display` writes the upper-case name (`INFO`) and honours width and
/// alignment, so `{:<5}` pads it.
///
/// ```
/// use dl_log::Level;
///
/// assert_eq!("Information".parse::<Level>()?, Level::Info);
/// assert_eq!("WARNING".parse::<Level>()?, Level::Warn);
/// assert_eq!(format!("[{:<5}]", Level::Warn), "[WARN ]");
/// assert!(Level::Error > Level::Warn);
/// # Ok::<(), dl_log::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    /// Very detailed diagnostics, normally off (Serilog `Verbose`).
    Trace = 0,
    /// Diagnostics useful while developing.
    Debug = 1,
    /// Normal operation: requests served, jobs finished (Serilog `Information`).
    Info = 2,
    /// Something unexpected that the application recovered from (Serilog `Warning`).
    Warn = 3,
    /// An operation failed.
    Error = 4,
    /// The application or a component cannot continue (Microsoft `Critical`). Logging at
    /// this level does not exit the process.
    Fatal = 5,
}

impl Level {
    /// All levels, from `Trace` to `Fatal`.
    pub const ALL: [Level; 6] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];

    /// The upper-case name: `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`, `FATAL`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }

    /// The lower-case name used by logfmt: `trace` … `fatal`.
    pub const fn as_lower_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }

    /// The Serilog level name written to CLEF `@l`: `Verbose`, `Debug`, `Information`,
    /// `Warning`, `Error`, `Fatal`.
    pub const fn serilog_name(self) -> &'static str {
        match self {
            Level::Trace => "Verbose",
            Level::Debug => "Debug",
            Level::Info => "Information",
            Level::Warn => "Warning",
            Level::Error => "Error",
            Level::Fatal => "Fatal",
        }
    }

    pub(crate) const fn from_u8(n: u8) -> Option<Level> {
        match n {
            0 => Some(Level::Trace),
            1 => Some(Level::Debug),
            2 => Some(Level::Info),
            3 => Some(Level::Warn),
            4 => Some(Level::Error),
            5 => Some(Level::Fatal),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

fn parse_level_name(s: &str) -> Option<Level> {
    const NAMES: [(&str, Level); 10] = [
        ("trace", Level::Trace),
        ("verbose", Level::Trace),
        ("debug", Level::Debug),
        ("info", Level::Info),
        ("information", Level::Info),
        ("warn", Level::Warn),
        ("warning", Level::Warn),
        ("error", Level::Error),
        ("fatal", Level::Fatal),
        ("critical", Level::Fatal),
    ];
    NAMES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(s))
        .map(|&(_, level)| level)
}

impl FromStr for Level {
    type Err = Error;

    fn from_str(s: &str) -> Result<Level, Error> {
        parse_level_name(s).ok_or_else(|| Error::ParseLevel { input: s.into() })
    }
}

/// A minimum level, or [`Off`](LevelFilter::Off) to disable logging.
///
/// A record passes when its level is at or above the filter, like Serilog's
/// `MinimumLevel`: `LevelFilter::Warn` lets `Warn`, `Error` and `Fatal` through.
/// `FromStr` accepts every [`Level`] name plus `off` and `none`.
///
/// ```
/// use dl_log::{Level, LevelFilter};
///
/// let min = LevelFilter::Warn;
/// assert!(min.allows(Level::Error));
/// assert!(!min.allows(Level::Info));
/// assert!(!LevelFilter::Off.allows(Level::Fatal));
/// assert_eq!("off".parse::<LevelFilter>()?, LevelFilter::Off);
/// assert_eq!(LevelFilter::from(Level::Debug), LevelFilter::Debug);
/// # Ok::<(), dl_log::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum LevelFilter {
    /// Everything.
    Trace = 0,
    /// `Debug` and above.
    Debug = 1,
    /// `Info` and above (the default).
    #[default]
    Info = 2,
    /// `Warn` and above.
    Warn = 3,
    /// `Error` and `Fatal`.
    Error = 4,
    /// Only `Fatal`.
    Fatal = 5,
    /// Nothing.
    Off = 6,
}

impl LevelFilter {
    /// Returns `true` when records at `level` pass this filter.
    #[inline]
    pub const fn allows(self, level: Level) -> bool {
        level as u8 >= self as u8
    }

    /// The lower-case name: `trace` … `fatal`, `off`.
    pub const fn as_str(self) -> &'static str {
        match self.to_level() {
            Some(level) => level.as_lower_str(),
            None => "off",
        }
    }

    /// The most verbose level that passes, or `None` for `Off`.
    pub const fn to_level(self) -> Option<Level> {
        Level::from_u8(self as u8)
    }

    pub(crate) const fn from_u8(n: u8) -> LevelFilter {
        match n {
            0 => LevelFilter::Trace,
            1 => LevelFilter::Debug,
            2 => LevelFilter::Info,
            3 => LevelFilter::Warn,
            4 => LevelFilter::Error,
            5 => LevelFilter::Fatal,
            _ => LevelFilter::Off,
        }
    }
}

impl From<Level> for LevelFilter {
    fn from(level: Level) -> LevelFilter {
        LevelFilter::from_u8(level as u8)
    }
}

impl fmt::Display for LevelFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for LevelFilter {
    type Err = Error;

    fn from_str(s: &str) -> Result<LevelFilter, Error> {
        if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") {
            return Ok(LevelFilter::Off);
        }
        parse_level_name(s)
            .map(LevelFilter::from)
            .ok_or_else(|| Error::ParseLevel { input: s.into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_names_case_insensitively() {
        let cases = [
            ("trace", Level::Trace),
            ("TRACE", Level::Trace),
            ("Verbose", Level::Trace),
            ("debug", Level::Debug),
            ("Debug", Level::Debug),
            ("info", Level::Info),
            ("Information", Level::Info),
            ("INFORMATION", Level::Info),
            ("warn", Level::Warn),
            ("Warning", Level::Warn),
            ("error", Level::Error),
            ("ERROR", Level::Error),
            ("fatal", Level::Fatal),
            ("Critical", Level::Fatal),
        ];
        for (s, level) in cases {
            assert_eq!(s.parse::<Level>().unwrap(), level, "{s}");
            assert_eq!(
                s.parse::<LevelFilter>().unwrap(),
                LevelFilter::from(level),
                "{s}"
            );
        }
    }

    #[test]
    fn rejects_unknown_names() {
        for s in ["", "inf", "warnings", " info", "off", "5"] {
            let err = s.parse::<Level>().unwrap_err();
            assert!(
                matches!(&err, Error::ParseLevel { input } if input == s),
                "{s}"
            );
        }
        assert_eq!(
            "verbos".parse::<Level>().unwrap_err().to_string(),
            "unknown log level \"verbos\" (expected trace, debug, info, warn, error or fatal)"
        );
        assert_eq!("OFF".parse::<LevelFilter>().unwrap(), LevelFilter::Off);
        assert_eq!("None".parse::<LevelFilter>().unwrap(), LevelFilter::Off);
        assert!("nope".parse::<LevelFilter>().is_err());
    }

    #[test]
    fn display_round_trips() {
        for level in Level::ALL {
            assert_eq!(level.to_string().parse::<Level>().unwrap(), level);
            assert_eq!(level.serilog_name().parse::<Level>().unwrap(), level);
            let filter = LevelFilter::from(level);
            assert_eq!(filter.to_string().parse::<LevelFilter>().unwrap(), filter);
            assert_eq!(filter.to_level(), Some(level));
        }
        assert_eq!(LevelFilter::Off.to_string(), "off");
        assert_eq!(format!("{:>6}|", Level::Info), "  INFO|");
    }

    #[test]
    fn ordering_and_filtering() {
        assert!(Level::Trace < Level::Debug && Level::Error < Level::Fatal);
        for filter in [
            LevelFilter::Trace,
            LevelFilter::Info,
            LevelFilter::Fatal,
            LevelFilter::Off,
        ] {
            for level in Level::ALL {
                assert_eq!(filter.allows(level), level as u8 >= filter as u8);
            }
        }
        assert_eq!(LevelFilter::default(), LevelFilter::Info);
        assert_eq!(LevelFilter::Off.to_level(), None);
    }
}
