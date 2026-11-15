use alloc::string::String;
use core::fmt;
use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::str::FromStr;

use crate::date::{MAX_DAYS, MIN_DAYS};
use crate::pattern::{self, Kinds, Parts};
use crate::time::{NANOS_PER_DAY, NANOS_PER_SECOND};
use crate::{
    Date, Duration, Error, Locale, Month, Offset, OffsetDateTime, Time, Weekday, buf, parse,
};

/// A civil (wall-clock) date and time without an offset, like .NET `DateTime` with
/// `DateTimeKind.Unspecified` or Python's naive `datetime`.
///
/// Use [`assume_offset`](Self::assume_offset) to pin it to an instant. Ordering is
/// chronological.
///
/// ```
/// use dl_time::{Date, DateTime, Duration, Time};
///
/// let check_in = DateTime::parse_iso("2026-11-15T14:00:00")?;
/// let check_out = check_in + Duration::from_hours(46);
/// assert_eq!(check_out.to_string(), "2026-11-17T12:00:00");
/// assert_eq!(check_out - check_in, Duration::from_hours(46));
/// assert_eq!(check_in.date().days_until(check_out.date()), 2); // nights
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    date: Date,
    time: Time,
}

impl DateTime {
    /// -9999-01-01T00:00:00.
    pub const MIN: DateTime = DateTime {
        date: Date::MIN,
        time: Time::MIDNIGHT,
    };
    /// 9999-12-31T23:59:59.999999999.
    pub const MAX: DateTime = DateTime {
        date: Date::MAX,
        time: Time::MAX,
    };
    /// 1970-01-01T00:00:00.
    pub const UNIX_EPOCH: DateTime = DateTime {
        date: Date::UNIX_EPOCH,
        time: Time::MIDNIGHT,
    };

    /// Combines a date and a time.
    pub const fn new(date: Date, time: Time) -> DateTime {
        DateTime { date, time }
    }

    /// The date part.
    pub const fn date(self) -> Date {
        self.date
    }

    /// The time part.
    pub const fn time(self) -> Time {
        self.time
    }

    /// The year.
    pub const fn year(self) -> i32 {
        self.date.year()
    }

    /// The month.
    pub const fn month(self) -> Month {
        self.date.month()
    }

    /// The day of the month.
    pub const fn day(self) -> u8 {
        self.date.day()
    }

    /// The day of the year (1 to 366).
    pub const fn ordinal(self) -> u16 {
        self.date.ordinal()
    }

    /// The day of the week.
    pub const fn weekday(self) -> Weekday {
        self.date.weekday()
    }

    /// The hour (0 to 23).
    pub const fn hour(self) -> u8 {
        self.time.hour()
    }

    /// The minute.
    pub const fn minute(self) -> u8 {
        self.time.minute()
    }

    /// The second.
    pub const fn second(self) -> u8 {
        self.time.second()
    }

    /// The nanosecond within the second.
    pub const fn nanosecond(self) -> u32 {
        self.time.nanosecond()
    }

    /// Replaces the date part.
    pub const fn with_date(self, date: Date) -> DateTime {
        DateTime { date, ..self }
    }

    /// Replaces the time part.
    pub const fn with_time(self, time: Time) -> DateTime {
        DateTime { time, ..self }
    }

    /// Replaces the year.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if out of range or the day does not exist in that year.
    pub const fn with_year(self, year: i32) -> Result<DateTime, Error> {
        match self.date.with_year(year) {
            Ok(date) => Ok(self.with_date(date)),
            Err(e) => Err(e),
        }
    }

    /// Replaces the month (1 to 12).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if invalid or the day does not exist in that month.
    pub const fn with_month(self, month: u8) -> Result<DateTime, Error> {
        match self.date.with_month(month) {
            Ok(date) => Ok(self.with_date(date)),
            Err(e) => Err(e),
        }
    }

    /// Replaces the day of the month.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if the day does not exist in the month.
    pub const fn with_day(self, day: u8) -> Result<DateTime, Error> {
        match self.date.with_day(day) {
            Ok(date) => Ok(self.with_date(date)),
            Err(e) => Err(e),
        }
    }

    /// Replaces the hour.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `hour > 23`.
    pub const fn with_hour(self, hour: u8) -> Result<DateTime, Error> {
        match self.time.with_hour(hour) {
            Ok(time) => Ok(self.with_time(time)),
            Err(e) => Err(e),
        }
    }

    /// Replaces the minute.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `minute > 59`.
    pub const fn with_minute(self, minute: u8) -> Result<DateTime, Error> {
        match self.time.with_minute(minute) {
            Ok(time) => Ok(self.with_time(time)),
            Err(e) => Err(e),
        }
    }

    /// Replaces the second.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `second > 59`.
    pub const fn with_second(self, second: u8) -> Result<DateTime, Error> {
        match self.time.with_second(second) {
            Ok(time) => Ok(self.with_time(time)),
            Err(e) => Err(e),
        }
    }

    /// Replaces the nanosecond.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if `nanosecond > 999_999_999`.
    pub const fn with_nanosecond(self, nanosecond: u32) -> Result<DateTime, Error> {
        match self.time.with_nanosecond(nanosecond) {
            Ok(time) => Ok(self.with_time(time)),
            Err(e) => Err(e),
        }
    }

    /// Midnight at the start of the same day.
    pub const fn start_of_day(self) -> DateTime {
        self.date.midnight()
    }

    /// Nanoseconds since 1970-01-01T00:00:00 on the same wall clock.
    pub(crate) const fn epoch_nanos(self) -> i128 {
        self.date.to_unix_days() as i128 * NANOS_PER_DAY as i128 + self.time.nanos_of_day() as i128
    }

    /// Inverse of [`epoch_nanos`](Self::epoch_nanos); `None` outside the date range.
    pub(crate) const fn from_epoch_nanos(nanos: i128) -> Option<DateTime> {
        let day = NANOS_PER_DAY as i128;
        let days = nanos.div_euclid(day);
        if days < MIN_DAYS as i128 || days > MAX_DAYS as i128 {
            return None;
        }
        match Date::from_unix_days(days as i64) {
            Ok(date) => {
                let time = Time::from_nanos_unchecked(nanos.rem_euclid(day) as u64);
                Some(DateTime { date, time })
            }
            Err(_) => None,
        }
    }

    /// Wall-clock time `seconds` + `nanos` (below 10⁹) after 1970-01-01T00:00:00; `None`
    /// outside the date range. Uses only `i64` arithmetic.
    pub(crate) const fn from_epoch_seconds(seconds: i64, nanos: u32) -> Option<DateTime> {
        let days = seconds.div_euclid(86_400);
        let Ok(date) = Date::from_unix_days(days) else {
            return None;
        };
        let sod = seconds.rem_euclid(86_400) as u64;
        let time = Time::from_nanos_unchecked(sod * NANOS_PER_SECOND + nanos as u64);
        Some(DateTime { date, time })
    }

    /// `self + duration`, or `None` outside the supported range.
    pub const fn checked_add(self, duration: Duration) -> Option<DateTime> {
        DateTime::from_epoch_nanos(self.epoch_nanos() + duration.whole_nanoseconds())
    }

    /// `self - duration`, or `None` outside the supported range.
    pub const fn checked_sub(self, duration: Duration) -> Option<DateTime> {
        DateTime::from_epoch_nanos(self.epoch_nanos() - duration.whole_nanoseconds())
    }

    /// The signed duration from `earlier` to `self` (`self - earlier`). Never overflows:
    /// the date range spans far less than [`Duration::MAX`].
    pub const fn duration_since(self, earlier: DateTime) -> Duration {
        match Duration::from_nanos_i128(self.epoch_nanos() - earlier.epoch_nanos()) {
            Some(d) => d,
            // Unreachable: the span of the date range is about 6.3e20 ns.
            None => Duration::ZERO,
        }
    }

    /// Interprets this wall-clock time as local time at `offset`.
    pub const fn assume_offset(self, offset: Offset) -> OffsetDateTime {
        OffsetDateTime::new(self, offset)
    }

    /// Interprets this wall-clock time as UTC.
    pub const fn assume_utc(self) -> OffsetDateTime {
        OffsetDateTime::new(self, Offset::UTC)
    }

    /// Parses ISO 8601 `YYYY-MM-DDTHH:MM[:SS[.f]]`. The separator may be `T`, `t` or a space;
    /// a trailing offset is rejected (use [`OffsetDateTime::parse_rfc3339`]). This is also the
    /// [`FromStr`] implementation.
    ///
    /// # Errors
    /// [`Error::Parse`] for bad syntax, [`Error::InvalidComponent`] for invalid values.
    pub fn parse_iso(s: &str) -> Result<DateTime, Error> {
        let mut c = parse::Cursor::new(s);
        let date = parse::date(&mut c, true)?;
        match c.peek() {
            Some(b'T' | b't' | b' ') => c.pos += 1,
            _ => return Err(c.err("'T' or ' ' between date and time")),
        }
        let time = parse::time(&mut c, true)?;
        c.end()?;
        Ok(DateTime { date, time })
    }

    /// Formats with a .NET-style pattern using English names. Offset tokens are rejected.
    ///
    /// ```
    /// let dt = dl_time::DateTime::parse_iso("2026-11-15T10:20:30")?;
    /// assert_eq!(dt.format("dd/MM/yyyy HH:mm:ss")?, "15/11/2026 10:20:30");
    /// # Ok::<(), dl_time::Error>(())
    /// ```
    ///
    /// # Errors
    /// [`Error::InvalidPattern`] for unknown letters or offset tokens.
    pub fn format(self, pattern: &str) -> Result<String, Error> {
        self.format_locale(pattern, Locale::En)
    }

    /// Formats with a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`] for unknown letters or offset tokens.
    pub fn format_locale(self, pattern: &str, locale: Locale) -> Result<String, Error> {
        let parts = Parts {
            date: Some(self.date),
            time: Some(self.time),
            offset: None,
        };
        pattern::format(parts, pattern, locale)
    }

    /// Parses `input` against a .NET-style pattern (English names). The pattern must contain
    /// a year; a missing month or day defaults to 1 and missing time fields to 0.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse(input: &str, pattern: &str) -> Result<DateTime, Error> {
        DateTime::parse_locale(input, pattern, Locale::En)
    }

    /// Parses `input` against a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse_locale(input: &str, pattern: &str, locale: Locale) -> Result<DateTime, Error> {
        let p = pattern::parse(input, pattern, locale, Kinds::DATE_TIME)?;
        Ok(DateTime {
            date: p.date()?,
            time: p.time()?,
        })
    }

    pub(crate) fn write<W: fmt::Write>(self, w: &mut W) -> fmt::Result {
        self.date.write(w)?;
        w.write_char('T')?;
        self.time.write(w)
    }
}

impl fmt::Display for DateTime {
    /// ISO 8601 `YYYY-MM-DDTHH:MM:SS[.fff|.ffffff|.fffffffff]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = buf::Buf::new();
        self.write(&mut b)?;
        f.pad(b.as_str())
    }
}

impl fmt::Debug for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for DateTime {
    type Err = Error;

    /// Same as [`DateTime::parse_iso`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DateTime::parse_iso(s)
    }
}

impl Add<Duration> for DateTime {
    type Output = DateTime;

    /// # Panics
    /// If the result is outside the supported range; see [`DateTime::checked_add`].
    #[track_caller]
    fn add(self, rhs: Duration) -> DateTime {
        self.checked_add(rhs).expect("DateTime + Duration overflow")
    }
}

impl Sub<Duration> for DateTime {
    type Output = DateTime;

    /// # Panics
    /// If the result is outside the supported range; see [`DateTime::checked_sub`].
    #[track_caller]
    fn sub(self, rhs: Duration) -> DateTime {
        self.checked_sub(rhs).expect("DateTime - Duration overflow")
    }
}

impl AddAssign<Duration> for DateTime {
    /// # Panics
    /// If the result is outside the supported range.
    #[track_caller]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl SubAssign<Duration> for DateTime {
    /// # Panics
    /// If the result is outside the supported range.
    #[track_caller]
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub for DateTime {
    type Output = Duration;

    /// Same as [`DateTime::duration_since`]; never panics.
    fn sub(self, rhs: DateTime) -> Duration {
        self.duration_since(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    fn dt(s: &str) -> DateTime {
        s.parse().unwrap()
    }

    #[test]
    fn accessors_and_with() {
        let x = dt("2026-11-15T10:20:30.123456789");
        assert_eq!((x.year(), x.month(), x.day()), (2026, Month::November, 15));
        assert_eq!(
            (x.hour(), x.minute(), x.second(), x.nanosecond()),
            (10, 20, 30, 123_456_789)
        );
        assert_eq!((x.ordinal(), x.weekday()), (319, Weekday::Sunday));
        assert_eq!(x.start_of_day(), dt("2026-11-15T00:00:00"));
        assert_eq!(x.with_year(2025).unwrap().year(), 2025);
        assert_eq!(x.with_month(2).unwrap().month(), Month::February);
        assert_eq!(
            x.with_day(31).map(DateTime::day),
            Err(Error::component("day", 31))
        );
        assert_eq!(x.with_hour(24), Err(Error::component("hour", 24)));
        assert_eq!(x.with_minute(1).unwrap().minute(), 1);
        assert_eq!(x.with_second(60), Err(Error::component("second", 60)));
        assert_eq!(
            x.with_nanosecond(0).unwrap().to_string(),
            "2026-11-15T10:20:30"
        );
        assert!(dt("2024-02-29T00:00:00").with_year(2023).is_err());
        assert!(x.with_month(13).is_err());
    }

    #[test]
    fn arithmetic() {
        let x = dt("2026-12-31T22:00:00");
        assert_eq!(x + Duration::from_hours(3), dt("2027-01-01T01:00:00"));
        assert_eq!(x - Duration::from_days(365), dt("2025-12-31T22:00:00"));
        assert_eq!(
            dt("2024-03-01T00:00:00") - Duration::NANOSECOND,
            dt("2024-02-29T23:59:59.999999999")
        );
        assert_eq!(dt("2027-01-01T01:00:00") - x, Duration::from_hours(3));
        assert_eq!(x - dt("2027-01-01T01:00:00"), Duration::from_hours(-3));
        let mut y = x;
        y += Duration::MINUTE;
        y -= Duration::SECOND;
        assert_eq!(y.to_string(), "2026-12-31T22:00:59");
        assert_eq!(DateTime::MAX.checked_add(Duration::NANOSECOND), None);
        assert_eq!(DateTime::MIN.checked_sub(Duration::NANOSECOND), None);
        assert_eq!(DateTime::MAX.checked_add(Duration::MIN), None);
        assert_eq!(DateTime::MIN.checked_add(Duration::MAX), None);
        let span = DateTime::MAX - DateTime::MIN;
        assert_eq!(DateTime::MIN + span, DateTime::MAX);
        assert_eq!(span.whole_days(), 7_304_484 - 1);
        assert_eq!(DateTime::MAX.duration_since(DateTime::MIN), span);
    }

    #[test]
    #[should_panic(expected = "overflow")]
    fn add_overflow_panics() {
        let _ = DateTime::MAX + Duration::SECOND;
    }

    #[test]
    fn parse_iso_forms() {
        assert_eq!(dt("2026-11-15 10:20"), dt("2026-11-15T10:20:00"));
        assert_eq!(dt("2026-11-15t10:20:00"), dt("2026-11-15T10:20:00"));
        assert_eq!(
            dt("-0001-12-31T23:59:59").to_string(),
            "-0001-12-31T23:59:59"
        );
        let err = |offset, expected| Err(Error::Parse { offset, expected });
        assert_eq!(
            "2026-11-15".parse::<DateTime>(),
            err(10, "'T' or ' ' between date and time")
        );
        assert_eq!(
            "2026-11-15T10:20:30Z".parse::<DateTime>(),
            err(19, "end of input")
        );
        assert_eq!(
            "2026-11-15T10".parse::<DateTime>(),
            err(13, "':' after hour")
        );
        assert_eq!(
            std::format!("{:?}", DateTime::MAX),
            "9999-12-31T23:59:59.999999999"
        );
        assert_eq!(DateTime::MIN.to_string(), "-9999-01-01T00:00:00");
    }
}
