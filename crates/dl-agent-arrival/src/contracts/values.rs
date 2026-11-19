//! The .NET parsers the request validator relies on, with their exact leniencies:
//! `bool.TryParse`, `DateTime.TryParseExact` for the two fixed formats and
//! `Convert.TryFromBase64String`. Numbers live in [`crate::netdec`].

use dl_time::{Date, DateTime, Time};

/// `bool.TryParse`: `True` / `False` in any case, surrounding white space and NULs ignored.
pub fn parse_bool(s: &str) -> Option<bool> {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if t.eq_ignore_ascii_case("true") {
        Some(true)
    } else if t.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn digits(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(b.iter().fold(0u32, |v, d| v * 10 + u32::from(d - b'0')))
}

fn date_part(b: &[u8]) -> Option<Date> {
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, d) = (digits(&b[..4])?, digits(&b[5..7])?, digits(&b[8..10])?);
    if y == 0 {
        return None;
    }
    Date::new(
        i32::try_from(y).ok()?,
        u8::try_from(m).ok()?,
        u8::try_from(d).ok()?,
    )
    .ok()
}

/// `DateTime.TryParseExact(s, "yyyy-MM-dd", Invariant, None)`.
pub fn parse_date(s: &str) -> Option<Date> {
    date_part(s.as_bytes())
}

/// `DateTime.TryParseExact(s, "yyyy-MM-dd'T'HH:mm:ss", Invariant, None)`.
pub fn parse_datetime(s: &str) -> Option<DateTime> {
    let b = s.as_bytes();
    if b.len() != 19 || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let date = date_part(&b[..10])?;
    let (h, mi, sec) = (
        digits(&b[11..13])?,
        digits(&b[14..16])?,
        digits(&b[17..19])?,
    );
    let time = Time::from_hms(
        u8::try_from(h).ok()?,
        u8::try_from(mi).ok()?,
        u8::try_from(sec).ok()?,
    )
    .ok()?;
    Some(DateTime::new(date, time))
}

fn sextet(c: u8) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// `Convert.TryFromBase64String`: white space (`' '`, `\t`, `\r`, `\n`) anywhere, up to
/// two `=` at the end only, unused trailing bits ignored.
pub fn parse_base64(s: &str) -> Option<Vec<u8>> {
    let chars: Vec<u8> = s
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        .collect();
    if !chars.len().is_multiple_of(4) {
        return None;
    }
    let pad = chars.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return None;
    }
    let body = &chars[..chars.len() - pad];
    let mut out = Vec::with_capacity(chars.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in body {
        acc = (acc << 6) | u32::from(sextet(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// `Convert.ToBase64String`.
pub fn to_base64(bytes: &[u8]) -> String {
    dl_encoding::base64::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_as_lenient_as_dotnet() {
        assert_eq!(parse_base64("A Q\nI D"), Some(vec![1, 2, 3]));
        assert_eq!(parse_base64("AR=="), Some(vec![1]));
        assert_eq!(parse_base64("AQ="), None);
        assert_eq!(parse_base64("AA==AA=="), None);
        assert_eq!(parse_base64("A==="), None);
    }

    #[test]
    fn dates_need_the_exact_format() {
        assert!(parse_date("2024-02-29").is_some());
        assert!(parse_date("2023-02-29").is_none());
        assert!(parse_datetime("2026-11-19T23:59:59").is_some());
        assert!(parse_datetime("2026-11-19T24:00:00").is_none());
        assert_eq!(parse_bool("\ttrue\0"), Some(true));
    }
}
