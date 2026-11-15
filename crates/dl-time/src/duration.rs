use alloc::string::String;
use core::fmt::{self, Write};
use core::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};
use core::str::FromStr;

use crate::Error;
use crate::buf::{write_fraction_trimmed, write_padded};
use crate::parse::Cursor;

const NS_US: i128 = 1_000;
const NS_MS: i128 = 1_000_000;
const NS_S: i128 = 1_000_000_000;
const NS_MIN: i128 = 60 * NS_S;
const NS_H: i128 = 3600 * NS_S;
const NS_D: i128 = 86_400 * NS_S;
const NS_W: i128 = 7 * NS_D;
const MAX_NANOS: i128 = i64::MAX as i128 * NS_S + (NS_S - 1);

/// A signed span of time with nanosecond precision.
///
/// # Representation
/// A single `i128` count of nanoseconds, limited to ±(`i64::MAX` seconds + 999 999 999 ns)
/// (about ±292 billion years). One integer means arithmetic and comparison are plain integer
/// operations, every value has exactly one representation (no mixed-sign seconds/nanos pair
/// to normalize), and the derived `Ord` and `Hash` are correct. The bound keeps
/// [`whole_seconds`](Self::whole_seconds) in `i64` and makes the range symmetric, so
/// [`abs`](Self::abs) and negation never overflow.
///
/// A day is always 86 400 seconds: this crate has fixed UTC offsets only, so there are no
/// daylight-saving days of 23 or 25 hours.
///
/// Operators (`+`, `-`, `*`, `/`) panic on overflow; use the `checked_*` methods for values
/// derived from untrusted input.
///
/// ```
/// use dl_time::Duration;
///
/// let d = Duration::from_hours(26) + Duration::from_minutes(3) + Duration::from_millis(4_500);
/// assert_eq!(d.whole_days(), 1);
/// assert_eq!(d.to_string(), "1d2h3m4.5s");
/// assert_eq!(d.to_iso8601(), "P1DT2H3M4.5S");
/// assert_eq!("P1DT2H3M4.5S".parse::<Duration>()?, d);
/// assert_eq!((-d).abs(), d);
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Duration {
    nanos: i128,
}

/// Panicking constructor for `from_minutes` and larger units.
const fn scaled(n: i64, unit: i128) -> Duration {
    match Duration::from_nanos_i128(n as i128 * unit) {
        Some(d) => d,
        None => panic!("Duration overflow"),
    }
}

impl Duration {
    /// Zero length.
    pub const ZERO: Duration = Duration { nanos: 0 };
    /// The largest duration, `i64::MAX` seconds + 999 999 999 ns.
    pub const MAX: Duration = Duration { nanos: MAX_NANOS };
    /// The smallest (most negative) duration, `-Duration::MAX`.
    pub const MIN: Duration = Duration { nanos: -MAX_NANOS };
    /// One nanosecond.
    pub const NANOSECOND: Duration = Duration { nanos: 1 };
    /// One microsecond.
    pub const MICROSECOND: Duration = Duration { nanos: NS_US };
    /// One millisecond.
    pub const MILLISECOND: Duration = Duration { nanos: NS_MS };
    /// One second.
    pub const SECOND: Duration = Duration { nanos: NS_S };
    /// One minute.
    pub const MINUTE: Duration = Duration { nanos: NS_MIN };
    /// One hour.
    pub const HOUR: Duration = Duration { nanos: NS_H };
    /// One day of 24 hours.
    pub const DAY: Duration = Duration { nanos: NS_D };
    /// One week of 7 days.
    pub const WEEK: Duration = Duration { nanos: NS_W };

    /// A duration of `weeks` × 7 days.
    ///
    /// # Panics
    /// If the result exceeds [`Duration::MAX`] (|weeks| > about 1.5 × 10¹³).
    #[track_caller]
    pub const fn from_weeks(weeks: i64) -> Duration {
        scaled(weeks, NS_W)
    }

    /// A duration of `days` × 24 hours.
    ///
    /// # Panics
    /// If the result exceeds [`Duration::MAX`] (|days| > about 1.07 × 10¹⁴).
    #[track_caller]
    pub const fn from_days(days: i64) -> Duration {
        scaled(days, NS_D)
    }

    /// A duration of `hours` hours.
    ///
    /// # Panics
    /// If the result exceeds [`Duration::MAX`] (|hours| > about 2.6 × 10¹⁵).
    #[track_caller]
    pub const fn from_hours(hours: i64) -> Duration {
        scaled(hours, NS_H)
    }

    /// A duration of `minutes` minutes.
    ///
    /// # Panics
    /// If the result exceeds [`Duration::MAX`] (|minutes| > about 1.5 × 10¹⁷).
    #[track_caller]
    pub const fn from_minutes(minutes: i64) -> Duration {
        scaled(minutes, NS_MIN)
    }

    /// A duration of `seconds` seconds. Never panics.
    pub const fn from_seconds(seconds: i64) -> Duration {
        Duration {
            nanos: seconds as i128 * NS_S,
        }
    }

    /// A duration of `millis` milliseconds. Never panics.
    pub const fn from_millis(millis: i64) -> Duration {
        Duration {
            nanos: millis as i128 * NS_MS,
        }
    }

    /// A duration of `micros` microseconds. Never panics.
    pub const fn from_micros(micros: i64) -> Duration {
        Duration {
            nanos: micros as i128 * NS_US,
        }
    }

    /// A duration of `nanos` nanoseconds. Never panics.
    pub const fn from_nanos(nanos: i64) -> Duration {
        Duration {
            nanos: nanos as i128,
        }
    }

    /// A duration from an `i128` nanosecond count, or `None` outside
    /// [`MIN`](Self::MIN)..=[`MAX`](Self::MAX).
    pub const fn from_nanos_i128(nanos: i128) -> Option<Duration> {
        if nanos > MAX_NANOS || nanos < -MAX_NANOS {
            None
        } else {
            Some(Duration { nanos })
        }
    }

    /// Whole weeks, truncated toward zero.
    pub const fn whole_weeks(self) -> i64 {
        (self.nanos / NS_W) as i64
    }

    /// Whole days, truncated toward zero.
    pub const fn whole_days(self) -> i64 {
        (self.nanos / NS_D) as i64
    }

    /// Whole hours, truncated toward zero.
    pub const fn whole_hours(self) -> i64 {
        (self.nanos / NS_H) as i64
    }

    /// Whole minutes, truncated toward zero.
    pub const fn whole_minutes(self) -> i64 {
        (self.nanos / NS_MIN) as i64
    }

    /// Whole seconds, truncated toward zero.
    pub const fn whole_seconds(self) -> i64 {
        (self.nanos / NS_S) as i64
    }

    /// Whole milliseconds, truncated toward zero.
    pub const fn whole_milliseconds(self) -> i128 {
        self.nanos / NS_MS
    }

    /// Whole microseconds, truncated toward zero.
    pub const fn whole_microseconds(self) -> i128 {
        self.nanos / NS_US
    }

    /// Total nanoseconds.
    pub const fn whole_nanoseconds(self) -> i128 {
        self.nanos
    }

    /// The nanoseconds left after [`whole_seconds`](Self::whole_seconds); same sign as `self`.
    pub const fn subsec_nanoseconds(self) -> i32 {
        (self.nanos % NS_S) as i32
    }

    /// The duration in seconds as a float (rounded to the nearest `f64`).
    pub fn as_seconds_f64(self) -> f64 {
        self.whole_seconds() as f64 + self.subsec_nanoseconds() as f64 / 1e9
    }

    /// The absolute value. Never overflows because the range is symmetric.
    pub const fn abs(self) -> Duration {
        Duration {
            nanos: self.nanos.abs(),
        }
    }

    /// Whether the duration is zero.
    pub const fn is_zero(self) -> bool {
        self.nanos == 0
    }

    /// Whether the duration is less than zero.
    pub const fn is_negative(self) -> bool {
        self.nanos < 0
    }

    /// Whether the duration is greater than zero.
    pub const fn is_positive(self) -> bool {
        self.nanos > 0
    }

    /// `self + rhs`, or `None` on overflow.
    pub const fn checked_add(self, rhs: Duration) -> Option<Duration> {
        // Both operands are within ±MAX_NANOS (< 2^94), so the i128 sum cannot overflow.
        Duration::from_nanos_i128(self.nanos + rhs.nanos)
    }

    /// `self - rhs`, or `None` on overflow.
    pub const fn checked_sub(self, rhs: Duration) -> Option<Duration> {
        Duration::from_nanos_i128(self.nanos - rhs.nanos)
    }

    /// `self × rhs`, or `None` on overflow.
    pub const fn checked_mul(self, rhs: i64) -> Option<Duration> {
        match self.nanos.checked_mul(rhs as i128) {
            Some(n) => Duration::from_nanos_i128(n),
            None => None,
        }
    }

    /// `self / rhs` truncated toward zero, or `None` if `rhs` is zero.
    pub const fn checked_div(self, rhs: i64) -> Option<Duration> {
        if rhs == 0 {
            None
        } else {
            Some(Duration {
                nanos: self.nanos / rhs as i128,
            })
        }
    }

    /// ISO 8601 duration such as `P1DT2H3M4.5S`. Uses days (always 24 h), hours, minutes and
    /// seconds with up to nine fraction digits; zero is `PT0S`; negative durations get a
    /// leading `-` (as in XML Schema `xs:duration`). Years, months and weeks are never
    /// written.
    pub fn to_iso8601(self) -> String {
        let mut s = String::with_capacity(24);
        // Writing to a String never fails.
        let _ = self.write_iso8601(&mut s);
        s
    }

    fn write_iso8601<W: Write>(self, w: &mut W) -> fmt::Result {
        if self.nanos == 0 {
            return w.write_str("PT0S");
        }
        if self.nanos < 0 {
            w.write_char('-')?;
        }
        let n = self.nanos.unsigned_abs();
        let days = n / NS_D as u128;
        let rem = n % NS_D as u128;
        w.write_char('P')?;
        if days > 0 {
            write!(w, "{days}D")?;
        }
        if rem > 0 {
            w.write_char('T')?;
            let (h, m) = (rem / NS_H as u128, rem / NS_MIN as u128 % 60);
            let (s, ns) = (rem / NS_S as u128 % 60, (rem % NS_S as u128) as u32);
            if h > 0 {
                write!(w, "{h}H")?;
            }
            if m > 0 {
                write!(w, "{m}M")?;
            }
            if s > 0 || ns > 0 {
                write_padded(w, s as u64, 1)?;
                write_fraction_trimmed(w, ns)?;
                w.write_char('S')?;
            }
        }
        Ok(())
    }

    /// Parses an ISO 8601 duration: an optional `-` or `+` sign, `P`, then weeks `W` and/or
    /// days `D`, then optionally `T` with hours `H`, minutes `M` and seconds `S`, in that
    /// order. Examples: `P30D`, `PT8H`, `P2W`, `PT1H30M`, `P1DT2H3M4.5S`, `-PT15M`.
    /// This is also the [`FromStr`] implementation.
    ///
    /// The last component may have a fraction (`.` or `,`, up to 9 digits), such as `PT1.5H`.
    /// Combining `W` with other components is an ISO 8601-2 extension and is accepted.
    ///
    /// Years (`Y`) and months (`M` before `T`) are rejected: they have no fixed length, so they
    /// cannot become an exact duration. Use [`Date::add_months`](crate::Date::add_months) and
    /// [`Date::add_years`](crate::Date::add_years) for calendar arithmetic.
    ///
    /// # Errors
    /// [`Error::Parse`] for bad syntax or year/month components; [`Error::Overflow`] if the
    /// value exceeds [`Duration::MAX`].
    pub fn parse_iso8601(s: &str) -> Result<Duration, Error> {
        let mut c = Cursor::new(s);
        let negative = c.eat(b'-');
        if !negative {
            c.eat(b'+');
        }
        c.expect(b'P', "'P'")?;
        // Units in the order they must appear; index of the next allowed unit.
        const DATE_UNITS: [(u8, i128); 2] = [(b'W', NS_W), (b'D', NS_D)];
        const TIME_UNITS: [(u8, i128); 3] = [(b'H', NS_H), (b'M', NS_MIN), (b'S', NS_S)];
        let mut total: i128 = 0;
        let mut any = false;
        let mut in_time = false;
        let mut next = 0usize;
        loop {
            if c.peek().is_none() {
                break;
            }
            if !in_time && c.eat(b'T') {
                in_time = true;
                next = 0;
                if c.peek().is_none() {
                    return Err(c.err("a time component after 'T'"));
                }
                continue;
            }
            let (int, frac) = number(&mut c)?;
            let at = c.pos;
            let units: &[(u8, i128)] = if in_time { &TIME_UNITS } else { &DATE_UNITS };
            let designator = c.peek();
            let Some(idx) = units.iter().position(|&(d, _)| Some(d) == designator) else {
                let expected = match (in_time, designator) {
                    (false, Some(b'Y' | b'M')) => "W or D (years and months have no fixed length)",
                    (false, _) => "a unit designator W or D",
                    (true, _) => "a unit designator H, M or S",
                };
                return Err(Error::Parse {
                    offset: at,
                    expected,
                });
            };
            if idx < next {
                return Err(Error::Parse {
                    offset: at,
                    expected: "components in W, D, H, M, S order",
                });
            }
            next = idx + 1;
            c.pos += 1;
            let unit = units[idx].1;
            let value = int
                .checked_mul(unit)
                .and_then(|v| v.checked_add(frac.map_or(0, |(f, digits)| f * unit / digits)))
                .and_then(|v| total.checked_add(v))
                .ok_or(Error::Overflow)?;
            total = value;
            any = true;
            if frac.is_some() && c.peek().is_some() {
                return Err(c.err("end of input after a fractional component"));
            }
        }
        if !any {
            return Err(c.err("at least one duration component"));
        }
        Duration::from_nanos_i128(if negative { -total } else { total }).ok_or(Error::Overflow)
    }

    fn write_display<W: Write>(self, w: &mut W) -> fmt::Result {
        if self.nanos == 0 {
            return w.write_str("0s");
        }
        if self.nanos < 0 {
            w.write_char('-')?;
        }
        let n = self.nanos.unsigned_abs();
        if n < NS_S as u128 {
            let n = n as u64;
            let (unit, scale) = if n >= 1_000_000 {
                ("ms", 1_000_000)
            } else if n >= 1_000 {
                ("µs", 1_000)
            } else {
                ("ns", 1)
            };
            write_padded(w, n / scale, 1)?;
            // Scale the remainder to nanoseconds so the trimmed fraction is right.
            write_fraction_trimmed(w, ((n % scale) * (1_000_000_000 / scale)) as u32)?;
            return w.write_str(unit);
        }
        let days = n / NS_D as u128;
        let h = n / NS_H as u128 % 24;
        let m = n / NS_MIN as u128 % 60;
        let s = (n / NS_S as u128 % 60) as u64;
        let ns = (n % NS_S as u128) as u32;
        for (v, unit) in [(days, 'd'), (h, 'h'), (m, 'm')] {
            if v > 0 {
                write!(w, "{v}{unit}")?;
            }
        }
        if s > 0 || ns > 0 {
            write_padded(w, s, 1)?;
            write_fraction_trimmed(w, ns)?;
            w.write_char('s')?;
        }
        Ok(())
    }
}

/// Reads `digits[(.|,)digits]`. Returns the integer part and `(fraction, 10^digits)`.
fn number(c: &mut Cursor<'_>) -> Result<(i128, Option<(i128, i128)>), Error> {
    let start = c.pos;
    let mut int: i128 = 0;
    while let Some(b @ b'0'..=b'9') = c.peek() {
        int = int
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as i128))
            .ok_or(Error::Overflow)?;
        c.pos += 1;
    }
    if c.pos == start {
        return Err(c.err("a number"));
    }
    if c.eat(b'.') || c.eat(b',') {
        let fstart = c.pos;
        let f = c.digits(1, 9, "fraction digits")?;
        if matches!(c.peek(), Some(b'0'..=b'9')) {
            return Err(c.err("at most 9 fraction digits"));
        }
        let scale = 10i128.pow((c.pos - fstart) as u32);
        return Ok((int, Some((f as i128, scale))));
    }
    Ok((int, None))
}

impl fmt::Display for Duration {
    /// Compact human-readable form: `1d2h3m4.5s`, `-15m`, `250ms`, `1.5µs`, `0s`.
    /// Zero components are omitted; a day is 24 hours. Honors width and alignment.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut b = crate::buf::Buf::new();
        self.write_display(&mut b)?;
        f.pad(b.as_str())
    }
}

impl fmt::Debug for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Duration {
    type Err = Error;

    /// Same as [`Duration::parse_iso8601`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Duration::parse_iso8601(s)
    }
}

impl Neg for Duration {
    type Output = Duration;

    /// Never overflows because the range is symmetric.
    fn neg(self) -> Duration {
        Duration { nanos: -self.nanos }
    }
}

macro_rules! panicking_op {
    ($trait:ident, $method:ident, $rhs:ty, $checked:ident, $msg:literal) => {
        impl $trait<$rhs> for Duration {
            type Output = Duration;

            /// # Panics
            #[doc = $msg]
            #[track_caller]
            fn $method(self, rhs: $rhs) -> Duration {
                match self.$checked(rhs) {
                    Some(d) => d,
                    None => panic!($msg),
                }
            }
        }
    };
}

panicking_op!(
    Add,
    add,
    Duration,
    checked_add,
    "Duration overflow in addition"
);
panicking_op!(
    Sub,
    sub,
    Duration,
    checked_sub,
    "Duration overflow in subtraction"
);
panicking_op!(
    Mul,
    mul,
    i64,
    checked_mul,
    "Duration overflow in multiplication"
);
panicking_op!(Div, div, i64, checked_div, "Duration division by zero");

impl AddAssign for Duration {
    /// # Panics
    /// On overflow.
    #[track_caller]
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl SubAssign for Duration {
    /// # Panics
    /// On overflow.
    #[track_caller]
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl core::iter::Sum for Duration {
    /// # Panics
    /// On overflow.
    fn sum<I: Iterator<Item = Duration>>(iter: I) -> Duration {
        iter.fold(Duration::ZERO, |a, b| a + b)
    }
}

impl TryFrom<core::time::Duration> for Duration {
    type Error = Error;

    /// Converts an unsigned `core::time::Duration`.
    ///
    /// # Errors
    /// [`Error::Overflow`] if it exceeds [`Duration::MAX`] (more than `i64::MAX` seconds).
    fn try_from(d: core::time::Duration) -> Result<Self, Self::Error> {
        Duration::from_nanos_i128(d.as_nanos() as i128).ok_or(Error::Overflow)
    }
}

impl TryFrom<Duration> for core::time::Duration {
    type Error = Error;

    /// Converts to an unsigned `core::time::Duration`.
    ///
    /// # Errors
    /// [`Error::Overflow`] if the duration is negative.
    fn try_from(d: Duration) -> Result<Self, Self::Error> {
        if d.nanos < 0 {
            return Err(Error::Overflow);
        }
        Ok(core::time::Duration::new(
            d.whole_seconds() as u64,
            d.subsec_nanoseconds() as u32,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn constructors_and_getters() {
        assert_eq!(Duration::from_weeks(2), Duration::from_days(14));
        assert_eq!(Duration::from_days(1), Duration::from_hours(24));
        assert_eq!(Duration::from_hours(1), Duration::from_minutes(60));
        assert_eq!(Duration::from_minutes(1), Duration::from_seconds(60));
        assert_eq!(Duration::from_seconds(1), Duration::from_millis(1000));
        assert_eq!(Duration::from_millis(1), Duration::from_micros(1000));
        assert_eq!(Duration::from_micros(1), Duration::from_nanos(1000));
        let d = Duration::from_nanos(-90_061_500_000_000);
        assert_eq!(d.whole_days(), -1);
        assert_eq!(d.whole_hours(), -25);
        assert_eq!(d.whole_minutes(), -1501);
        assert_eq!(d.whole_seconds(), -90_061);
        assert_eq!(d.whole_milliseconds(), -90_061_500);
        assert_eq!(d.whole_microseconds(), -90_061_500_000);
        assert_eq!(d.subsec_nanoseconds(), -500_000_000);
        assert_eq!(d.whole_weeks(), 0);
        assert_eq!(d.as_seconds_f64(), -90_061.5);
        assert!(d.is_negative() && !d.is_positive() && !d.is_zero());
        assert_eq!(Duration::MAX.whole_seconds(), i64::MAX);
        assert_eq!(Duration::MIN.whole_seconds(), -i64::MAX);
        assert_eq!(Duration::from_seconds(i64::MIN).whole_seconds(), i64::MIN);
        assert_eq!(Duration::from_nanos_i128(MAX_NANOS + 1), None);
    }

    #[test]
    #[should_panic(expected = "Duration overflow")]
    fn from_days_overflow_panics() {
        let _ = Duration::from_days(i64::MAX);
    }

    #[test]
    fn checked_arithmetic() {
        let h = Duration::HOUR;
        assert_eq!(h.checked_add(h), Some(Duration::from_hours(2)));
        assert_eq!(
            h.checked_sub(Duration::from_hours(3)),
            Some(Duration::from_hours(-2))
        );
        assert_eq!(Duration::MAX.checked_add(Duration::NANOSECOND), None);
        assert_eq!(Duration::MIN.checked_sub(Duration::NANOSECOND), None);
        assert_eq!(Duration::MAX.checked_mul(2), None);
        assert_eq!(Duration::MAX.checked_mul(-1), Some(Duration::MIN));
        assert_eq!(Duration::MAX.checked_mul(i64::MIN), None);
        assert_eq!(h.checked_div(0), None);
        assert_eq!(h.checked_div(-4), Some(Duration::from_minutes(-15)));
        assert_eq!(Duration::MIN.abs(), Duration::MAX);
        assert_eq!(-Duration::MAX, Duration::MIN);
        let mut x = Duration::ZERO;
        x += h * 3;
        x -= h / 2;
        assert_eq!(x, Duration::from_minutes(150));
        let total: Duration = [h, h, Duration::MINUTE].into_iter().sum();
        assert_eq!(total, Duration::from_minutes(121));
    }

    #[test]
    #[should_panic(expected = "overflow in addition")]
    fn add_overflow_panics() {
        let _ = Duration::MAX + Duration::NANOSECOND;
    }

    #[test]
    fn std_conversions() {
        let std = core::time::Duration::new(5, 7);
        let d = Duration::try_from(std).unwrap();
        assert_eq!(d.whole_nanoseconds(), 5_000_000_007);
        assert_eq!(core::time::Duration::try_from(d), Ok(std));
        assert_eq!(core::time::Duration::try_from(-d), Err(Error::Overflow));
        assert_eq!(
            Duration::try_from(core::time::Duration::MAX),
            Err(Error::Overflow)
        );
    }

    #[test]
    fn display() {
        let cases = [
            (Duration::ZERO, "0s"),
            (Duration::from_nanos(42), "42ns"),
            (Duration::from_nanos(1_500), "1.5µs"),
            (Duration::from_millis(250), "250ms"),
            (Duration::from_nanos(1_234_567), "1.234567ms"),
            (Duration::from_seconds(1), "1s"),
            (Duration::from_minutes(-15), "-15m"),
            (Duration::from_seconds(3605), "1h5s"),
            (Duration::from_millis(93_784_500), "1d2h3m4.5s"),
            (Duration::from_days(400), "400d"),
            (Duration::from_nanos(-86_400_000_000_001), "-1d0.000000001s"),
        ];
        for (d, s) in cases {
            assert_eq!(d.to_string(), s);
        }
        assert_eq!(std::format!("{:>6}", Duration::HOUR), "    1h");
        assert_eq!(
            std::format!("{:?}", Duration::MIN),
            "-106751991167300d15h30m7.999999999s"
        );
    }

    #[test]
    fn iso8601_format() {
        let cases = [
            (Duration::ZERO, "PT0S"),
            (Duration::from_millis(93_784_500), "P1DT2H3M4.5S"),
            (Duration::from_days(30), "P30D"),
            (Duration::from_hours(8), "PT8H"),
            (Duration::from_minutes(-90), "-PT1H30M"),
            (Duration::from_nanos(1), "PT0.000000001S"),
            (Duration::from_seconds(59), "PT59S"),
            (Duration::from_days(1) + Duration::SECOND, "P1DT1S"),
        ];
        for (d, s) in cases {
            assert_eq!(d.to_iso8601(), s);
            assert_eq!(Duration::parse_iso8601(s), Ok(d), "{s}");
        }
        for d in [Duration::MAX, Duration::MIN] {
            assert_eq!(Duration::parse_iso8601(&d.to_iso8601()), Ok(d));
        }
    }

    #[test]
    fn iso8601_parse() {
        let ok = [
            ("P2W", Duration::from_weeks(2)),
            ("P1W2D", Duration::from_days(9)),
            ("PT1.5H", Duration::from_minutes(90)),
            ("PT0,5S", Duration::from_millis(500)),
            ("P0.5D", Duration::from_hours(12)),
            ("+PT15M", Duration::from_minutes(15)),
            ("-P1D", Duration::from_days(-1)),
            ("PT36H", Duration::from_hours(36)),
            ("P0D", Duration::ZERO),
            ("PT1M0.25S", Duration::from_millis(60_250)),
        ];
        for (s, d) in ok {
            assert_eq!(s.parse::<Duration>(), Ok(d), "{s}");
        }
        let err = |offset, expected| Err(Error::Parse { offset, expected });
        assert_eq!(
            "P1Y".parse::<Duration>(),
            err(2, "W or D (years and months have no fixed length)")
        );
        assert_eq!(
            "P2M".parse::<Duration>(),
            err(2, "W or D (years and months have no fixed length)")
        );
        assert_eq!(
            "P".parse::<Duration>(),
            err(1, "at least one duration component")
        );
        assert_eq!(
            "PT".parse::<Duration>(),
            err(2, "a time component after 'T'")
        );
        assert_eq!("1D".parse::<Duration>(), err(0, "'P'"));
        assert_eq!(
            "PT1D".parse::<Duration>(),
            err(3, "a unit designator H, M or S")
        );
        assert_eq!(
            "P1D2W".parse::<Duration>(),
            err(4, "components in W, D, H, M, S order")
        );
        assert_eq!(
            "PT1S2M".parse::<Duration>(),
            err(5, "components in W, D, H, M, S order")
        );
        assert_eq!(
            "PT1H1H".parse::<Duration>(),
            err(5, "components in W, D, H, M, S order")
        );
        assert_eq!(
            "PT1.5H30M".parse::<Duration>(),
            err(6, "end of input after a fractional component")
        );
        assert_eq!("PTH".parse::<Duration>(), err(2, "a number"));
        assert_eq!("P1".parse::<Duration>(), err(2, "a unit designator W or D"));
        assert_eq!(
            "P1DT".parse::<Duration>(),
            err(4, "a time component after 'T'")
        );
        assert_eq!("PT1.S".parse::<Duration>(), err(4, "fraction digits"));
        assert_eq!(
            "PT0.0000000001S".parse::<Duration>(),
            err(13, "at most 9 fraction digits")
        );
        assert_eq!(
            "P99999999999999999999D".parse::<Duration>(),
            Err(Error::Overflow)
        );
        assert_eq!(
            "P9999999999999999999999999999999999999999D".parse::<Duration>(),
            Err(Error::Overflow)
        );
        assert!("p1d".parse::<Duration>().is_err());
        assert!("".parse::<Duration>().is_err());
        assert!("-".parse::<Duration>().is_err());
    }
}
