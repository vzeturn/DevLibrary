use alloc::string::String;
use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::{Add, AddAssign, Sub, SubAssign};
use core::str::FromStr;

use crate::pattern::{self, Kinds, Parts};
use crate::time::NANOS_PER_SECOND;
use crate::{
    Date, DateTime, Duration, Error, Locale, Month, Offset, Time, Weekday, buf, http, parse,
};

/// An instant on the time line, represented as a local [`DateTime`] plus the [`Offset`] it is
/// observed at, like .NET `DateTimeOffset` or a Python aware `datetime` with a fixed
/// `timezone`.
///
/// # Equality and ordering
/// `==`, `Ord` and `Hash` compare the **instant**, not the fields: `10:00+07:00` equals
/// `03:00Z`. Two equal values may still format differently because they keep their own
/// offset. Compare [`datetime`](Self::datetime) and [`offset`](Self::offset) to test for
/// identical fields.
///
/// # Range
/// The local date-time is always within -9999-01-01..=9999-12-31. Converting to another
/// offset can leave that range near the ends (for example 9999-12-31T23:00-05:00 is year
/// 10000 in UTC); [`checked_to_offset`](Self::checked_to_offset) reports this, while
/// [`to_offset`](Self::to_offset) and [`to_utc`](Self::to_utc) panic. The Unix timestamp
/// getters always work.
///
/// ```
/// use dl_time::{Offset, OffsetDateTime};
///
/// let t = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30+07:00")?;
/// assert_eq!(t.unix_seconds(), 1_794_712_830);
/// assert_eq!(t.to_utc().to_rfc3339(), "2026-11-15T03:20:30Z");
/// assert_eq!(t, OffsetDateTime::parse_rfc3339("2026-11-15T03:20:30Z")?);
/// assert_eq!(t.to_http_date(), "Sun, 15 Nov 2026 03:20:30 GMT");
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy)]
pub struct OffsetDateTime {
    local: DateTime,
    offset: Offset,
}

impl OffsetDateTime {
    /// 1970-01-01T00:00:00Z.
    pub const UNIX_EPOCH: OffsetDateTime = OffsetDateTime {
        local: DateTime::UNIX_EPOCH,
        offset: Offset::UTC,
    };

    /// A local date-time observed at `offset`. Same as [`DateTime::assume_offset`].
    pub const fn new(local: DateTime, offset: Offset) -> OffsetDateTime {
        OffsetDateTime { local, offset }
    }

    /// The instant `nanos` nanoseconds after the Unix epoch, in UTC.
    ///
    /// # Errors
    /// [`Error::Overflow`] if the UTC date is outside -9999-01-01..=9999-12-31.
    pub const fn from_unix_nanos(nanos: i128) -> Result<OffsetDateTime, Error> {
        let seconds = nanos.div_euclid(NANOS_PER_SECOND as i128);
        if seconds < i64::MIN as i128 || seconds > i64::MAX as i128 {
            return Err(Error::Overflow);
        }
        let sub = nanos.rem_euclid(NANOS_PER_SECOND as i128) as u32;
        OffsetDateTime::utc(seconds as i64, sub)
    }

    /// UTC result of the `from_unix_*` constructors.
    const fn utc(seconds: i64, nanos: u32) -> Result<OffsetDateTime, Error> {
        match DateTime::from_epoch_seconds(seconds, nanos) {
            Some(local) => Ok(OffsetDateTime {
                local,
                offset: Offset::UTC,
            }),
            None => Err(Error::Overflow),
        }
    }

    /// The instant `seconds` seconds after the Unix epoch (negative before), in UTC.
    ///
    /// # Errors
    /// [`Error::Overflow`] if the UTC date is outside -9999-01-01..=9999-12-31.
    pub const fn from_unix_seconds(seconds: i64) -> Result<OffsetDateTime, Error> {
        OffsetDateTime::utc(seconds, 0)
    }

    /// The instant `millis` milliseconds after the Unix epoch, in UTC. In the browser, pass
    /// JavaScript's `Date.now()` here.
    ///
    /// # Errors
    /// [`Error::Overflow`] if the UTC date is outside -9999-01-01..=9999-12-31.
    pub const fn from_unix_millis(millis: i64) -> Result<OffsetDateTime, Error> {
        let sub = millis.rem_euclid(1000) as u32 * 1_000_000;
        OffsetDateTime::utc(millis.div_euclid(1000), sub)
    }

    /// Nanoseconds since the Unix epoch.
    pub const fn unix_nanos(self) -> i128 {
        self.unix_seconds() as i128 * NANOS_PER_SECOND as i128 + self.nanosecond() as i128
    }

    /// Whole seconds since the Unix epoch, rounded toward negative infinity
    /// (1969-12-31T23:59:59.5Z is `-1`), like POSIX `time_t`.
    pub const fn unix_seconds(self) -> i64 {
        // The sub-second part is non-negative, so this is already the floor.
        self.local.date().to_unix_days() as i64 * 86_400
            + self.local.time().seconds_from_midnight() as i64
            - self.offset.whole_seconds() as i64
    }

    /// Whole milliseconds since the Unix epoch, rounded toward negative infinity, like
    /// JavaScript `Date.getTime()`.
    pub const fn unix_millis(self) -> i64 {
        self.unix_seconds() * 1000 + (self.nanosecond() / 1_000_000) as i64
    }

    /// The current time in UTC from the system clock.
    ///
    /// Requires the `std` feature and is not available on `wasm32-unknown-unknown`, which
    /// has no clock: in the browser call `OffsetDateTime::from_unix_millis(Date.now())`.
    /// A clock outside the supported range saturates to its nearest end.
    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub fn now_utc() -> OffsetDateTime {
        let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        OffsetDateTime::from_unix_nanos(nanos).unwrap_or(if nanos < 0 {
            DateTime::MIN.assume_utc()
        } else {
            DateTime::MAX.assume_utc()
        })
    }

    /// The current time from the system clock, observed at `offset`.
    ///
    /// Same platform requirements as [`now_utc`](Self::now_utc).
    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub fn now(offset: Offset) -> OffsetDateTime {
        let utc = OffsetDateTime::now_utc();
        utc.checked_to_offset(offset).unwrap_or(utc)
    }

    /// The offset.
    pub const fn offset(self) -> Offset {
        self.offset
    }

    /// The local date and time at [`offset`](Self::offset).
    pub const fn datetime(self) -> DateTime {
        self.local
    }

    /// The local date.
    pub const fn date(self) -> Date {
        self.local.date()
    }

    /// The local time of day.
    pub const fn time(self) -> Time {
        self.local.time()
    }

    /// The local year.
    pub const fn year(self) -> i32 {
        self.local.year()
    }

    /// The local month.
    pub const fn month(self) -> Month {
        self.local.month()
    }

    /// The local day of the month.
    pub const fn day(self) -> u8 {
        self.local.day()
    }

    /// The local day of the week.
    pub const fn weekday(self) -> Weekday {
        self.local.weekday()
    }

    /// The local hour.
    pub const fn hour(self) -> u8 {
        self.local.hour()
    }

    /// The local minute.
    pub const fn minute(self) -> u8 {
        self.local.minute()
    }

    /// The second.
    pub const fn second(self) -> u8 {
        self.local.second()
    }

    /// The nanosecond within the second.
    pub const fn nanosecond(self) -> u32 {
        self.local.nanosecond()
    }

    /// The same instant observed at `offset`, or `None` if the local date there would be
    /// outside -9999..=9999.
    pub const fn checked_to_offset(self, offset: Offset) -> Option<OffsetDateTime> {
        let local = self.unix_seconds() + offset.whole_seconds() as i64;
        match DateTime::from_epoch_seconds(local, self.nanosecond()) {
            Some(local) => Some(OffsetDateTime { local, offset }),
            None => None,
        }
    }

    /// The same instant observed at `offset`.
    ///
    /// ```
    /// use dl_time::{Offset, OffsetDateTime};
    ///
    /// let utc = OffsetDateTime::parse_rfc3339("2026-11-15T20:30:00Z")?;
    /// assert_eq!(utc.to_offset(Offset::VIETNAM).to_rfc3339(), "2026-11-16T03:30:00+07:00");
    /// # Ok::<(), dl_time::Error>(())
    /// ```
    ///
    /// # Panics
    /// If the local date at `offset` is outside -9999..=9999, which can only happen within
    /// 18 hours of the ends of the range; see [`checked_to_offset`](Self::checked_to_offset).
    #[track_caller]
    pub const fn to_offset(self, offset: Offset) -> OffsetDateTime {
        match self.checked_to_offset(offset) {
            Some(t) => t,
            None => panic!("OffsetDateTime::to_offset out of range"),
        }
    }

    /// The same instant in UTC.
    ///
    /// # Panics
    /// If the UTC date is outside -9999..=9999; see
    /// [`checked_to_offset`](Self::checked_to_offset).
    #[track_caller]
    pub const fn to_utc(self) -> OffsetDateTime {
        self.to_offset(Offset::UTC)
    }

    /// `self + duration` at the same offset, or `None` outside the supported range.
    pub const fn checked_add(self, duration: Duration) -> Option<OffsetDateTime> {
        match self.local.checked_add(duration) {
            Some(local) => Some(OffsetDateTime {
                local,
                offset: self.offset,
            }),
            None => None,
        }
    }

    /// `self - duration` at the same offset, or `None` outside the supported range.
    pub const fn checked_sub(self, duration: Duration) -> Option<OffsetDateTime> {
        match self.local.checked_sub(duration) {
            Some(local) => Some(OffsetDateTime {
                local,
                offset: self.offset,
            }),
            None => None,
        }
    }

    /// The signed duration from `earlier` to `self`, whatever their offsets. Never overflows.
    pub const fn duration_since(self, earlier: OffsetDateTime) -> Duration {
        match Duration::from_nanos_i128(self.unix_nanos() - earlier.unix_nanos()) {
            Some(d) => d,
            // Unreachable: the representable span is about 6.3e20 ns.
            None => Duration::ZERO,
        }
    }

    /// Parses an RFC 3339 timestamp such as `2026-11-15T10:20:30.5+07:00` or
    /// `2026-11-15 03:20:30Z`. This is also the [`FromStr`] implementation.
    ///
    /// Accepted: `T`, `t` or a space between date and time; `Z` or `z` for UTC; a fraction
    /// of 1 to 9 digits. Rejected: leap second `60` (not representable; reported as
    /// [`Error::InvalidComponent`] for `"second"`), more than 9 fraction digits, missing
    /// seconds, offsets beyond ±18:00 and signed or 5+ digit years. `-00:00` ("offset
    /// unknown") is read as UTC.
    ///
    /// # Errors
    /// [`Error::Parse`] with the byte offset for bad syntax, [`Error::InvalidComponent`] for
    /// out-of-range values.
    pub fn parse_rfc3339(s: &str) -> Result<OffsetDateTime, Error> {
        let mut c = parse::Cursor::new(s);
        let date = parse::date(&mut c, false)?;
        match c.peek() {
            Some(b'T' | b't' | b' ') => c.pos += 1,
            _ => return Err(c.err("'T' between date and time")),
        }
        let time = parse::time(&mut c, false)?;
        let offset = parse::rfc3339_offset(&mut c)?;
        c.end()?;
        Ok(OffsetDateTime {
            local: DateTime::new(date, time),
            offset,
        })
    }

    /// Formats as RFC 3339 with the shortest exact fraction (none, 3, 6 or 9 digits) and `Z`
    /// for UTC: `2026-11-15T03:20:30Z`, `2026-11-15T10:20:30.500+07:00`.
    ///
    /// The output is valid RFC 3339 for years 0000 to 9999 and whole-minute offsets.
    /// Negative years are written `-YYYY` and an offset with a seconds component as
    /// `±HH:MM:SS` (as Python's `isoformat` does); neither can come from
    /// [`parse_rfc3339`](Self::parse_rfc3339).
    pub fn to_rfc3339(self) -> String {
        let mut s = String::with_capacity(36);
        // Writing to a String never fails.
        let _ = self.write_rfc3339(&mut s);
        s
    }

    fn write_rfc3339<W: fmt::Write>(self, w: &mut W) -> fmt::Result {
        self.local.write(w)?;
        if self.offset.is_utc() {
            w.write_char('Z')
        } else {
            self.offset.write(w)
        }
    }

    /// Formats as an HTTP date (RFC 9110 IMF-fixdate) in GMT:
    /// `Sun, 06 Nov 1994 08:49:37 GMT`. The fraction is dropped. Never panics, even when the
    /// UTC date is outside -9999..=9999.
    pub fn to_http_date(self) -> String {
        http::format(self.unix_seconds())
    }

    /// Parses an HTTP date in any of the three formats RFC 9110 requires recipients to accept:
    /// IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), obsolete RFC 850
    /// (`Sunday, 06-Nov-94 08:49:37 GMT`) and ANSI C `asctime()` (`Sun Nov  6 08:49:37 1994`).
    /// The result is in UTC.
    ///
    /// Names are case-sensitive as specified. The weekday is checked for syntax but not
    /// against the date. Two-digit RFC 850 years map 00–49 to 2000–2049 and 50–99 to
    /// 1950–1999, without consulting the clock.
    ///
    /// # Errors
    /// [`Error::Parse`] for bad syntax, [`Error::InvalidComponent`] for invalid values.
    pub fn parse_http_date(s: &str) -> Result<OffsetDateTime, Error> {
        http::parse(s)
    }

    /// Formats the local date-time and offset with a .NET-style pattern (English names).
    ///
    /// ```
    /// let t = dl_time::OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30+07:00")?;
    /// assert_eq!(t.format("yyyy-MM-dd HH:mm zzz")?, "2026-11-15 10:20 +07:00");
    /// # Ok::<(), dl_time::Error>(())
    /// ```
    ///
    /// # Errors
    /// [`Error::InvalidPattern`] for unknown letters.
    pub fn format(self, pattern: &str) -> Result<String, Error> {
        self.format_locale(pattern, Locale::En)
    }

    /// Formats with a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`] for unknown letters.
    pub fn format_locale(self, pattern: &str, locale: Locale) -> Result<String, Error> {
        let parts = Parts {
            date: Some(self.local.date()),
            time: Some(self.local.time()),
            offset: Some(self.offset),
        };
        pattern::format(parts, pattern, locale)
    }

    /// Parses `input` against a .NET-style pattern (English names). The pattern must contain
    /// a year and an offset token (`zzz`, `zz`, `z` or `K`).
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse(input: &str, pattern: &str) -> Result<OffsetDateTime, Error> {
        OffsetDateTime::parse_locale(input, pattern, Locale::En)
    }

    /// Parses `input` against a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse_locale(
        input: &str,
        pattern: &str,
        locale: Locale,
    ) -> Result<OffsetDateTime, Error> {
        let p = pattern::parse(input, pattern, locale, Kinds::ALL)?;
        Ok(OffsetDateTime {
            local: DateTime::new(p.date()?, p.time()?),
            offset: p.offset()?,
        })
    }
}

impl OffsetDateTime {
    /// The instant as (Unix seconds, nanoseconds), cheaper to compare than unix_nanos.
    const fn instant(self) -> (i64, u32) {
        (self.unix_seconds(), self.nanosecond())
    }
}

impl PartialEq for OffsetDateTime {
    /// Compares instants: `10:00+07:00 == 03:00Z`.
    fn eq(&self, other: &Self) -> bool {
        self.instant() == other.instant()
    }
}

impl Eq for OffsetDateTime {}

impl PartialOrd for OffsetDateTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OffsetDateTime {
    /// Orders by instant.
    fn cmp(&self, other: &Self) -> Ordering {
        self.instant().cmp(&other.instant())
    }
}

impl Hash for OffsetDateTime {
    /// Hashes the instant, consistent with `Eq`.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.instant().hash(state);
    }
}

impl fmt::Display for OffsetDateTime {
    /// RFC 3339, as [`to_rfc3339`](OffsetDateTime::to_rfc3339).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = buf::Buf::new();
        self.write_rfc3339(&mut b)?;
        f.pad(b.as_str())
    }
}

impl fmt::Debug for OffsetDateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for OffsetDateTime {
    type Err = Error;

    /// Same as [`OffsetDateTime::parse_rfc3339`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OffsetDateTime::parse_rfc3339(s)
    }
}

impl Add<Duration> for OffsetDateTime {
    type Output = OffsetDateTime;

    /// # Panics
    /// If the local result is outside the supported range; see
    /// [`OffsetDateTime::checked_add`].
    #[track_caller]
    fn add(self, rhs: Duration) -> OffsetDateTime {
        self.checked_add(rhs)
            .expect("OffsetDateTime + Duration overflow")
    }
}

impl Sub<Duration> for OffsetDateTime {
    type Output = OffsetDateTime;

    /// # Panics
    /// If the local result is outside the supported range; see
    /// [`OffsetDateTime::checked_sub`].
    #[track_caller]
    fn sub(self, rhs: Duration) -> OffsetDateTime {
        self.checked_sub(rhs)
            .expect("OffsetDateTime - Duration overflow")
    }
}

impl AddAssign<Duration> for OffsetDateTime {
    /// # Panics
    /// If the result is outside the supported range.
    #[track_caller]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl SubAssign<Duration> for OffsetDateTime {
    /// # Panics
    /// If the result is outside the supported range.
    #[track_caller]
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl Sub for OffsetDateTime {
    type Output = Duration;

    /// Same as [`OffsetDateTime::duration_since`]; never panics.
    fn sub(self, rhs: OffsetDateTime) -> Duration {
        self.duration_since(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    fn t(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse_rfc3339(s).unwrap()
    }

    #[test]
    fn equality_is_by_instant() {
        let a = t("2026-11-15T10:00:00+07:00");
        let b = t("2026-11-15T03:00:00Z");
        assert_eq!(a, b);
        assert_ne!(a.to_string(), b.to_string());
        assert_ne!(a.datetime(), b.datetime());
        assert!(t("2026-11-15T09:59:59+07:00") < b);
        use std::hash::{BuildHasher, RandomState};
        let s = RandomState::new();
        assert_eq!(s.hash_one(a), s.hash_one(b));
        let mut v = std::vec![t("2026-01-01T00:00:00Z"), a, t("2025-12-31T23:00:00-02:00")];
        v.sort();
        assert_eq!(v[0].to_string(), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn offset_conversion_across_midnight() {
        let utc = t("2026-12-31T20:30:00Z");
        let vn = utc.to_offset(Offset::VIETNAM);
        assert_eq!(vn.to_string(), "2027-01-01T03:30:00+07:00");
        assert_eq!(
            (vn.year(), vn.month(), vn.day(), vn.hour()),
            (2027, Month::January, 1, 3)
        );
        assert_eq!(vn.date().weekday(), Weekday::Friday);
        assert_eq!(vn.to_utc().to_string(), "2026-12-31T20:30:00Z");
        let ny = vn.to_offset(Offset::from_hms(-5, 0, 0).unwrap());
        assert_eq!(ny.to_string(), "2026-12-31T15:30:00-05:00");
        let early = t("2026-03-01T05:00:00+07:00").to_utc();
        assert_eq!(early.to_string(), "2026-02-28T22:00:00Z");
        assert_eq!(t("2024-03-01T01:00:00+07:00").to_utc().day(), 29);
        assert_eq!(vn.unix_seconds(), utc.unix_seconds());
    }

    #[test]
    fn range_edges() {
        let late = t("9999-12-31T23:00:00-05:00");
        assert_eq!(late.checked_to_offset(Offset::UTC), None);
        assert_eq!(late.unix_seconds(), 253_402_315_200);
        assert_eq!(late.to_http_date(), "Sat, 01 Jan 10000 04:00:00 GMT");
        let max = OffsetDateTime::from_unix_nanos(253_402_300_799_999_999_999).unwrap();
        assert_eq!(max.to_string(), "9999-12-31T23:59:59.999999999Z");
        assert_eq!(
            OffsetDateTime::from_unix_nanos(253_402_300_800_000_000_000),
            Err(Error::Overflow)
        );
        assert_eq!(max.checked_add(Duration::NANOSECOND), None);
        assert_eq!(
            max.checked_sub(Duration::NANOSECOND)
                .map(|x| x.nanosecond()),
            Some(999_999_998)
        );
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn to_offset_panics_out_of_range() {
        let _ = t("9999-12-31T23:00:00-05:00").to_utc();
    }

    #[test]
    fn arithmetic() {
        let a = t("2026-11-15T22:00:00+07:00");
        let b = a + Duration::from_hours(8);
        assert_eq!(b.to_string(), "2026-11-16T06:00:00+07:00");
        assert_eq!(b - a, Duration::from_hours(8));
        assert_eq!(b - Duration::from_hours(8), a);
        assert_eq!(
            t("2026-11-16T00:00:00Z") - t("2026-11-16T00:00:00+07:00"),
            Duration::from_hours(7)
        );
        let mut c = a;
        c += Duration::DAY;
        c -= Duration::HOUR;
        assert_eq!(c.to_string(), "2026-11-16T21:00:00+07:00");
    }

    #[cfg(all(
        feature = "std",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    #[test]
    fn now_is_plausible() {
        let now = OffsetDateTime::now_utc();
        assert!(now.year() >= 2024 && now.offset().is_utc());
        let vn = OffsetDateTime::now(Offset::VIETNAM);
        assert_eq!(vn.offset(), Offset::VIETNAM);
        assert!((vn - now).abs() < Duration::from_seconds(60));
    }
}
