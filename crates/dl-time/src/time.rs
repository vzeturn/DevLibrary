use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use crate::pattern::{self, Kinds, Parts};
use crate::{Error, Locale, buf, parse};

pub(crate) const NANOS_PER_SECOND: u64 = 1_000_000_000;
pub(crate) const NANOS_PER_DAY: u64 = 86_400 * NANOS_PER_SECOND;

/// A time of day with nanosecond precision, 00:00:00 to 23:59:59.999999999.
///
/// Leap seconds (second 60) are not representable.
///
/// ```
/// use dl_time::Time;
///
/// let t = Time::from_hms_milli(10, 20, 30, 250)?;
/// assert_eq!(t.to_string(), "10:20:30.250");
/// assert_eq!(t.seconds_from_midnight(), 37_230);
/// assert_eq!("10:20".parse::<Time>()?, Time::from_hms(10, 20, 0)?);
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    // Field order gives chronological derived ordering.
    hour: u8,
    minute: u8,
    second: u8,
    nanosecond: u32,
}

impl Time {
    /// 00:00:00.
    pub const MIDNIGHT: Time = Time {
        hour: 0,
        minute: 0,
        second: 0,
        nanosecond: 0,
    };
    /// 12:00:00.
    pub const NOON: Time = Time {
        hour: 12,
        minute: 0,
        second: 0,
        nanosecond: 0,
    };
    /// 23:59:59.999999999, the last representable instant of a day.
    pub const MAX: Time = Time {
        hour: 23,
        minute: 59,
        second: 59,
        nanosecond: 999_999_999,
    };

    /// Creates a time from hour (0 to 23), minute, second (0 to 59) and nanosecond
    /// (0 to 999 999 999).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] naming the first component out of range.
    pub const fn new(hour: u8, minute: u8, second: u8, nanosecond: u32) -> Result<Time, Error> {
        if hour > 23 {
            Err(Error::component("hour", hour as i64))
        } else if minute > 59 {
            Err(Error::component("minute", minute as i64))
        } else if second > 59 {
            Err(Error::component("second", second as i64))
        } else if nanosecond > 999_999_999 {
            Err(Error::component("nanosecond", nanosecond as i64))
        } else {
            Ok(Time {
                hour,
                minute,
                second,
                nanosecond,
            })
        }
    }

    /// Creates a time from hour, minute and second.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] for an out-of-range component.
    pub const fn from_hms(hour: u8, minute: u8, second: u8) -> Result<Time, Error> {
        Time::new(hour, minute, second, 0)
    }

    /// Creates a time from hour, minute, second and millisecond (0 to 999).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] for an out-of-range component.
    pub const fn from_hms_milli(
        hour: u8,
        minute: u8,
        second: u8,
        millisecond: u16,
    ) -> Result<Time, Error> {
        if millisecond > 999 {
            return Err(Error::component("millisecond", millisecond as i64));
        }
        Time::new(hour, minute, second, millisecond as u32 * 1_000_000)
    }

    /// Creates a time from hour, minute, second and microsecond (0 to 999 999).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] for an out-of-range component.
    pub const fn from_hms_micro(
        hour: u8,
        minute: u8,
        second: u8,
        microsecond: u32,
    ) -> Result<Time, Error> {
        if microsecond > 999_999 {
            return Err(Error::component("microsecond", microsecond as i64));
        }
        Time::new(hour, minute, second, microsecond * 1_000)
    }

    /// Creates a time from hour, minute, second and nanosecond; same as [`Time::new`].
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] for an out-of-range component.
    pub const fn from_hms_nano(
        hour: u8,
        minute: u8,
        second: u8,
        nanosecond: u32,
    ) -> Result<Time, Error> {
        Time::new(hour, minute, second, nanosecond)
    }

    /// Creates a time from the number of nanoseconds since midnight.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `nanos` is a day (86 400 × 10⁹) or more.
    pub const fn from_nanos_of_day(nanos: u64) -> Result<Time, Error> {
        if nanos >= NANOS_PER_DAY {
            return Err(Error::component("nanos of day", nanos as i64));
        }
        Ok(Time::from_nanos_unchecked(nanos))
    }

    /// Creates a time from the number of seconds since midnight (0 to 86 399).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `seconds` is 86 400 or more.
    pub const fn from_seconds_from_midnight(seconds: u32) -> Result<Time, Error> {
        if seconds >= 86_400 {
            return Err(Error::component("seconds from midnight", seconds as i64));
        }
        Ok(Time::from_nanos_unchecked(
            seconds as u64 * NANOS_PER_SECOND,
        ))
    }

    /// `nanos` must be below one day.
    pub(crate) const fn from_nanos_unchecked(nanos: u64) -> Time {
        let secs = (nanos / NANOS_PER_SECOND) as u32;
        Time {
            hour: (secs / 3600) as u8,
            minute: (secs / 60 % 60) as u8,
            second: (secs % 60) as u8,
            nanosecond: (nanos % NANOS_PER_SECOND) as u32,
        }
    }

    /// The hour (0 to 23).
    pub const fn hour(self) -> u8 {
        self.hour
    }

    /// The minute (0 to 59).
    pub const fn minute(self) -> u8 {
        self.minute
    }

    /// The second (0 to 59).
    pub const fn second(self) -> u8 {
        self.second
    }

    /// The millisecond within the second (0 to 999), truncated.
    pub const fn millisecond(self) -> u16 {
        (self.nanosecond / 1_000_000) as u16
    }

    /// The microsecond within the second (0 to 999 999), truncated.
    pub const fn microsecond(self) -> u32 {
        self.nanosecond / 1_000
    }

    /// The nanosecond within the second (0 to 999 999 999).
    pub const fn nanosecond(self) -> u32 {
        self.nanosecond
    }

    /// Whole seconds since midnight (0 to 86 399).
    pub const fn seconds_from_midnight(self) -> u32 {
        self.hour as u32 * 3600 + self.minute as u32 * 60 + self.second as u32
    }

    /// Nanoseconds since midnight.
    pub const fn nanos_of_day(self) -> u64 {
        self.seconds_from_midnight() as u64 * NANOS_PER_SECOND + self.nanosecond as u64
    }

    /// Replaces the hour.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `hour > 23`.
    pub const fn with_hour(self, hour: u8) -> Result<Time, Error> {
        Time::new(hour, self.minute, self.second, self.nanosecond)
    }

    /// Replaces the minute.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `minute > 59`.
    pub const fn with_minute(self, minute: u8) -> Result<Time, Error> {
        Time::new(self.hour, minute, self.second, self.nanosecond)
    }

    /// Replaces the second.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `second > 59`.
    pub const fn with_second(self, second: u8) -> Result<Time, Error> {
        Time::new(self.hour, self.minute, second, self.nanosecond)
    }

    /// Replaces the nanosecond.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `nanosecond > 999_999_999`.
    pub const fn with_nanosecond(self, nanosecond: u32) -> Result<Time, Error> {
        Time::new(self.hour, self.minute, self.second, nanosecond)
    }

    /// Parses ISO 8601 `HH:MM`, `HH:MM:SS` or `HH:MM:SS.f` with 1 to 9 fraction digits
    /// (`,` is also accepted as the decimal mark). This is also the [`FromStr`]
    /// implementation.
    ///
    /// # Errors
    /// [`Error::Parse`] for bad syntax, [`Error::InvalidComponent`] for values out of range
    /// (including the leap second `60`).
    pub fn parse_iso(s: &str) -> Result<Time, Error> {
        let mut c = parse::Cursor::new(s);
        let t = parse::time(&mut c, true)?;
        c.end()?;
        Ok(t)
    }

    /// Formats with a .NET-style pattern using English names. Date and offset tokens are
    /// rejected.
    ///
    /// ```
    /// let t = dl_time::Time::from_hms(14, 5, 0)?;
    /// assert_eq!(t.format("h:mm tt")?, "2:05 PM");
    /// # Ok::<(), dl_time::Error>(())
    /// ```
    ///
    /// # Errors
    /// [`Error::InvalidPattern`] for unknown letters or tokens this type cannot provide.
    pub fn format(self, pattern: &str) -> Result<String, Error> {
        self.format_locale(pattern, Locale::En)
    }

    /// Formats with a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`] for unknown letters or tokens this type cannot provide.
    pub fn format_locale(self, pattern: &str, locale: Locale) -> Result<String, Error> {
        pattern::format(
            Parts {
                date: None,
                time: Some(self),
                offset: None,
            },
            pattern,
            locale,
        )
    }

    /// Parses `input` against a .NET-style pattern (English names). Missing fields are 0.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse(input: &str, pattern: &str) -> Result<Time, Error> {
        Time::parse_locale(input, pattern, Locale::En)
    }

    /// Parses `input` against a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse_locale(input: &str, pattern: &str, locale: Locale) -> Result<Time, Error> {
        pattern::parse(input, pattern, locale, Kinds::TIME)?.time()
    }

    pub(crate) fn write<W: fmt::Write>(self, w: &mut W) -> fmt::Result {
        buf::write2(w, self.hour)?;
        w.write_char(':')?;
        buf::write2(w, self.minute)?;
        w.write_char(':')?;
        buf::write2(w, self.second)?;
        buf::write_fraction(w, self.nanosecond)
    }
}

impl Default for Time {
    /// Midnight.
    fn default() -> Self {
        Time::MIDNIGHT
    }
}

impl fmt::Display for Time {
    /// `HH:MM:SS`, followed by `.fff`, `.ffffff` or `.fffffffff` (the shortest exact one)
    /// when the nanosecond is not zero.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = buf::Buf::new();
        self.write(&mut b)?;
        f.pad(b.as_str())
    }
}

impl fmt::Debug for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Time {
    type Err = Error;

    /// Same as [`Time::parse_iso`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Time::parse_iso(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn constructors() {
        assert_eq!(Time::new(24, 0, 0, 0), Err(Error::component("hour", 24)));
        assert_eq!(Time::new(0, 60, 0, 0), Err(Error::component("minute", 60)));
        assert_eq!(Time::new(0, 0, 60, 0), Err(Error::component("second", 60)));
        assert_eq!(
            Time::new(0, 0, 0, 1_000_000_000),
            Err(Error::component("nanosecond", 1_000_000_000))
        );
        assert_eq!(
            Time::from_hms_milli(1, 2, 3, 1000),
            Err(Error::component("millisecond", 1000))
        );
        assert_eq!(
            Time::from_hms_micro(1, 2, 3, 1_000_000),
            Err(Error::component("microsecond", 1_000_000))
        );
        let t = Time::from_hms_micro(1, 2, 3, 456_789).unwrap();
        assert_eq!((t.hour(), t.minute(), t.second()), (1, 2, 3));
        assert_eq!(
            (t.millisecond(), t.microsecond(), t.nanosecond()),
            (456, 456_789, 456_789_000)
        );
        assert_eq!(Time::from_hms_nano(1, 2, 3, 4), Time::new(1, 2, 3, 4));
        assert_eq!(Time::default(), Time::MIDNIGHT);
    }

    #[test]
    fn nanos_of_day_round_trip() {
        assert_eq!(Time::MAX.nanos_of_day(), NANOS_PER_DAY - 1);
        assert_eq!(Time::from_nanos_of_day(NANOS_PER_DAY - 1), Ok(Time::MAX));
        assert!(Time::from_nanos_of_day(NANOS_PER_DAY).is_err());
        assert_eq!(
            Time::from_seconds_from_midnight(37_230),
            Time::from_hms(10, 20, 30)
        );
        assert!(Time::from_seconds_from_midnight(86_400).is_err());
        assert_eq!(Time::NOON.seconds_from_midnight(), 43_200);
        let mut s = 0;
        while s < 86_400 {
            let t = Time::from_seconds_from_midnight(s).unwrap();
            assert_eq!(t.seconds_from_midnight(), s);
            s += 7;
        }
    }

    #[test]
    fn with_components() {
        let t = Time::from_hms(10, 20, 30).unwrap();
        assert_eq!(t.with_hour(23).unwrap().hour(), 23);
        assert!(t.with_hour(24).is_err());
        assert!(t.with_minute(60).is_err());
        assert!(t.with_second(60).is_err());
        assert_eq!(t.with_nanosecond(5).unwrap().nanosecond(), 5);
    }

    #[test]
    fn display_and_parse() {
        let cases = [
            (Time::from_hms(10, 20, 30).unwrap(), "10:20:30"),
            (Time::new(10, 20, 30, 500_000_000).unwrap(), "10:20:30.500"),
            (Time::new(10, 20, 30, 120_000).unwrap(), "10:20:30.000120"),
            (Time::new(0, 0, 0, 1).unwrap(), "00:00:00.000000001"),
        ];
        for (t, s) in cases {
            assert_eq!(t.to_string(), s);
            assert_eq!(s.parse::<Time>(), Ok(t));
        }
        assert_eq!("10:20".parse::<Time>(), Time::from_hms(10, 20, 0));
        assert_eq!(
            "10:20:30,5".parse::<Time>(),
            Time::new(10, 20, 30, 500_000_000)
        );
        assert_eq!(
            "23:59:60".parse::<Time>(),
            Err(Error::component("second", 60))
        );
        assert_eq!(
            "24:00:00".parse::<Time>(),
            Err(Error::component("hour", 24))
        );
        assert!("10:20:".parse::<Time>().is_err());
        assert!("10:20:30.".parse::<Time>().is_err());
        assert!("10:20:30.1234567891".parse::<Time>().is_err());
        assert!("1:20".parse::<Time>().is_err());
        assert_eq!(std::format!("{:?}", Time::NOON), "12:00:00");
    }
}
