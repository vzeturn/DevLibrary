//! `System.TimeSpan`: 100 ns ticks, `TimeSpan.Parse` (invariant culture) and the `c` format.

use std::fmt;
use std::time::Duration;

/// Ticks per second (a tick is 100 ns).
const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_MINUTE: i64 = 60 * TICKS_PER_SECOND;
const TICKS_PER_HOUR: i64 = 60 * TICKS_PER_MINUTE;
const TICKS_PER_DAY: i64 = 24 * TICKS_PER_HOUR;
/// `TimeSpan.MaxValue.Days`.
const MAX_DAYS: i64 = 10_675_199;

/// A signed time interval with the range and precision of .NET `TimeSpan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct TimeSpan {
    ticks: i64,
}

/// Why [`TimeSpan::parse`] refused a string; the variants are the two .NET exceptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSpanParseError {
    /// `FormatException`: the text has no TimeSpan shape.
    Format,
    /// `OverflowException`: a component is out of range or has too many digits.
    Overflow,
}

impl TimeSpan {
    /// The zero interval.
    pub const ZERO: TimeSpan = TimeSpan { ticks: 0 };

    /// An interval of `ticks` 100 ns units.
    pub const fn from_ticks(ticks: i64) -> TimeSpan {
        TimeSpan { ticks }
    }

    /// An interval of whole seconds.
    pub const fn from_secs(seconds: i64) -> TimeSpan {
        TimeSpan {
            ticks: seconds.saturating_mul(TICKS_PER_SECOND),
        }
    }

    /// An interval of whole milliseconds.
    pub const fn from_millis(millis: i64) -> TimeSpan {
        TimeSpan {
            ticks: millis.saturating_mul(10_000),
        }
    }

    /// The interval in 100 ns ticks.
    pub const fn ticks(self) -> i64 {
        self.ticks
    }

    /// `TimeSpan.TotalSeconds`.
    pub fn total_seconds(self) -> f64 {
        self.ticks as f64 / TICKS_PER_SECOND as f64
    }

    /// The interval as a `Duration`; `None` when it is negative.
    pub fn to_duration(self) -> Option<Duration> {
        let ticks = u64::try_from(self.ticks).ok()?;
        let tps = TICKS_PER_SECOND as u64;
        Some(Duration::new(ticks / tps, ((ticks % tps) * 100) as u32))
    }

    /// `TimeSpan.Parse(text, CultureInfo.InvariantCulture)`: `[-]d`, `[-][d.]h:m[:s[.f]]` and
    /// `[-]d:h:m:s[.f]`, surrounding white space allowed. `h:m:s` with hours above 23 is
    /// read as `d:h:m`, like .NET (`24:00:00` is 24 days).
    pub fn parse(text: &str) -> Result<TimeSpan, TimeSpanParseError> {
        use TimeSpanParseError::{Format, Overflow};
        let text = super::dotnet::trim(text);
        let (negative, body) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let mut nums: Vec<&str> = Vec::new();
        let mut seps: Vec<u8> = Vec::new();
        let mut start = 0;
        for (i, b) in body.bytes().enumerate() {
            match b {
                b'0'..=b'9' => {}
                b'.' | b':' => {
                    if i == start {
                        return Err(Format);
                    }
                    nums.push(&body[start..i]);
                    seps.push(b);
                    start = i + 1;
                }
                _ => return Err(Format),
            }
        }
        if start >= body.len() {
            return Err(Format);
        }
        nums.push(&body[start..]);

        let n = |i: usize| -> Result<i64, TimeSpanParseError> {
            let digits = nums[i];
            if digits.len() > 10 {
                return Err(Overflow);
            }
            digits.parse::<i64>().map_err(|_| Overflow)
        };
        let fraction = |i: usize| -> Result<i64, TimeSpanParseError> {
            let digits = nums[i];
            if digits.len() > 7 {
                return Err(Overflow);
            }
            let v: i64 = digits.parse().map_err(|_| Overflow)?;
            Ok(v * 10_i64.pow(7 - digits.len() as u32))
        };
        // (days, hours, minutes, seconds, fraction ticks)
        let parts: (i64, i64, i64, i64, i64) = match seps.as_slice() {
            [] => (n(0)?, 0, 0, 0, 0),
            [b':'] => (0, n(0)?, n(1)?, 0, 0),
            [b':', b':'] => {
                let (a, b, c) = (n(0)?, n(1)?, n(2)?);
                if a <= 23 && b <= 59 && c <= 59 {
                    (0, a, b, c, 0)
                } else {
                    (a, b, c, 0, 0)
                }
            }
            [b'.', b':'] => (n(0)?, n(1)?, n(2)?, 0, 0),
            [b'.', b':', b':'] | [b':', b':', b':'] => (n(0)?, n(1)?, n(2)?, n(3)?, 0),
            [b':', b':', b'.'] => (0, n(0)?, n(1)?, n(2)?, fraction(3)?),
            [b'.', b':', b':', b'.'] | [b':', b':', b':', b'.'] => {
                (n(0)?, n(1)?, n(2)?, n(3)?, fraction(4)?)
            }
            _ => return Err(Format),
        };
        let (d, h, m, s, f) = parts;
        if d > MAX_DAYS || h > 23 || m > 59 || s > 59 {
            return Err(Overflow);
        }
        let total = i128::from(d) * i128::from(TICKS_PER_DAY)
            + i128::from(h) * i128::from(TICKS_PER_HOUR)
            + i128::from(m) * i128::from(TICKS_PER_MINUTE)
            + i128::from(s) * i128::from(TICKS_PER_SECOND)
            + i128::from(f);
        let signed = if negative { -total } else { total };
        i64::try_from(signed)
            .map(TimeSpan::from_ticks)
            .map_err(|_| Overflow)
    }
}

/// The `c` (constant) format of `TimeSpan.ToString()`: `[-][d.]hh:mm:ss[.fffffff]`.
impl fmt::Display for TimeSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let negative = self.ticks < 0;
        let t = self.ticks.unsigned_abs();
        let (tpd, tph, tpm, tps) = (
            TICKS_PER_DAY as u64,
            TICKS_PER_HOUR as u64,
            TICKS_PER_MINUTE as u64,
            TICKS_PER_SECOND as u64,
        );
        let days = t / tpd;
        let hours = t % tpd / tph;
        let minutes = t % tph / tpm;
        let seconds = t % tpm / tps;
        let frac = t % tps;
        if negative {
            f.write_str("-")?;
        }
        if days > 0 {
            write!(f, "{days}.")?;
        }
        write!(f, "{hours:02}:{minutes:02}:{seconds:02}")?;
        if frac > 0 {
            write!(f, ".{frac:07}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TimeSpanParseError::{Format, Overflow};
    use super::*;

    /// Expected values from `TimeSpan.Parse` through the .NET 10 configuration binder.
    #[test]
    fn parse_matches_dotnet() {
        let ok = [
            ("00:00:01", "00:00:01"),
            ("1", "1.00:00:00"),
            ("00:01", "00:01:00"),
            ("1.02:03:04", "1.02:03:04"),
            ("1:02:03:04", "1.02:03:04"),
            ("00:00:01.5", "00:00:01.5000000"),
            (" 00:00:02 ", "00:00:02"),
            ("\t00:00:03\t", "00:00:03"),
            ("-00:00:01", "-00:00:01"),
            ("24:00:00", "24.00:00:00"),
            ("0:0:1", "00:00:01"),
            ("10675199.02:48:05.4775807", "10675199.02:48:05.4775807"),
            ("00:00:1", "00:00:01"),
            ("001:00:00", "01:00:00"),
            ("0001:00:00", "01:00:00"),
            ("1:2", "01:02:00"),
            ("1.2:3", "1.02:03:00"),
            ("1.2:3:4.5", "1.02:03:04.5000000"),
            ("1.02:03", "1.02:03:00"),
            ("00:00:01.0000001", "00:00:01.0000001"),
        ];
        for (text, shown) in ok {
            assert_eq!(
                TimeSpan::parse(text).map(|t| t.to_string()),
                Ok(shown.to_owned()),
                "{text:?}"
            );
        }
        let bad = [
            ("00:60:00", Overflow),
            ("00:00:60", Overflow),
            ("abc", Format),
            ("", Format),
            ("00:00:01.12345678", Overflow),
            ("1.5", Format),
            ("10675199.02:48:05.4775808", Overflow),
            ("00:00:01.", Format),
            ("- 00:00:01", Format),
            ("+00:00:01", Format),
            ("00:00:00,5", Format),
            ("99999999", Overflow),
            ("1:2:3:4:5", Format),
        ];
        for (text, err) in bad {
            assert_eq!(TimeSpan::parse(text), Err(err), "{text:?}");
        }
    }

    #[test]
    fn display_and_conversions() {
        assert_eq!(TimeSpan::from_secs(30).to_string(), "00:00:30");
        assert_eq!(TimeSpan::from_millis(1500).to_string(), "00:00:01.5000000");
        assert_eq!(TimeSpan::from_secs(-1).to_string(), "-00:00:01");
        assert_eq!(TimeSpan::from_ticks(1).to_string(), "00:00:00.0000001");
        assert_eq!(
            TimeSpan::from_millis(1500).to_duration(),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(TimeSpan::from_secs(-1).to_duration(), None);
        assert_eq!(TimeSpan::from_secs(45).total_seconds(), 45.0);
    }
}
