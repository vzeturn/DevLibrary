//! Month and weekday enums plus the raw proleptic Gregorian calendar algorithms.

use core::fmt;
use core::str::FromStr;

use crate::Error;

/// Returns `true` if `year` is a leap year in the proleptic Gregorian calendar
/// (divisible by 4, except centuries not divisible by 400).
///
/// ```
/// assert!(dl_time::is_leap_year(2024));
/// assert!(!dl_time::is_leap_year(1900));
/// assert!(dl_time::is_leap_year(2000));
/// ```
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Number of days in `month` of `year` (28 to 31).
pub(crate) const fn month_len(year: i32, month: u8) -> u8 {
    match month {
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days before the first of each month in a common year.
pub(crate) const CUMULATIVE: [u16; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

/// Days since 1970-01-01 of a valid civil date (Howard Hinnant's `days_from_civil`).
pub(crate) const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let y = year as i64 - (month <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let m = month as i64;
    let mp = if m > 2 { m - 3 } else { m + 9 }; // March = 0
    let doy = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Civil date of a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
/// Valid for any `days` with `|days| < 2^62`.
pub(crate) const fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    (yoe + era * 400 + (month <= 2) as i64, month, day)
}

/// Weekday of a day count since 1970-01-01 (a Thursday).
pub(crate) const fn weekday_from_days(days: i64) -> Weekday {
    Weekday::from_index((days + 3).rem_euclid(7) as u8)
}

/// Number of ISO 8601 weeks (52 or 53) in `iso_year`.
pub(crate) const fn iso_weeks_in_year(iso_year: i32) -> u8 {
    let jan1 = weekday_from_days(days_from_civil(iso_year, 1, 1));
    match jan1 {
        Weekday::Thursday => 53,
        Weekday::Wednesday if is_leap_year(iso_year) => 53,
        _ => 52,
    }
}

/// Case-insensitive ASCII match of a name or its first three letters.
fn match_name<T: Copy>(s: &str, items: &[T], name: fn(T) -> &'static str) -> Option<T> {
    items.iter().copied().find(|&item| {
        let full = name(item);
        s.eq_ignore_ascii_case(full) || s.eq_ignore_ascii_case(&full[..3])
    })
}

/// A month of the Gregorian calendar. The discriminant is the month number (1 to 12).
///
/// ```
/// use dl_time::Month;
///
/// assert_eq!(Month::November.number(), 11);
/// assert_eq!(Month::from_number(2), Ok(Month::February));
/// assert_eq!(Month::December.succ(), Month::January);
/// assert_eq!("sep".parse::<Month>(), Ok(Month::September));
/// assert_eq!(Month::February.length(2024), 29);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum Month {
    January = 1,
    February,
    March,
    April,
    May,
    June,
    July,
    August,
    September,
    October,
    November,
    December,
}

impl Month {
    /// All months, January first.
    pub const ALL: [Month; 12] = [
        Month::January,
        Month::February,
        Month::March,
        Month::April,
        Month::May,
        Month::June,
        Month::July,
        Month::August,
        Month::September,
        Month::October,
        Month::November,
        Month::December,
    ];

    /// Month number, 1 (January) to 12 (December).
    pub const fn number(self) -> u8 {
        self as u8
    }

    /// Month from its number (1 to 12).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] with name `"month"` if `n` is not in 1..=12.
    pub const fn from_number(n: u8) -> Result<Month, Error> {
        if n >= 1 && n <= 12 {
            Ok(Month::ALL[n as usize - 1])
        } else {
            Err(Error::component("month", n as i64))
        }
    }

    /// English name, for example `"January"`.
    pub const fn name(self) -> &'static str {
        const NAMES: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        NAMES[self as usize - 1]
    }

    /// Three-letter English abbreviation, for example `"Jan"`.
    pub const fn short_name(self) -> &'static str {
        const NAMES: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        NAMES[self as usize - 1]
    }

    /// The following month; December wraps to January.
    pub const fn succ(self) -> Month {
        Month::ALL[self as usize % 12]
    }

    /// The preceding month; January wraps to December.
    pub const fn pred(self) -> Month {
        Month::ALL[(self as usize + 10) % 12]
    }

    /// Number of days in this month of `year` (28 to 31).
    pub const fn length(self, year: i32) -> u8 {
        month_len(year, self as u8)
    }
}

impl fmt::Display for Month {
    /// Writes the English name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

impl FromStr for Month {
    type Err = Error;

    /// Parses an English name or three-letter abbreviation, ignoring ASCII case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match_name(s, &Month::ALL, Month::name).ok_or(Error::Parse {
            offset: 0,
            expected: "an English month name",
        })
    }
}

impl TryFrom<u8> for Month {
    type Error = Error;

    fn try_from(n: u8) -> Result<Self, Self::Error> {
        Month::from_number(n)
    }
}

impl From<Month> for u8 {
    fn from(m: Month) -> u8 {
        m as u8
    }
}

/// A day of the week. Ordered and numbered as in ISO 8601: Monday = 1, Sunday = 7.
///
/// ```
/// use dl_time::Weekday;
///
/// assert_eq!(Weekday::Sunday.iso_number(), 7);
/// assert_eq!(Weekday::Sunday.days_from_sunday(), 0); // JavaScript getDay(), .NET DayOfWeek
/// assert_eq!(Weekday::Monday.days_from_monday(), 0); // Python weekday()
/// assert_eq!(Weekday::Sunday.succ(), Weekday::Monday);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[allow(missing_docs)]
pub enum Weekday {
    Monday = 1,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// All weekdays, Monday first.
    pub const ALL: [Weekday; 7] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];

    /// `index` is days from Monday, already reduced to 0..=6.
    pub(crate) const fn from_index(index: u8) -> Weekday {
        Weekday::ALL[index as usize % 7]
    }

    /// ISO 8601 number: Monday = 1 ... Sunday = 7.
    pub const fn iso_number(self) -> u8 {
        self as u8
    }

    /// Days since Monday: Monday = 0 ... Sunday = 6 (Python `date.weekday()`).
    pub const fn days_from_monday(self) -> u8 {
        self as u8 - 1
    }

    /// Days since Sunday: Sunday = 0 ... Saturday = 6 (JavaScript `getDay()`,
    /// .NET `DayOfWeek`).
    pub const fn days_from_sunday(self) -> u8 {
        self as u8 % 7
    }

    /// Weekday from its ISO 8601 number (Monday = 1 ... Sunday = 7).
    ///
    /// # Errors
    /// [`Error::InvalidComponent`] with name `"weekday"` if `n` is not in 1..=7.
    pub const fn from_iso_number(n: u8) -> Result<Weekday, Error> {
        if n >= 1 && n <= 7 {
            Ok(Weekday::ALL[n as usize - 1])
        } else {
            Err(Error::component("weekday", n as i64))
        }
    }

    /// English name, for example `"Monday"`.
    pub const fn name(self) -> &'static str {
        const NAMES: [&str; 7] = [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday",
        ];
        NAMES[self as usize - 1]
    }

    /// Three-letter English abbreviation, for example `"Mon"`.
    pub const fn short_name(self) -> &'static str {
        const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        NAMES[self as usize - 1]
    }

    /// The following day; Sunday wraps to Monday.
    pub const fn succ(self) -> Weekday {
        Weekday::from_index(self as u8 % 7)
    }

    /// The preceding day; Monday wraps to Sunday.
    pub const fn pred(self) -> Weekday {
        Weekday::from_index((self as u8 + 5) % 7)
    }
}

impl fmt::Display for Weekday {
    /// Writes the English name.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

impl FromStr for Weekday {
    type Err = Error;

    /// Parses an English name or three-letter abbreviation, ignoring ASCII case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match_name(s, &Weekday::ALL, Weekday::name).ok_or(Error::Parse {
            offset: 0,
            expected: "an English weekday name",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leap_rules() {
        for (y, leap) in [
            (2024, true),
            (2023, false),
            (1900, false),
            (2000, true),
            (2100, false),
            (2400, true),
            (0, true),
            (-4, true),
            (-100, false),
            (-400, true),
            (-1, false),
        ] {
            assert_eq!(is_leap_year(y), leap, "{y}");
        }
    }

    #[test]
    fn hinnant_epoch_and_known_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // Python: date(1, 1, 1).toordinal() - 719163
        assert_eq!(days_from_civil(1, 1, 1), -719_162);
        assert_eq!(weekday_from_days(0), Weekday::Thursday);
    }

    #[test]
    fn month_api() {
        for (i, m) in Month::ALL.iter().enumerate() {
            assert_eq!(m.number() as usize, i + 1);
            assert_eq!(Month::from_number(m.number()), Ok(*m));
            assert_eq!(m.succ().pred(), *m);
            assert_eq!(m.name().parse::<Month>(), Ok(*m));
            assert_eq!(m.short_name().to_ascii_uppercase().parse::<Month>(), Ok(*m));
        }
        assert!(Month::from_number(0).is_err());
        assert!(Month::from_number(13).is_err());
        assert_eq!(Month::January.pred(), Month::December);
        assert!("Janu".parse::<Month>().is_err());
        assert_eq!(Month::try_from(4), Ok(Month::April));
        assert_eq!(u8::from(Month::May), 5);
        assert_eq!(std::format!("{:>5}", Month::May), "  May");
    }

    #[test]
    fn weekday_api() {
        for (i, w) in Weekday::ALL.iter().enumerate() {
            assert_eq!(w.days_from_monday() as usize, i);
            assert_eq!(Weekday::from_iso_number(w.iso_number()), Ok(*w));
            assert_eq!(w.succ().pred(), *w);
            assert_eq!(w.name().parse::<Weekday>(), Ok(*w));
            assert_eq!(w.short_name().parse::<Weekday>(), Ok(*w));
        }
        assert_eq!(Weekday::Saturday.days_from_sunday(), 6);
        assert_eq!(Weekday::Monday.pred(), Weekday::Sunday);
        assert!(Weekday::from_iso_number(0).is_err());
        assert!(Weekday::from_iso_number(8).is_err());
        assert!("xyz".parse::<Weekday>().is_err());
    }

    #[test]
    fn weeks_in_year() {
        // Python: date(y, 12, 28).isocalendar().week
        assert_eq!(iso_weeks_in_year(2020), 53);
        assert_eq!(iso_weeks_in_year(2015), 53);
        assert_eq!(iso_weeks_in_year(2026), 53);
        assert_eq!(iso_weeks_in_year(2024), 52);
        assert_eq!(iso_weeks_in_year(2021), 52);
    }
}
