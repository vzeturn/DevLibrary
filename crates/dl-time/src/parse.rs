//! Byte cursor and the fixed-syntax ISO 8601 / RFC 3339 component parsers.

use crate::{Date, Error, Offset, Time};

/// Forward-only cursor over the input bytes. Offsets in errors are byte offsets.
pub(crate) struct Cursor<'a> {
    pub(crate) src: &'a str,
    pub(crate) s: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) const fn new(s: &'a str) -> Self {
        Cursor {
            src: s,
            s: s.as_bytes(),
            pos: 0,
        }
    }

    /// The unread input. Empty if `pos` is not on a character boundary (never the case:
    /// the cursor only advances over whole characters).
    pub(crate) fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    pub(crate) const fn err(&self, expected: &'static str) -> Error {
        Error::Parse {
            offset: self.pos,
            expected,
        }
    }

    pub(crate) fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub(crate) fn expect(&mut self, b: u8, expected: &'static str) -> Result<(), Error> {
        if self.eat(b) {
            Ok(())
        } else {
            Err(self.err(expected))
        }
    }

    pub(crate) fn expect_str(&mut self, lit: &str, expected: &'static str) -> Result<(), Error> {
        if self.s[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(self.err(expected))
        }
    }

    pub(crate) fn end(&self) -> Result<(), Error> {
        if self.pos == self.s.len() {
            Ok(())
        } else {
            Err(self.err("end of input"))
        }
    }

    /// Reads between `min` and `max` ASCII digits (greedy), `max <= 9`.
    pub(crate) fn digits(
        &mut self,
        min: usize,
        max: usize,
        expected: &'static str,
    ) -> Result<u32, Error> {
        let start = self.pos;
        let mut v = 0u32;
        while self.pos - start < max {
            match self.peek() {
                Some(b @ b'0'..=b'9') => {
                    v = v * 10 + (b - b'0') as u32;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if self.pos - start < min {
            return Err(Error::Parse {
                offset: start,
                expected,
            });
        }
        Ok(v)
    }

    /// Reads exactly `n` digits.
    pub(crate) fn fixed(&mut self, n: usize, expected: &'static str) -> Result<u32, Error> {
        self.digits(n, n, expected)
    }

    /// Reads 1 to 9 fraction digits and returns nanoseconds. A tenth digit is an error.
    pub(crate) fn fraction(&mut self) -> Result<u32, Error> {
        let start = self.pos;
        let v = self.digits(1, 9, "fraction digits")?;
        if matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(self.err("at most 9 fraction digits"));
        }
        Ok(v * 10u32.pow((9 - (self.pos - start)) as u32))
    }
}

/// `[+-]YYYY-MM-DD`; the sign is accepted only when `signed` is set.
pub(crate) fn date(c: &mut Cursor<'_>, signed: bool) -> Result<Date, Error> {
    let negative = signed && c.eat(b'-');
    if signed && !negative {
        c.eat(b'+');
    }
    let year = c.fixed(4, "four-digit year")? as i32;
    c.expect(b'-', "'-' after year")?;
    let month = c.fixed(2, "two-digit month")? as u8;
    c.expect(b'-', "'-' after month")?;
    let day = c.fixed(2, "two-digit day")? as u8;
    Date::new(if negative { -year } else { year }, month, day)
}

/// `HH:MM:SS[.f{1,9}]`, or also `HH:MM` when `seconds_optional`.
pub(crate) fn time(c: &mut Cursor<'_>, seconds_optional: bool) -> Result<Time, Error> {
    let hour = c.fixed(2, "two-digit hour")? as u8;
    c.expect(b':', "':' after hour")?;
    let minute = c.fixed(2, "two-digit minute")? as u8;
    let mut second = 0;
    let mut nanos = 0;
    if !seconds_optional || c.peek() == Some(b':') {
        c.expect(b':', "':' after minute")?;
        second = c.fixed(2, "two-digit second")? as u8;
        if c.eat(b'.') || (seconds_optional && c.eat(b',')) {
            nanos = c.fraction()?;
        }
    }
    Time::new(hour, minute, second, nanos)
}

/// `Z`, `z` or `(+|-)HH:MM`. `-00:00` (RFC 3339 "unknown local offset") maps to UTC.
pub(crate) fn rfc3339_offset(c: &mut Cursor<'_>) -> Result<Offset, Error> {
    match c.peek() {
        Some(b'Z' | b'z') => {
            c.pos += 1;
            Ok(Offset::UTC)
        }
        Some(sign @ (b'+' | b'-')) => {
            c.pos += 1;
            let h = c.fixed(2, "two-digit offset hour")? as i32;
            c.expect(b':', "':' in offset")?;
            let m = c.fixed(2, "two-digit offset minute")? as i32;
            if m > 59 {
                return Err(Error::component("offset minute", m as i64));
            }
            let secs = h * 3600 + m * 60;
            Offset::from_seconds(if sign == b'-' { -secs } else { secs })
        }
        _ => Err(c.err("'Z' or a numeric offset")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_digits() {
        let mut c = Cursor::new("12345x");
        assert_eq!(c.digits(1, 2, "d"), Ok(12));
        assert_eq!(c.fixed(3, "d"), Ok(345));
        assert_eq!(
            c.fixed(1, "d"),
            Err(Error::Parse {
                offset: 5,
                expected: "d"
            })
        );
        let mut c = Cursor::new("1234567890");
        assert!(c.fraction().is_err());
        let mut c = Cursor::new("5");
        assert_eq!(c.fraction(), Ok(500_000_000));
    }
}
