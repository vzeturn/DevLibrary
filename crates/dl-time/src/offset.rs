use core::fmt;
use core::str::FromStr;

use crate::parse::Cursor;
use crate::{Error, buf};

const MAX_SECONDS: i32 = 18 * 3600;

/// A fixed offset from UTC in seconds east of Greenwich, at most ±18:00.
///
/// There is no time zone database: an `Offset` never changes with the date. Vietnam has used
/// UTC+07:00 without daylight saving time since 1975, so [`Offset::VIETNAM`] is exact for
/// current dates.
///
/// ```
/// use dl_time::Offset;
///
/// assert_eq!(Offset::VIETNAM.to_string(), "+07:00");
/// assert_eq!(Offset::from_hms(-3, -30, 0)?.whole_seconds(), -12_600);
/// assert_eq!("+05:45".parse::<Offset>()?, Offset::from_hms(5, 45, 0)?);
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Offset {
    seconds: i32,
}

impl Offset {
    /// UTC, `+00:00`.
    pub const UTC: Offset = Offset { seconds: 0 };
    /// Vietnam (Indochina Time), `+07:00`.
    pub const VIETNAM: Offset = Offset { seconds: 7 * 3600 };

    /// Creates an offset from seconds east of UTC.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] with name `"offset"` if `|seconds| > 64 800` (18 hours).
    pub const fn from_seconds(seconds: i32) -> Result<Offset, Error> {
        if seconds > MAX_SECONDS || seconds < -MAX_SECONDS {
            Err(Error::component("offset", seconds as i64))
        } else {
            Ok(Offset { seconds })
        }
    }

    /// Creates an offset from hours, minutes and seconds. All non-zero components must have
    /// the same sign: `-03:30` is `from_hms(-3, -30, 0)`.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if minutes or seconds are outside -59..=59, signs are
    /// mixed, or the total exceeds ±18:00.
    pub const fn from_hms(hours: i8, minutes: i8, seconds: i8) -> Result<Offset, Error> {
        if minutes < -59 || minutes > 59 {
            return Err(Error::component("offset minute", minutes as i64));
        }
        if seconds < -59 || seconds > 59 {
            return Err(Error::component("offset second", seconds as i64));
        }
        let (h, m, s) = (hours as i32, minutes as i32, seconds as i32);
        if (h > 0 || m > 0 || s > 0) && (h < 0 || m < 0 || s < 0) {
            return Err(Error::component("offset", (h * 3600 + m * 60 + s) as i64));
        }
        Offset::from_seconds(h * 3600 + m * 60 + s)
    }

    /// Seconds east of UTC (negative west).
    pub const fn whole_seconds(self) -> i32 {
        self.seconds
    }

    /// Whole minutes east of UTC, truncated toward zero.
    pub const fn whole_minutes(self) -> i16 {
        (self.seconds / 60) as i16
    }

    /// Whole hours east of UTC, truncated toward zero.
    pub const fn whole_hours(self) -> i8 {
        (self.seconds / 3600) as i8
    }

    /// The offset as `(hours, minutes, seconds)`, every component with the offset's sign.
    pub const fn as_hms(self) -> (i8, i8, i8) {
        let s = self.seconds;
        ((s / 3600) as i8, (s / 60 % 60) as i8, (s % 60) as i8)
    }

    /// Whether this is UTC (zero offset).
    pub const fn is_utc(self) -> bool {
        self.seconds == 0
    }

    /// Whether the offset is west of UTC.
    pub const fn is_negative(self) -> bool {
        self.seconds < 0
    }

    /// Writes `±HH:MM`, plus `:SS` when the seconds component is not zero.
    pub(crate) fn write<W: fmt::Write>(self, w: &mut W) -> fmt::Result {
        let abs = self.seconds.unsigned_abs();
        w.write_char(if self.seconds < 0 { '-' } else { '+' })?;
        buf::write2(w, (abs / 3600) as u8)?;
        w.write_char(':')?;
        buf::write2(w, (abs / 60 % 60) as u8)?;
        if !abs.is_multiple_of(60) {
            w.write_char(':')?;
            buf::write2(w, (abs % 60) as u8)?;
        }
        Ok(())
    }
}

impl fmt::Display for Offset {
    /// `±HH:MM` (`+00:00` for UTC), or `±HH:MM:SS` if the offset has a seconds component.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = buf::Buf::new();
        self.write(&mut b)?;
        f.pad(b.as_str())
    }
}

impl fmt::Debug for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Offset {
    type Err = Error;

    /// Parses `Z`, `z`, `±HH`, `±HHMM`, `±HH:MM` or `±HH:MM:SS`.
    ///
    /// # Errors
    /// [`Error::Parse`] for bad syntax, [`Error::InvalidComponent`] for an out-of-range value.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut c = Cursor::new(s);
        if c.eat(b'Z') || c.eat(b'z') {
            c.end()?;
            return Ok(Offset::UTC);
        }
        let negative = match c.peek() {
            Some(b'+') => false,
            Some(b'-') => true,
            _ => return Err(c.err("'Z', '+' or '-'")),
        };
        c.pos += 1;
        let h = c.fixed(2, "two-digit offset hour")? as i32;
        let mut m = 0;
        let mut sec = 0;
        if c.eat(b':') {
            m = c.fixed(2, "two-digit offset minute")? as i32;
            if c.eat(b':') {
                sec = c.fixed(2, "two-digit offset second")? as i32;
            }
        } else if c.peek().is_some() {
            m = c.fixed(2, "two-digit offset minute")? as i32;
        }
        c.end()?;
        if m > 59 {
            return Err(Error::component("offset minute", m as i64));
        }
        if sec > 59 {
            return Err(Error::component("offset second", sec as i64));
        }
        let total = h * 3600 + m * 60 + sec;
        Offset::from_seconds(if negative { -total } else { total })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn constructors() {
        assert_eq!(
            Offset::from_seconds(64_800).map(Offset::whole_hours),
            Ok(18)
        );
        assert_eq!(
            Offset::from_seconds(64_801),
            Err(Error::component("offset", 64_801))
        );
        assert_eq!(
            Offset::from_seconds(-64_801),
            Err(Error::component("offset", -64_801))
        );
        assert_eq!(Offset::from_hms(7, 0, 0), Ok(Offset::VIETNAM));
        assert_eq!(
            Offset::from_hms(-3, 30, 0),
            Err(Error::component("offset", -10_800 + 1800))
        );
        assert_eq!(
            Offset::from_hms(0, -30, 0).map(Offset::whole_seconds),
            Ok(-1800)
        );
        assert!(Offset::from_hms(0, 60, 0).is_err());
        assert!(Offset::from_hms(0, 0, -60).is_err());
        assert!(Offset::from_hms(19, 0, 0).is_err());
        let o = Offset::from_hms(-5, -45, -30).unwrap();
        assert_eq!(o.as_hms(), (-5, -45, -30));
        assert_eq!(o.whole_minutes(), -345);
        assert!(o.is_negative() && !o.is_utc());
        assert!(Offset::UTC.is_utc());
        assert_eq!(Offset::default(), Offset::UTC);
    }

    #[test]
    fn display_and_parse() {
        assert_eq!(Offset::UTC.to_string(), "+00:00");
        assert_eq!(Offset::VIETNAM.to_string(), "+07:00");
        assert_eq!(Offset::from_hms(-9, -30, 0).unwrap().to_string(), "-09:30");
        assert_eq!(
            Offset::from_hms(0, 0, -15).unwrap().to_string(),
            "-00:00:15"
        );
        assert_eq!(
            Offset::from_hms(5, 30, 15).unwrap().to_string(),
            "+05:30:15"
        );
        for (s, secs) in [
            ("Z", 0),
            ("z", 0),
            ("+07", 25_200),
            ("+0700", 25_200),
            ("-03:30", -12_600),
            ("+05:30:15", 19_815),
            ("-00:00", 0),
            ("+18:00", 64_800),
        ] {
            assert_eq!(
                s.parse::<Offset>().map(Offset::whole_seconds),
                Ok(secs),
                "{s}"
            );
        }
        assert_eq!(
            "+18:01".parse::<Offset>(),
            Err(Error::component("offset", 64_860))
        );
        assert_eq!(
            "+07:60".parse::<Offset>(),
            Err(Error::component("offset minute", 60))
        );
        assert_eq!(
            "+07:00:60".parse::<Offset>(),
            Err(Error::component("offset second", 60))
        );
        assert!("07:00".parse::<Offset>().is_err());
        assert!("+7".parse::<Offset>().is_err());
        assert!("+07:0".parse::<Offset>().is_err());
        assert!("Z ".parse::<Offset>().is_err());
        assert!("".parse::<Offset>().is_err());
    }
}
