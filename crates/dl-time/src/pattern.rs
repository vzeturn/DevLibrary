//! .NET-style custom format patterns (`dd/MM/yyyy HH:mm:ss`) and locales.

use alloc::string::String;
use core::iter::Peekable;

use crate::buf::write_padded;
use crate::parse::Cursor;
use crate::{Date, Error, Month, Offset, Time, Weekday};

/// Language of month names, weekday names and AM/PM designators in custom patterns.
///
/// | | `MMMM` | `MMM` | `dddd` | `ddd` | `tt` |
/// |---|---|---|---|---|---|
/// | [`Locale::En`] | November | Nov | Sunday | Sun | AM / PM |
/// | [`Locale::Vi`] | Tháng 11 | Thg 11 | Chủ Nhật | CN | SA / CH |
///
/// Numbers are always ASCII digits and literal characters are never translated.
///
/// ```
/// use dl_time::{Date, Locale, Weekday};
///
/// let d = Date::new(2026, 11, 15)?;
/// assert_eq!(d.format_locale("dddd, 'ngày' dd/MM/yyyy", Locale::Vi)?, "Chủ Nhật, ngày 15/11/2026");
/// assert_eq!(Locale::Vi.weekday_name(Weekday::Monday), "Thứ Hai");
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Locale {
    /// English (invariant culture names).
    #[default]
    En,
    /// Vietnamese: months `Tháng 1` to `Tháng 12`, weekdays `Thứ Hai` to `Thứ Bảy` and
    /// `Chủ Nhật`, `SA` (sáng) / `CH` (chiều).
    Vi,
}

const VI_MONTHS: [&str; 12] = [
    "Tháng 1",
    "Tháng 2",
    "Tháng 3",
    "Tháng 4",
    "Tháng 5",
    "Tháng 6",
    "Tháng 7",
    "Tháng 8",
    "Tháng 9",
    "Tháng 10",
    "Tháng 11",
    "Tháng 12",
];
const VI_MONTHS_SHORT: [&str; 12] = [
    "Thg 1", "Thg 2", "Thg 3", "Thg 4", "Thg 5", "Thg 6", "Thg 7", "Thg 8", "Thg 9", "Thg 10",
    "Thg 11", "Thg 12",
];
const VI_WEEKDAYS: [&str; 7] = [
    "Thứ Hai",
    "Thứ Ba",
    "Thứ Tư",
    "Thứ Năm",
    "Thứ Sáu",
    "Thứ Bảy",
    "Chủ Nhật",
];
const VI_WEEKDAYS_SHORT: [&str; 7] = ["T2", "T3", "T4", "T5", "T6", "T7", "CN"];

impl Locale {
    /// Full month name (`MMMM`).
    pub const fn month_name(self, month: Month) -> &'static str {
        match self {
            Locale::En => month.name(),
            Locale::Vi => VI_MONTHS[month as usize - 1],
        }
    }

    /// Abbreviated month name (`MMM`).
    pub const fn month_short_name(self, month: Month) -> &'static str {
        match self {
            Locale::En => month.short_name(),
            Locale::Vi => VI_MONTHS_SHORT[month as usize - 1],
        }
    }

    /// Full weekday name (`dddd`).
    pub const fn weekday_name(self, weekday: Weekday) -> &'static str {
        match self {
            Locale::En => weekday.name(),
            Locale::Vi => VI_WEEKDAYS[weekday as usize - 1],
        }
    }

    /// Abbreviated weekday name (`ddd`).
    pub const fn weekday_short_name(self, weekday: Weekday) -> &'static str {
        match self {
            Locale::En => weekday.short_name(),
            Locale::Vi => VI_WEEKDAYS_SHORT[weekday as usize - 1],
        }
    }

    /// AM/PM designator (`tt`): before noon if `pm` is false.
    pub const fn am_pm(self, pm: bool) -> &'static str {
        match (self, pm) {
            (Locale::En, false) => "AM",
            (Locale::En, true) => "PM",
            (Locale::Vi, false) => "SA",
            (Locale::Vi, true) => "CH",
        }
    }
}

/// Which parts a value can supply to a pattern.
#[derive(Clone, Copy)]
pub(crate) struct Kinds {
    date: bool,
    time: bool,
    offset: bool,
}

impl Kinds {
    pub(crate) const DATE: Kinds = Kinds {
        date: true,
        time: false,
        offset: false,
    };
    pub(crate) const TIME: Kinds = Kinds {
        date: false,
        time: true,
        offset: false,
    };
    pub(crate) const DATE_TIME: Kinds = Kinds {
        date: true,
        time: true,
        offset: false,
    };
    pub(crate) const ALL: Kinds = Kinds {
        date: true,
        time: true,
        offset: true,
    };
}

/// The value being formatted.
pub(crate) struct Parts {
    pub(crate) date: Option<Date>,
    pub(crate) time: Option<Time>,
    pub(crate) offset: Option<Offset>,
}

impl Parts {
    fn kinds(&self) -> Kinds {
        Kinds {
            date: self.date.is_some(),
            time: self.time.is_some(),
            offset: self.offset.is_some(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Year,
    Month,
    /// `d`, `dd` day of month; `ddd`, `dddd` weekday.
    Day,
    Hour24,
    Hour12,
    Minute,
    Second,
    Frac,
    FracOpt,
    AmPm,
    Offset,
    OffsetK,
}

impl Field {
    fn check(self, kinds: Kinds, at: usize) -> Result<(), Error> {
        let (ok, reason) = match self {
            Field::Year | Field::Month | Field::Day => (
                kinds.date,
                "date field in a pattern for a value without a date",
            ),
            Field::Offset | Field::OffsetK => (
                kinds.offset,
                "offset field in a pattern for a value without an offset",
            ),
            _ => (
                kinds.time,
                "time field in a pattern for a value without a time",
            ),
        };
        if ok {
            Ok(())
        } else {
            Err(Error::InvalidPattern { offset: at, reason })
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Item<'a> {
    Lit(&'a str),
    Tok(Field, u8),
}

/// Tokenizer over a pattern. Yields `(byte offset in pattern, item)`.
struct Items<'a> {
    pat: &'a str,
    pos: usize,
}

impl<'a> Items<'a> {
    fn new(pat: &'a str) -> Peekable<Items<'a>> {
        Items { pat, pos: 0 }.peekable()
    }

    fn fail(
        &mut self,
        offset: usize,
        reason: &'static str,
    ) -> Option<Result<(usize, Item<'a>), Error>> {
        self.pos = self.pat.len();
        Some(Err(Error::InvalidPattern { offset, reason }))
    }
}

impl<'a> Iterator for Items<'a> {
    type Item = Result<(usize, Item<'a>), Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.pat.as_bytes();
        let start = self.pos;
        let b = *bytes.get(start)?;
        match b {
            b'\'' | b'"' => {
                let Some(len) = bytes[start + 1..].iter().position(|&x| x == b) else {
                    return self.fail(start, "unterminated quoted literal");
                };
                self.pos = start + len + 2;
                // Quotes are ASCII, so both slice ends are character boundaries.
                Some(Ok((
                    start,
                    Item::Lit(&self.pat[start + 1..start + 1 + len]),
                )))
            }
            b'\\' => {
                let Some(ch) = self.pat[start + 1..].chars().next() else {
                    return self.fail(start, "backslash at end of pattern");
                };
                self.pos = start + 1 + ch.len_utf8();
                Some(Ok((start, Item::Lit(&self.pat[start + 1..self.pos]))))
            }
            b if b.is_ascii_alphabetic() => {
                let count = bytes[start..].iter().take_while(|&&x| x == b).count();
                self.pos = start + count;
                let (field, max) = match b {
                    b'y' => (Field::Year, 5),
                    b'M' => (Field::Month, 4),
                    b'd' => (Field::Day, 4),
                    b'H' => (Field::Hour24, 2),
                    b'h' => (Field::Hour12, 2),
                    b'm' => (Field::Minute, 2),
                    b's' => (Field::Second, 2),
                    b'f' => (Field::Frac, 9),
                    b'F' => (Field::FracOpt, 9),
                    b't' => (Field::AmPm, 2),
                    b'z' => (Field::Offset, 3),
                    b'K' => (Field::OffsetK, 1),
                    _ => return self.fail(start, "unknown pattern letter (quote literal text)"),
                };
                if count > max {
                    return self.fail(start, "too many repeated pattern letters");
                }
                Some(Ok((start, Item::Tok(field, count as u8))))
            }
            _ => {
                let len = bytes[start..]
                    .iter()
                    .take_while(|&&x| {
                        !(x.is_ascii_alphabetic() || matches!(x, b'\'' | b'"' | b'\\'))
                    })
                    .count();
                self.pos = start + len;
                // The run stops before an ASCII byte or at the end: a character boundary.
                Some(Ok((start, Item::Lit(&self.pat[start..self.pos]))))
            }
        }
    }
}

const POW10: [u32; 10] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
];

fn write_offset(out: &mut String, offset: Offset, n: u8) {
    let abs = offset.whole_seconds().unsigned_abs();
    out.push(if offset.is_negative() { '-' } else { '+' });
    let _ = write_padded(out, (abs / 3600) as u64, if n == 1 { 1 } else { 2 });
    if n == 3 {
        out.push(':');
        let _ = write_padded(out, (abs / 60 % 60) as u64, 2);
    }
}

/// Formats `parts` with `pattern`.
pub(crate) fn format(parts: Parts, pattern: &str, locale: Locale) -> Result<String, Error> {
    let kinds = parts.kinds();
    let date = parts.date.unwrap_or(Date::UNIX_EPOCH);
    let time = parts.time.unwrap_or(Time::MIDNIGHT);
    let offset = parts.offset.unwrap_or(Offset::UTC);
    let mut out = String::with_capacity(pattern.len() + 16);
    for item in Items::new(pattern) {
        let (at, item) = item?;
        let (field, n) = match item {
            Item::Lit(s) => {
                out.push_str(s);
                continue;
            }
            Item::Tok(field, n) => (field, n),
        };
        field.check(kinds, at)?;
        let w = n as usize;
        match field {
            Field::Year => {
                let y = date.year();
                if n <= 2 {
                    num(&mut out, y.unsigned_abs() % 100, w);
                } else {
                    if y < 0 {
                        out.push('-');
                    }
                    num(&mut out, y.unsigned_abs(), w);
                }
            }
            Field::Month => match n {
                3 => out.push_str(locale.month_short_name(date.month())),
                4 => out.push_str(locale.month_name(date.month())),
                _ => num(&mut out, date.month().number() as u32, w),
            },
            Field::Day => match n {
                3 => out.push_str(locale.weekday_short_name(date.weekday())),
                4 => out.push_str(locale.weekday_name(date.weekday())),
                _ => num(&mut out, date.day() as u32, w),
            },
            Field::Hour24 => num(&mut out, time.hour() as u32, w),
            Field::Hour12 => {
                let h = time.hour() % 12;
                num(&mut out, if h == 0 { 12 } else { h as u32 }, w);
            }
            Field::Minute => num(&mut out, time.minute() as u32, w),
            Field::Second => num(&mut out, time.second() as u32, w),
            Field::Frac => num(&mut out, time.nanosecond() / POW10[9 - w], w),
            Field::FracOpt => {
                let mut v = time.nanosecond() / POW10[9 - w];
                let mut digits = w;
                while digits > 0 && v.is_multiple_of(10) {
                    v /= 10;
                    digits -= 1;
                }
                if digits > 0 {
                    num(&mut out, v, digits);
                } else if out.ends_with('.') {
                    // Like .NET, drop a decimal point directly before an empty fraction.
                    out.pop();
                }
            }
            Field::AmPm => {
                let s = locale.am_pm(time.hour() >= 12);
                out.push_str(if n == 1 { &s[..1] } else { s });
            }
            Field::Offset => write_offset(&mut out, offset, n),
            Field::OffsetK => {
                if offset.is_utc() {
                    out.push('Z');
                } else {
                    write_offset(&mut out, offset, 3);
                }
            }
        }
    }
    Ok(out)
}

fn num(out: &mut String, v: u32, width: usize) {
    // Writing to a String never fails.
    let _ = write_padded(out, v as u64, width);
}

/// Fields collected while parsing.
#[derive(Default)]
pub(crate) struct Parsed {
    pattern_len: usize,
    year: Option<i32>,
    month: Option<u8>,
    day: Option<u8>,
    weekday: Option<(Weekday, usize)>,
    hour: Option<u8>,
    hour12: Option<u8>,
    pm: Option<bool>,
    minute: Option<u8>,
    second: Option<u8>,
    nanos: Option<u32>,
    offset: Option<Offset>,
}

fn set<T: PartialEq + Copy>(slot: &mut Option<T>, value: T, at: usize) -> Result<(), Error> {
    match *slot {
        Some(old) if old != value => Err(Error::Parse {
            offset: at,
            expected: "the same value as an earlier field",
        }),
        _ => {
            *slot = Some(value);
            Ok(())
        }
    }
}

/// Byte length of `name` at the start of `input`, compared case-insensitively.
fn prefix_len(input: &str, name: &str) -> Option<usize> {
    let mut it = input.char_indices();
    for nc in name.chars() {
        let (_, ic) = it.next()?;
        if ic != nc && !ic.to_lowercase().eq(nc.to_lowercase()) {
            return None;
        }
    }
    Some(it.next().map_or(input.len(), |(i, _)| i))
}

/// Index of the longest name matching at the cursor; advances past it.
fn match_name(
    c: &mut Cursor<'_>,
    count: usize,
    name: impl Fn(usize) -> &'static str,
    expected: &'static str,
) -> Result<usize, Error> {
    let rest = c.rest();
    let best = (0..count)
        .filter_map(|i| prefix_len(rest, name(i)).map(|len| (len, i)))
        .max_by_key(|&(len, _)| len);
    match best {
        Some((len, i)) => {
            c.pos += len;
            Ok(i)
        }
        None => Err(c.err(expected)),
    }
}

fn two_digit_year(yy: u32) -> i32 {
    if yy < 50 {
        2000 + yy as i32
    } else {
        1900 + yy as i32
    }
}

fn numeric(c: &mut Cursor<'_>, n: u8, expected: &'static str) -> Result<u8, Error> {
    let min = if n == 1 { 1 } else { 2 };
    Ok(c.digits(min, 2, expected)? as u8)
}

fn parse_offset(c: &mut Cursor<'_>, n: u8) -> Result<Offset, Error> {
    let negative = match c.peek() {
        Some(b'+') => false,
        Some(b'-') => true,
        _ => return Err(c.err("'+' or '-' of an offset")),
    };
    c.pos += 1;
    let h = numeric(c, n.min(2), "offset hours")? as i32;
    let mut m = 0;
    if n == 3 {
        c.expect(b':', "':' in offset")?;
        m = c.fixed(2, "two-digit offset minutes")? as i32;
        if m > 59 {
            return Err(Error::component("offset minute", m as i64));
        }
    }
    let secs = h * 3600 + m * 60;
    Offset::from_seconds(if negative { -secs } else { secs })
}

/// Parses `input` with `pattern`, allowing only fields in `kinds`.
pub(crate) fn parse(
    input: &str,
    pattern: &str,
    locale: Locale,
    kinds: Kinds,
) -> Result<Parsed, Error> {
    // Validate the whole pattern first so pattern errors do not depend on the input.
    for item in Items::new(pattern) {
        if let (at, Item::Tok(field, _)) = item? {
            field.check(kinds, at)?;
        }
    }
    let mut c = Cursor::new(input);
    let mut p = Parsed {
        pattern_len: pattern.len(),
        ..Parsed::default()
    };
    let mut items = Items::new(pattern);
    while let Some(item) = items.next() {
        let (_, item) = item?;
        let (field, n) = match item {
            Item::Lit(s) => {
                // `.FFF` may format as nothing at all; accept a missing point then.
                let before_opt =
                    matches!(items.peek(), Some(Ok((_, Item::Tok(Field::FracOpt, _)))));
                if s == "." && before_opt && c.peek() != Some(b'.') {
                    continue;
                }
                c.expect_str(s, "literal text from the pattern")?;
                continue;
            }
            Item::Tok(field, n) => (field, n),
        };
        let start = c.pos;
        match field {
            Field::Year => {
                let y = match n {
                    1 => two_digit_year(c.digits(1, 2, "year")?),
                    2 => two_digit_year(c.fixed(2, "two-digit year")?),
                    _ => {
                        let negative = c.eat(b'-');
                        let w = n as usize;
                        let v = c.digits(w, w.max(4), "year digits")? as i32;
                        if negative { -v } else { v }
                    }
                };
                set(&mut p.year, y, start)?;
            }
            Field::Month => {
                let m = match n {
                    3 => {
                        let i = match_name(
                            &mut c,
                            12,
                            |i| locale.month_short_name(Month::ALL[i]),
                            "an abbreviated month name",
                        )?;
                        i as u8 + 1
                    }
                    4 => {
                        let i = match_name(
                            &mut c,
                            12,
                            |i| locale.month_name(Month::ALL[i]),
                            "a month name",
                        )?;
                        i as u8 + 1
                    }
                    _ => numeric(&mut c, n, "month digits")?,
                };
                set(&mut p.month, m, start)?;
            }
            Field::Day if n >= 3 => {
                let i = if n == 3 {
                    match_name(
                        &mut c,
                        7,
                        |i| locale.weekday_short_name(Weekday::ALL[i]),
                        "an abbreviated weekday name",
                    )?
                } else {
                    match_name(
                        &mut c,
                        7,
                        |i| locale.weekday_name(Weekday::ALL[i]),
                        "a weekday name",
                    )?
                };
                let wd = Weekday::ALL[i];
                match p.weekday {
                    Some((w, _)) if w != wd => {
                        return Err(Error::Parse {
                            offset: start,
                            expected: "the same value as an earlier field",
                        });
                    }
                    Some(_) => {}
                    None => p.weekday = Some((wd, start)),
                }
            }
            Field::Day => set(&mut p.day, numeric(&mut c, n, "day digits")?, start)?,
            Field::Hour24 => set(&mut p.hour, numeric(&mut c, n, "hour digits")?, start)?,
            Field::Hour12 => set(&mut p.hour12, numeric(&mut c, n, "hour digits")?, start)?,
            Field::Minute => set(&mut p.minute, numeric(&mut c, n, "minute digits")?, start)?,
            Field::Second => set(&mut p.second, numeric(&mut c, n, "second digits")?, start)?,
            Field::Frac | Field::FracOpt => {
                let w = n as usize;
                let min = if field == Field::Frac { w } else { 0 };
                let v = c.digits(min, w, "fraction digits")?;
                let read = c.pos - start;
                set(&mut p.nanos, v * POW10[9 - read], start)?;
            }
            Field::AmPm => {
                let rest = c.rest();
                let mut found = None;
                for pm in [false, true] {
                    let s = locale.am_pm(pm);
                    let s = if n == 1 { &s[..1] } else { s };
                    if let Some(len) = prefix_len(rest, s) {
                        found = Some((pm, len));
                        break;
                    }
                }
                let Some((pm, len)) = found else {
                    return Err(c.err("an AM/PM designator"));
                };
                c.pos += len;
                set(&mut p.pm, pm, start)?;
            }
            Field::Offset => set(&mut p.offset, parse_offset(&mut c, n)?, start)?,
            Field::OffsetK => {
                let o = if c.eat(b'Z') || c.eat(b'z') {
                    Offset::UTC
                } else {
                    parse_offset(&mut c, 3)?
                };
                set(&mut p.offset, o, start)?;
            }
        }
    }
    c.end()?;
    Ok(p)
}

impl Parsed {
    /// The date; the year is required, month and day default to 1.
    pub(crate) fn date(&self) -> Result<Date, Error> {
        let Some(year) = self.year else {
            return Err(Error::InvalidPattern {
                offset: self.pattern_len,
                reason: "pattern has no year (y)",
            });
        };
        let date = Date::new(year, self.month.unwrap_or(1), self.day.unwrap_or(1))?;
        match self.weekday {
            Some((w, at)) if w != date.weekday() => Err(Error::Parse {
                offset: at,
                expected: "the weekday of the parsed date",
            }),
            _ => Ok(date),
        }
    }

    /// The time; missing fields are 0, `h` without `tt` is read as AM.
    pub(crate) fn time(&self) -> Result<Time, Error> {
        let hour = match (self.hour12, self.hour) {
            (Some(h12), h24) => {
                if !(1..=12).contains(&h12) {
                    return Err(Error::component("hour", h12 as i64));
                }
                let h = h12 % 12 + if self.pm == Some(true) { 12 } else { 0 };
                if let Some(h24) = h24
                    && h24 != h
                {
                    return Err(Error::component("hour", h24 as i64));
                }
                h
            }
            (None, Some(h)) => {
                if let Some(pm) = self.pm
                    && h < 24
                    && (h >= 12) != pm
                {
                    return Err(Error::component("hour", h as i64));
                }
                h
            }
            (None, None) => 0,
        };
        Time::new(
            hour,
            self.minute.unwrap_or(0),
            self.second.unwrap_or(0),
            self.nanos.unwrap_or(0),
        )
    }

    /// The offset, which the pattern must contain.
    pub(crate) fn offset(&self) -> Result<Offset, Error> {
        self.offset.ok_or(Error::InvalidPattern {
            offset: self.pattern_len,
            reason: "pattern has no offset (zzz, zz, z or K)",
        })
    }
}
