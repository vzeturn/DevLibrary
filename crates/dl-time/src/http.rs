//! HTTP dates (RFC 9110 section 5.6.7).

use alloc::string::String;
use core::fmt::Write;

use crate::calendar::{civil_from_days, weekday_from_days};
use crate::parse::Cursor;
use crate::{Date, DateTime, Error, Month, OffsetDateTime, Time, Weekday, buf};

/// IMF-fixdate for a Unix timestamp. Works for any timestamp of a representable
/// `OffsetDateTime`, including UTC years just outside -9999..=9999.
pub(crate) fn format(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let sod = unix_seconds.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let mut s = String::with_capacity(29);
    s.push_str(weekday_from_days(days).short_name());
    s.push_str(", ");
    // Writing to a String never fails.
    let _ = buf::write2(&mut s, day);
    s.push(' ');
    s.push_str(Month::ALL[month as usize - 1].short_name());
    s.push(' ');
    let _ = buf::write_year(&mut s, year);
    s.push(' ');
    let _ = buf::write2(&mut s, (sod / 3600) as u8);
    s.push(':');
    let _ = buf::write2(&mut s, (sod / 60 % 60) as u8);
    let _ = write!(s, ":{:02} GMT", sod % 60);
    s
}

fn month(c: &mut Cursor<'_>) -> Result<u8, Error> {
    let rest = &c.s[c.pos..];
    for m in Month::ALL {
        if rest.starts_with(m.short_name().as_bytes()) {
            c.pos += 3;
            return Ok(m.number());
        }
    }
    Err(c.err("a month name such as 'Nov'"))
}

fn hms(c: &mut Cursor<'_>) -> Result<Time, Error> {
    let h = c.fixed(2, "two-digit hour")? as u8;
    c.expect(b':', "':' after hour")?;
    let m = c.fixed(2, "two-digit minute")? as u8;
    c.expect(b':', "':' after minute")?;
    let s = c.fixed(2, "two-digit second")? as u8;
    Time::from_hms(h, m, s)
}

pub(crate) fn parse(s: &str) -> Result<OffsetDateTime, Error> {
    let mut c = Cursor::new(s);
    let start = c.pos;
    while c.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
        c.pos += 1;
    }
    let name = &c.s[start..c.pos];
    let short = Weekday::ALL
        .iter()
        .any(|w| w.short_name().as_bytes() == name);
    let long = Weekday::ALL.iter().any(|w| w.name().as_bytes() == name);
    let bad_name = Error::Parse {
        offset: start,
        expected: "a weekday name such as 'Sun'",
    };
    let (year, month, day, time);
    if c.eat(b',') {
        c.expect(b' ', "' ' after ','")?;
        if short {
            // IMF-fixdate: Sun, 06 Nov 1994 08:49:37 GMT
            day = c.fixed(2, "two-digit day")? as u8;
            c.expect(b' ', "' ' after day")?;
            month = self::month(&mut c)?;
            c.expect(b' ', "' ' after month")?;
            year = c.fixed(4, "four-digit year")? as i32;
            c.expect(b' ', "' ' after year")?;
        } else if long {
            // RFC 850: Sunday, 06-Nov-94 08:49:37 GMT
            day = c.fixed(2, "two-digit day")? as u8;
            c.expect(b'-', "'-' after day")?;
            month = self::month(&mut c)?;
            c.expect(b'-', "'-' after month")?;
            let yy = c.fixed(2, "two-digit year")? as i32;
            year = if yy < 50 { 2000 + yy } else { 1900 + yy };
            c.expect(b' ', "' ' after year")?;
        } else {
            return Err(bad_name);
        }
        time = hms(&mut c)?;
        c.expect_str(" GMT", "' GMT'")?;
    } else if short && c.eat(b' ') {
        // asctime: Sun Nov  6 08:49:37 1994
        month = self::month(&mut c)?;
        c.expect(b' ', "' ' after month")?;
        day = if c.eat(b' ') {
            c.fixed(1, "one-digit day")?
        } else {
            c.fixed(2, "two-digit day")?
        } as u8;
        c.expect(b' ', "' ' after day")?;
        time = hms(&mut c)?;
        c.expect(b' ', "' ' after time")?;
        year = c.fixed(4, "four-digit year")? as i32;
    } else if short || long {
        return Err(c.err("',' or ' ' after the weekday"));
    } else {
        return Err(bad_name);
    }
    c.end()?;
    Ok(DateTime::new(Date::new(year, month, day)?, time).assume_utc())
}

#[cfg(test)]
mod tests {
    use crate::{Error, OffsetDateTime};

    fn p(s: &str) -> Result<OffsetDateTime, Error> {
        OffsetDateTime::parse_http_date(s)
    }

    #[test]
    fn rfc9110_examples() {
        // RFC 9110 section 5.6.7: all three denote 784111777.
        for s in [
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(
                p(s).map(OffsetDateTime::unix_seconds),
                Ok(784_111_777),
                "{s}"
            );
        }
        let t = OffsetDateTime::from_unix_seconds(784_111_777).unwrap();
        assert_eq!(t.to_http_date(), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(p("Sun Nov 16 08:49:37 1994").map(|t| t.day()), Ok(16));
    }

    #[test]
    fn formatting() {
        let vn = OffsetDateTime::parse_rfc3339("2026-01-01T06:59:59.999+07:00").unwrap();
        assert_eq!(vn.to_http_date(), "Wed, 31 Dec 2025 23:59:59 GMT");
        assert_eq!(
            OffsetDateTime::UNIX_EPOCH.to_http_date(),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
        let neg = OffsetDateTime::from_unix_seconds(-1).unwrap();
        assert_eq!(neg.to_http_date(), "Wed, 31 Dec 1969 23:59:59 GMT");
        for secs in [
            0i64,
            951_782_400,
            1_794_712_830,
            -2_208_988_800,
            253_402_300_799,
        ] {
            let t = OffsetDateTime::from_unix_seconds(secs).unwrap();
            assert_eq!(p(&t.to_http_date()), Ok(t));
        }
    }

    #[test]
    fn two_digit_years() {
        assert_eq!(
            p("Thursday, 01-Jan-70 00:00:00 GMT").map(|t| t.year()),
            Ok(1970)
        );
        assert_eq!(
            p("Friday, 31-Dec-49 00:00:00 GMT").map(|t| t.year()),
            Ok(2049)
        );
        assert_eq!(
            p("Saturday, 01-Jan-50 00:00:00 GMT").map(|t| t.year()),
            Ok(1950)
        );
    }

    #[test]
    fn rejects() {
        let err = |offset, expected| Err(Error::Parse { offset, expected });
        assert_eq!(
            p("sun, 06 Nov 1994 08:49:37 GMT"),
            err(0, "a weekday name such as 'Sun'")
        );
        assert_eq!(
            p("Sun, 06 nov 1994 08:49:37 GMT"),
            err(8, "a month name such as 'Nov'")
        );
        assert_eq!(p("Sun, 06 Nov 1994 08:49:37 UTC"), err(25, "' GMT'"));
        assert_eq!(p("Sun, 06 Nov 1994 08:49:37 GMT "), err(29, "end of input"));
        assert_eq!(p("Sun, 6 Nov 1994 08:49:37 GMT"), err(5, "two-digit day"));
        assert_eq!(p("Sun 06 Nov 1994"), err(4, "a month name such as 'Nov'"));
        assert_eq!(
            p("Sunday 06 Nov 1994"),
            err(6, "',' or ' ' after the weekday")
        );
        assert_eq!(
            p("Sun, 31 Feb 1994 08:49:37 GMT"),
            Err(Error::component("day", 31))
        );
        assert_eq!(
            p("Sun, 06 Nov 1994 08:49:60 GMT"),
            Err(Error::component("second", 60))
        );
        assert_eq!(
            p("Sun, 06 Nov 1994 24:00:00 GMT"),
            Err(Error::component("hour", 24))
        );
        assert!(p("Sun,06 Nov 1994 08:49:37 GMT").is_err());
        assert!(p("Sun Nov 6 08:49:37 1994").is_err());
        assert!(p("").is_err());
        // The weekday is not checked against the date (as in Go's net/http).
        assert!(p("Mon, 06 Nov 1994 08:49:37 GMT").is_ok());
    }
}
