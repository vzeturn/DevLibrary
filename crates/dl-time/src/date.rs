use alloc::string::String;
use core::fmt;
use core::str::FromStr;

use crate::calendar::{
    CUMULATIVE, civil_from_days, days_from_civil, iso_weeks_in_year, month_len, weekday_from_days,
};
use crate::pattern::{self, Kinds, Parts};
use crate::{DateTime, Error, Locale, Month, Time, Weekday, buf, is_leap_year, parse};

pub(crate) const MIN_YEAR: i32 = -9999;
pub(crate) const MAX_YEAR: i32 = 9999;
pub(crate) const MIN_DAYS: i64 = days_from_civil(MIN_YEAR, 1, 1);
pub(crate) const MAX_DAYS: i64 = days_from_civil(MAX_YEAR, 12, 31);

/// A calendar date in the proleptic Gregorian calendar, years -9999 to 9999.
///
/// Year 0 exists (it is 1 BC), as in ISO 8601. Ordering is chronological.
///
/// ```
/// use dl_time::{Date, Month, Weekday};
///
/// let d = Date::new(2026, 11, 15)?;
/// assert_eq!((d.year(), d.month(), d.day()), (2026, Month::November, 15));
/// assert_eq!(d.weekday(), Weekday::Sunday);
/// assert_eq!(d.ordinal(), 319);
/// assert_eq!(d.iso_week(), (2026, 46));
/// assert_eq!(d.add_days(30).to_string(), "2026-12-15");
/// assert_eq!(Date::new(2024, 1, 31)?.add_months(1), Date::new(2024, 2, 29)?);
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    // Field order gives chronological derived ordering.
    year: i16,
    month: u8,
    day: u8,
}

impl Date {
    /// The earliest supported date, -9999-01-01.
    pub const MIN: Date = Date {
        year: MIN_YEAR as i16,
        month: 1,
        day: 1,
    };
    /// The latest supported date, 9999-12-31.
    pub const MAX: Date = Date {
        year: MAX_YEAR as i16,
        month: 12,
        day: 31,
    };
    /// 1970-01-01.
    pub const UNIX_EPOCH: Date = Date {
        year: 1970,
        month: 1,
        day: 1,
    };

    /// Creates a date from year, month (1 to 12) and day of month.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] naming `"year"`, `"month"` or `"day"` when a component
    /// is out of range, for example February 29 in a common year.
    pub const fn new(year: i32, month: u8, day: u8) -> Result<Date, Error> {
        if year < MIN_YEAR || year > MAX_YEAR {
            return Err(Error::component("year", year as i64));
        }
        if month < 1 || month > 12 {
            return Err(Error::component("month", month as i64));
        }
        if day < 1 || day > month_len(year, month) {
            return Err(Error::component("day", day as i64));
        }
        Ok(Date {
            year: year as i16,
            month,
            day,
        })
    }

    /// Caller guarantees the components form a valid date in range.
    pub(crate) const fn from_ymd_unchecked(year: i32, month: u8, day: u8) -> Date {
        Date {
            year: year as i16,
            month,
            day,
        }
    }

    /// Creates a date from a year and a day of the year (1 to 365 or 366).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] for an out-of-range year or ordinal.
    pub fn from_ordinal(year: i32, ordinal: u16) -> Result<Date, Error> {
        Date::new(year, 1, 1)?;
        let leap = is_leap_year(year) as u16;
        if ordinal < 1 || ordinal > 365 + leap {
            return Err(Error::component("ordinal", ordinal as i64));
        }
        let mut month = 12;
        while month > 1 {
            let before = CUMULATIVE[month - 1] + if month > 2 { leap } else { 0 };
            if ordinal > before {
                return Ok(Date::from_ymd_unchecked(
                    year,
                    month as u8,
                    (ordinal - before) as u8,
                ));
            }
            month -= 1;
        }
        Ok(Date::from_ymd_unchecked(year, 1, ordinal as u8))
    }

    /// Creates a date from an ISO 8601 week date: ISO week-numbering year, week (1 to 52
    /// or 53) and weekday.
    ///
    /// ```
    /// use dl_time::{Date, Weekday};
    ///
    /// // 2021-01-03 is the Sunday of week 53 of ISO year 2020.
    /// assert_eq!(Date::from_iso_week(2020, 53, Weekday::Sunday)?, Date::new(2021, 1, 3)?);
    /// # Ok::<(), dl_time::Error>(())
    /// ```
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] for a week that does not exist in `iso_year`, or
    /// [`Error::Overflow`] if the date falls outside the supported range.
    pub fn from_iso_week(iso_year: i32, week: u8, weekday: Weekday) -> Result<Date, Error> {
        if !(MIN_YEAR - 1..=MAX_YEAR + 1).contains(&iso_year) {
            return Err(Error::component("year", iso_year as i64));
        }
        if week < 1 || week > iso_weeks_in_year(iso_year) {
            return Err(Error::component("week", week as i64));
        }
        // Week 1 is the week containing January 4.
        let jan4 = days_from_civil(iso_year, 1, 4);
        let monday = jan4 - weekday_from_days(jan4).days_from_monday() as i64;
        let days = monday + (week as i64 - 1) * 7 + weekday.days_from_monday() as i64;
        Date::from_unix_days(days).map_err(|_| Error::Overflow)
    }

    /// Creates a date from the number of days since 1970-01-01 (negative before).
    ///
    /// # Errors
    /// [`Error::Overflow`] if the date is outside -9999-01-01..=9999-12-31.
    pub const fn from_unix_days(days: i64) -> Result<Date, Error> {
        if days < MIN_DAYS || days > MAX_DAYS {
            return Err(Error::Overflow);
        }
        let (y, m, d) = civil_from_days(days);
        Ok(Date::from_ymd_unchecked(y as i32, m, d))
    }

    /// Number of days since 1970-01-01 (negative before).
    pub const fn to_unix_days(self) -> i32 {
        days_from_civil(self.year as i32, self.month, self.day) as i32
    }

    /// The year (-9999 to 9999).
    pub const fn year(self) -> i32 {
        self.year as i32
    }

    /// The month.
    pub const fn month(self) -> Month {
        Month::ALL[self.month as usize - 1]
    }

    /// The day of the month (1 to 31).
    pub const fn day(self) -> u8 {
        self.day
    }

    /// The day of the year (1 to 366).
    pub const fn ordinal(self) -> u16 {
        let leap = (self.month > 2 && is_leap_year(self.year as i32)) as u16;
        CUMULATIVE[self.month as usize - 1] + leap + self.day as u16
    }

    /// The day of the week.
    pub const fn weekday(self) -> Weekday {
        weekday_from_days(self.to_unix_days() as i64)
    }

    /// The ISO 8601 week date as `(iso_year, week)`. The ISO year differs from
    /// [`year`](Self::year) for some days around January 1: 2021-01-01 is in week 53 of 2020.
    pub const fn iso_week(self) -> (i32, u8) {
        let year = self.year as i32;
        let ordinal = self.ordinal() as i32;
        let wd = self.weekday().iso_number() as i32;
        let week = (ordinal - wd + 10) / 7;
        if week < 1 {
            (year - 1, iso_weeks_in_year(year - 1))
        } else if week > iso_weeks_in_year(year) as i32 {
            (year + 1, 1)
        } else {
            (year, week as u8)
        }
    }

    /// Whether the date's year is a leap year.
    pub const fn is_leap_year(self) -> bool {
        is_leap_year(self.year as i32)
    }

    /// Number of days in the date's month (28 to 31).
    pub const fn days_in_month(self) -> u8 {
        month_len(self.year as i32, self.month)
    }

    /// Number of days in the date's year (365 or 366).
    pub const fn days_in_year(self) -> u16 {
        if self.is_leap_year() { 366 } else { 365 }
    }

    /// The date `days` days later (earlier if negative), or `None` outside the range.
    pub const fn checked_add_days(self, days: i64) -> Option<Date> {
        let Some(target) = (self.to_unix_days() as i64).checked_add(days) else {
            return None;
        };
        match Date::from_unix_days(target) {
            Ok(d) => Some(d),
            Err(_) => None,
        }
    }

    /// The date `days` days later (earlier if negative).
    ///
    /// # Panics
    /// If the result is outside the supported range; see
    /// [`checked_add_days`](Self::checked_add_days).
    #[track_caller]
    pub const fn add_days(self, days: i64) -> Date {
        match self.checked_add_days(days) {
            Some(d) => d,
            None => panic!("Date::add_days overflow"),
        }
    }

    /// The date `months` months later (earlier if negative), clamping the day to the end of
    /// the target month: January 31 + 1 month = February 28 (29 in leap years). Returns
    /// `None` outside the range.
    pub const fn checked_add_months(self, months: i32) -> Option<Date> {
        let total = self.year as i64 * 12 + (self.month as i64 - 1) + months as i64;
        let year = total.div_euclid(12);
        if year < MIN_YEAR as i64 || year > MAX_YEAR as i64 {
            return None;
        }
        let year = year as i32;
        let month = (total.rem_euclid(12) + 1) as u8;
        let len = month_len(year, month);
        let day = if self.day > len { len } else { self.day };
        Some(Date::from_ymd_unchecked(year, month, day))
    }

    /// The date `months` months later (earlier if negative), clamped to the month end.
    ///
    /// # Panics
    /// If the result is outside the supported range; see
    /// [`checked_add_months`](Self::checked_add_months).
    #[track_caller]
    pub const fn add_months(self, months: i32) -> Date {
        match self.checked_add_months(months) {
            Some(d) => d,
            None => panic!("Date::add_months overflow"),
        }
    }

    /// The same day `years` years later (earlier if negative); February 29 becomes
    /// February 28 in a common year. Returns `None` outside the range.
    pub const fn checked_add_years(self, years: i32) -> Option<Date> {
        match (years as i64).checked_mul(12) {
            Some(m) if m >= i32::MIN as i64 && m <= i32::MAX as i64 => {
                self.checked_add_months(m as i32)
            }
            _ => None,
        }
    }

    /// The same day `years` years later (earlier if negative), February 29 clamped.
    ///
    /// # Panics
    /// If the result is outside the supported range; see
    /// [`checked_add_years`](Self::checked_add_years).
    #[track_caller]
    pub const fn add_years(self, years: i32) -> Date {
        match self.checked_add_years(years) {
            Some(d) => d,
            None => panic!("Date::add_years overflow"),
        }
    }

    /// The first day of the date's month.
    pub const fn first_day_of_month(self) -> Date {
        Date { day: 1, ..self }
    }

    /// The last day of the date's month.
    pub const fn last_day_of_month(self) -> Date {
        Date {
            day: self.days_in_month(),
            ..self
        }
    }

    /// The most recent `first_day` on or before this date, or `None` if that falls before
    /// [`Date::MIN`].
    pub const fn checked_start_of_week(self, first_day: Weekday) -> Option<Date> {
        let back = (self.weekday().days_from_monday() as i64 - first_day.days_from_monday() as i64)
            .rem_euclid(7);
        self.checked_add_days(-back)
    }

    /// The most recent `first_day` on or before this date: `start_of_week(Weekday::Monday)`
    /// for ISO weeks, `start_of_week(Weekday::Sunday)` for US-style weeks.
    ///
    /// # Panics
    /// Only in the first days of year -9999 when the week start precedes [`Date::MIN`];
    /// see [`checked_start_of_week`](Self::checked_start_of_week).
    #[track_caller]
    pub const fn start_of_week(self, first_day: Weekday) -> Date {
        match self.checked_start_of_week(first_day) {
            Some(d) => d,
            None => panic!("Date::start_of_week overflow"),
        }
    }

    /// The next day, or `None` after [`Date::MAX`].
    pub const fn checked_succ(self) -> Option<Date> {
        self.checked_add_days(1)
    }

    /// The previous day, or `None` before [`Date::MIN`].
    pub const fn checked_pred(self) -> Option<Date> {
        self.checked_add_days(-1)
    }

    /// The next day.
    ///
    /// # Panics
    /// On [`Date::MAX`].
    #[track_caller]
    pub const fn succ(self) -> Date {
        self.add_days(1)
    }

    /// The previous day.
    ///
    /// # Panics
    /// On [`Date::MIN`].
    #[track_caller]
    pub const fn pred(self) -> Date {
        self.add_days(-1)
    }

    /// Number of days from `self` to `other`: positive if `other` is later. For a hotel stay
    /// this is the number of nights between check-in and check-out dates.
    pub const fn days_until(self, other: Date) -> i32 {
        other.to_unix_days() - self.to_unix_days()
    }

    /// Replaces the year.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if the year is out of range or the day does not exist
    /// (February 29 in a common year).
    pub const fn with_year(self, year: i32) -> Result<Date, Error> {
        Date::new(year, self.month, self.day)
    }

    /// Replaces the month (1 to 12).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if the month is invalid or the day does not exist in it.
    pub const fn with_month(self, month: u8) -> Result<Date, Error> {
        Date::new(self.year as i32, month, self.day)
    }

    /// Replaces the day of the month.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if the day does not exist in the month.
    pub const fn with_day(self, day: u8) -> Result<Date, Error> {
        Date::new(self.year as i32, self.month, day)
    }

    /// Combines the date with a time of day.
    pub const fn at(self, time: Time) -> DateTime {
        DateTime::new(self, time)
    }

    /// Combines the date with a time given as hour, minute and second.
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] if the time is invalid.
    pub const fn and_hms(self, hour: u8, minute: u8, second: u8) -> Result<DateTime, Error> {
        match Time::from_hms(hour, minute, second) {
            Ok(t) => Ok(DateTime::new(self, t)),
            Err(e) => Err(e),
        }
    }

    /// Midnight at the start of the date.
    pub const fn midnight(self) -> DateTime {
        DateTime::new(self, Time::MIDNIGHT)
    }

    /// Parses an ISO 8601 calendar date `YYYY-MM-DD`, with an optional `-` or `+` sign for
    /// the year. This is also the [`FromStr`] implementation.
    ///
    /// # Errors
    /// [`Error::Parse`] for bad syntax, [`Error::InvalidComponent`] for an invalid date.
    pub fn parse_iso(s: &str) -> Result<Date, Error> {
        let mut c = parse::Cursor::new(s);
        let d = parse::date(&mut c, true)?;
        c.end()?;
        Ok(d)
    }

    /// Formats with a .NET-style pattern using English names. See the crate documentation
    /// for the token table. Time and offset tokens are rejected.
    ///
    /// ```
    /// let d = dl_time::Date::new(2026, 11, 15)?;
    /// assert_eq!(d.format("dd/MM/yyyy")?, "15/11/2026");
    /// assert_eq!(d.format("ddd, MMM d yyyy")?, "Sun, Nov 15 2026");
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
                date: Some(self),
                time: None,
                offset: None,
            },
            pattern,
            locale,
        )
    }

    /// Parses `input` against a .NET-style pattern (English names). The pattern must contain
    /// a year; a missing month or day defaults to 1.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse(input: &str, pattern: &str) -> Result<Date, Error> {
        Date::parse_locale(input, pattern, Locale::En)
    }

    /// Parses `input` against a .NET-style pattern using the names of `locale`.
    ///
    /// # Errors
    /// [`Error::InvalidPattern`], [`Error::Parse`] or [`Error::InvalidComponent`].
    pub fn parse_locale(input: &str, pattern: &str, locale: Locale) -> Result<Date, Error> {
        pattern::parse(input, pattern, locale, Kinds::DATE)?.date()
    }

    pub(crate) fn write<W: fmt::Write>(self, w: &mut W) -> fmt::Result {
        buf::write_year(w, self.year as i64)?;
        w.write_char('-')?;
        buf::write2(w, self.month)?;
        w.write_char('-')?;
        buf::write2(w, self.day)
    }
}

impl fmt::Display for Date {
    /// ISO 8601 `YYYY-MM-DD` (negative years as `-YYYY-MM-DD`). Honors width and alignment.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = buf::Buf::new();
        self.write(&mut b)?;
        f.pad(b.as_str())
    }
}

impl fmt::Debug for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Date {
    type Err = Error;

    /// Same as [`Date::parse_iso`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Date::parse_iso(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    fn d(y: i32, m: u8, day: u8) -> Date {
        Date::new(y, m, day).unwrap()
    }

    #[test]
    fn constructors_validate() {
        assert_eq!(Date::new(2023, 2, 29), Err(Error::component("day", 29)));
        assert!(Date::new(2024, 2, 29).is_ok());
        assert_eq!(Date::new(2024, 13, 1), Err(Error::component("month", 13)));
        assert_eq!(Date::new(2024, 0, 1), Err(Error::component("month", 0)));
        assert_eq!(Date::new(2024, 4, 31), Err(Error::component("day", 31)));
        assert_eq!(Date::new(2024, 4, 0), Err(Error::component("day", 0)));
        assert_eq!(Date::new(10000, 1, 1), Err(Error::component("year", 10000)));
        assert_eq!(
            Date::new(-10000, 1, 1),
            Err(Error::component("year", -10000))
        );
        assert_eq!(Date::MIN, d(-9999, 1, 1));
        assert_eq!(Date::MAX, d(9999, 12, 31));
    }

    #[test]
    fn ordinal_round_trip() {
        assert_eq!(Date::from_ordinal(2024, 60), Ok(d(2024, 2, 29)));
        assert_eq!(Date::from_ordinal(2023, 60), Ok(d(2023, 3, 1)));
        assert_eq!(Date::from_ordinal(2024, 366), Ok(d(2024, 12, 31)));
        assert_eq!(
            Date::from_ordinal(2023, 366),
            Err(Error::component("ordinal", 366))
        );
        assert_eq!(
            Date::from_ordinal(2023, 0),
            Err(Error::component("ordinal", 0))
        );
        assert_eq!(Date::from_ordinal(2023, 1), Ok(d(2023, 1, 1)));
        for y in [2023, 2024] {
            let mut date = d(y, 1, 1);
            for ord in 1..=date.days_in_year() {
                assert_eq!(date.ordinal(), ord);
                assert_eq!(Date::from_ordinal(y, ord), Ok(date));
                date = date.succ();
            }
        }
    }

    #[test]
    fn month_end_clamping() {
        assert_eq!(d(2023, 1, 31).add_months(1), d(2023, 2, 28));
        assert_eq!(d(2024, 1, 31).add_months(1), d(2024, 2, 29));
        assert_eq!(d(2024, 3, 31).add_months(-1), d(2024, 2, 29));
        assert_eq!(d(2024, 5, 31).add_months(1), d(2024, 6, 30));
        assert_eq!(d(2024, 12, 15).add_months(1), d(2025, 1, 15));
        assert_eq!(d(2024, 1, 15).add_months(-1), d(2023, 12, 15));
        assert_eq!(d(2024, 1, 15).add_months(-25), d(2021, 12, 15));
        assert_eq!(d(2024, 8, 31).add_months(6), d(2025, 2, 28));
        assert_eq!(d(2024, 2, 29).add_years(1), d(2025, 2, 28));
        assert_eq!(d(2024, 2, 29).add_years(4), d(2028, 2, 29));
        assert_eq!(d(2024, 2, 29).add_years(-100), d(1924, 2, 29));
        assert_eq!(d(2000, 2, 29).add_years(100), d(2100, 2, 28));
        assert_eq!(Date::MAX.checked_add_months(1), None);
        assert_eq!(Date::MIN.checked_add_months(-1), None);
        assert_eq!(Date::MAX.checked_add_years(i32::MAX), None);
        assert_eq!(Date::MIN.checked_add_years(i32::MIN), None);
        assert_eq!(d(2024, 6, 1).checked_add_months(i32::MAX), None);
        assert_eq!(d(9999, 1, 31).checked_add_months(11), Some(Date::MAX));
    }

    #[test]
    fn day_arithmetic_and_bounds() {
        assert_eq!(d(2026, 11, 15).add_days(30), d(2026, 12, 15));
        assert_eq!(d(2024, 2, 28).succ(), d(2024, 2, 29));
        assert_eq!(d(2024, 3, 1).pred(), d(2024, 2, 29));
        assert_eq!(Date::MAX.checked_succ(), None);
        assert_eq!(Date::MIN.checked_pred(), None);
        assert_eq!(Date::MIN.checked_add_days(i64::MIN), None);
        assert_eq!(Date::MAX.checked_add_days(i64::MAX), None);
        assert_eq!(Date::MIN.days_until(Date::MAX), 7_304_483);
        assert_eq!(d(2026, 11, 15).days_until(d(2026, 11, 18)), 3);
        assert_eq!(d(2026, 11, 18).days_until(d(2026, 11, 15)), -3);
        assert_eq!(Date::from_unix_days(MAX_DAYS + 1), Err(Error::Overflow));
        assert_eq!(Date::from_unix_days(MIN_DAYS - 1), Err(Error::Overflow));
        assert_eq!(Date::UNIX_EPOCH.to_unix_days(), 0);
    }

    #[test]
    #[should_panic(expected = "overflow")]
    fn add_days_panics_on_overflow() {
        let _ = Date::MAX.add_days(1);
    }

    #[test]
    fn month_helpers() {
        let x = d(2024, 2, 10);
        assert_eq!(x.first_day_of_month(), d(2024, 2, 1));
        assert_eq!(x.last_day_of_month(), d(2024, 2, 29));
        assert_eq!(d(2023, 2, 10).last_day_of_month(), d(2023, 2, 28));
        assert_eq!(x.days_in_month(), 29);
        assert_eq!(x.days_in_year(), 366);
        assert!(x.is_leap_year());
        assert_eq!(x.with_day(31), Err(Error::component("day", 31)));
        assert_eq!(x.with_month(3), Ok(d(2024, 3, 10)));
        assert_eq!(
            d(2024, 2, 29).with_year(2023),
            Err(Error::component("day", 29))
        );
    }

    #[test]
    fn start_of_week() {
        // 2026-11-15 is a Sunday.
        let sun = d(2026, 11, 15);
        assert_eq!(sun.start_of_week(Weekday::Monday), d(2026, 11, 9));
        assert_eq!(sun.start_of_week(Weekday::Sunday), sun);
        assert_eq!(
            d(2026, 11, 16).start_of_week(Weekday::Monday),
            d(2026, 11, 16)
        );
        assert_eq!(
            d(2026, 11, 14).start_of_week(Weekday::Saturday),
            d(2026, 11, 14)
        );
        assert_eq!(
            d(2026, 11, 13).start_of_week(Weekday::Saturday),
            d(2026, 11, 7)
        );
        // -9999-01-01 is a Monday (proleptic), so Sunday-start weeks begin before MIN.
        assert_eq!(Date::MIN.weekday(), Weekday::Monday);
        assert_eq!(Date::MIN.checked_start_of_week(Weekday::Sunday), None);
        assert_eq!(
            Date::MIN.checked_start_of_week(Weekday::Monday),
            Some(Date::MIN)
        );
    }

    #[test]
    fn iso_week_boundaries() {
        // Python: date(...).isocalendar()
        let cases = [
            ((2021, 1, 1), (2020, 53, Weekday::Friday)),
            ((2021, 1, 3), (2020, 53, Weekday::Sunday)),
            ((2021, 1, 4), (2021, 1, Weekday::Monday)),
            ((2024, 12, 30), (2025, 1, Weekday::Monday)),
            ((2024, 12, 29), (2024, 52, Weekday::Sunday)),
            ((2026, 12, 31), (2026, 53, Weekday::Thursday)),
            ((2027, 1, 3), (2026, 53, Weekday::Sunday)),
            ((2008, 12, 29), (2009, 1, Weekday::Monday)),
            ((2010, 1, 3), (2009, 53, Weekday::Sunday)),
            ((2005, 1, 2), (2004, 53, Weekday::Sunday)),
        ];
        for ((y, m, day), (iy, w, wd)) in cases {
            let date = d(y, m, day);
            assert_eq!(date.iso_week(), (iy, w), "{date}");
            assert_eq!(date.weekday(), wd, "{date}");
            assert_eq!(Date::from_iso_week(iy, w, wd), Ok(date));
        }
        assert_eq!(
            Date::from_iso_week(2021, 53, Weekday::Monday),
            Err(Error::component("week", 53))
        );
        assert_eq!(
            Date::from_iso_week(2021, 0, Weekday::Monday),
            Err(Error::component("week", 0))
        );
        // Python: date(9999, 12, 31).isocalendar() == (9999, 52, 5)
        assert_eq!(
            Date::from_iso_week(9999, 52, Weekday::Friday),
            Ok(Date::MAX)
        );
        assert_eq!(
            Date::from_iso_week(9999, 52, Weekday::Saturday),
            Err(Error::Overflow)
        );
        assert_eq!(
            Date::from_iso_week(10000, 1, Weekday::Sunday),
            Err(Error::Overflow)
        );
        assert!(Date::from_iso_week(i32::MAX, 1, Weekday::Sunday).is_err());
        assert_eq!(Date::MAX.iso_week().0, 9999);
        assert_eq!(Date::MIN.iso_week(), (-9999, 1));
    }

    #[test]
    fn display_and_parse() {
        assert_eq!(d(2026, 11, 15).to_string(), "2026-11-15");
        assert_eq!(d(-44, 3, 15).to_string(), "-0044-03-15");
        assert_eq!(d(7, 1, 2).to_string(), "0007-01-02");
        assert_eq!(std::format!("{:>12}", d(2026, 1, 2)), "  2026-01-02");
        assert_eq!(std::format!("{:?}", d(2026, 1, 2)), "2026-01-02");
        for s in [
            "2026-11-15",
            "-0044-03-15",
            "0000-02-29",
            "9999-12-31",
            "-9999-01-01",
        ] {
            assert_eq!(s.parse::<Date>().unwrap().to_string(), s);
        }
        assert_eq!("+2026-11-15".parse::<Date>(), Ok(d(2026, 11, 15)));
        assert_eq!(
            "2026-1-15".parse::<Date>(),
            Err(Error::Parse {
                offset: 5,
                expected: "two-digit month"
            })
        );
        assert_eq!(
            "2026-02-30".parse::<Date>(),
            Err(Error::component("day", 30))
        );
        assert_eq!(
            "2026-11-15T".parse::<Date>(),
            Err(Error::Parse {
                offset: 10,
                expected: "end of input"
            })
        );
        assert!("".parse::<Date>().is_err());
        assert!("20261115".parse::<Date>().is_err());
    }
}
