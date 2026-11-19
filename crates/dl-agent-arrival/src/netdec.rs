//! .NET `decimal` parsing and text, exactly as the agent's contracts use them.
//!
//! Two parsers feed one conversion (`Number.TryNumberToDecimal`): [`parse_json`] reads a
//! JSON number the way `JsonElement.TryGetDecimal` does (the canonical row hash), and
//! [`parse_number_style`] reads a request parameter like
//! `decimal.TryParse(s, NumberStyles.Number, InvariantCulture)` (sign before or after,
//! thousands separators, no exponent). [`NetDecimal::g29`] is `ToString("G29")` and the
//! `Display` impl is `ToString()` (scale kept).

use std::fmt;

/// Digits the .NET decimal number buffer stores (`DecimalNumberBufferLength - 1`).
const MAX_DIGITS: usize = 30;
/// Decimal precision (`DecimalPrecision`).
const PRECISION: i32 = 29;

/// .NET's `NumberBuffer` for a decimal: significant digits, decimal-point position,
/// sign and whether digits past the buffer were non-zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NumberBuffer {
    digits: Vec<u8>,
    scale: i32,
    negative: bool,
    nonzero_tail: bool,
}

/// A .NET `decimal`: 96-bit magnitude, scale 0..=28 and a sign (negative zero exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetDecimal {
    /// The magnitude (below 2^96).
    pub mantissa: u128,
    /// Digits after the decimal point.
    pub scale: u8,
    /// The sign bit.
    pub negative: bool,
}

impl NetDecimal {
    /// `ToString("G29", InvariantCulture)`: trailing zeros dropped, `1E-05` below 0.0001.
    pub fn g29(&self) -> String {
        if self.mantissa == 0 {
            return "0".into();
        }
        let text = self.mantissa.to_string();
        let trimmed = text.trim_end_matches('0');
        let dig_pos = text.len() as i32 - i32::from(self.scale);
        crate::stj::num::general(self.negative, trimmed, dig_pos, PRECISION)
    }

    /// The value for a SQL `decimal` parameter (`None` past 38 digits, which .NET's
    /// 96-bit decimal never produces).
    pub fn to_sql_decimal(&self) -> Option<dl_decimal::Decimal> {
        dl_decimal::Decimal::from_parts(self.mantissa, self.negative, u32::from(self.scale)).ok()
    }
}

impl fmt::Display for NetDecimal {
    /// `ToString()`: the scale is kept (`1.50`), negative zero prints without a sign.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.mantissa.to_string();
        let scale = usize::from(self.scale);
        if self.negative && self.mantissa != 0 {
            f.write_str("-")?;
        }
        if scale == 0 {
            return f.write_str(&digits);
        }
        if digits.len() <= scale {
            f.write_str("0.")?;
            for _ in digits.len()..scale {
                f.write_str("0")?;
            }
            return f.write_str(&digits);
        }
        let (int, frac) = digits.split_at(digits.len() - scale);
        write!(f, "{int}.{frac}")
    }
}

/// A JSON number as `JsonElement.TryGetDecimal` reads it (`Utf8Parser`, exponent allowed);
/// `None` when it does not fit a decimal.
pub fn parse_json(raw: &str) -> Option<NetDecimal> {
    to_decimal(&utf8_number(raw.as_bytes())?)
}

/// `Utf8Parser.TryParseNumber` for a decimal buffer; the whole input must be consumed.
fn utf8_number(src: &[u8]) -> Option<NumberBuffer> {
    let n = src.len();
    let mut nb = NumberBuffer::default();
    let mut i = 0;
    let mut c = *src.first()?;
    if c == b'-' || c == b'+' {
        nb.negative = c == b'-';
        i += 1;
        c = *src.get(i)?;
    }
    let start_before = i;
    while i < n && src[i] == b'0' {
        i += 1;
    }
    if i == n {
        return Some(nb);
    }
    let start_nonlead = i;
    let mut count = 0usize;
    let mut tail = 0u8;
    while i < n {
        c = src[i];
        let v = c.wrapping_sub(b'0');
        if v > 9 {
            break;
        }
        i += 1;
        count += 1;
        if count >= MAX_DIGITS {
            tail |= v;
        }
    }
    nb.nonzero_tail = tail != 0;
    let before = i - start_before;
    let nonlead = i - start_nonlead;
    nb.digits
        .extend_from_slice(&src[start_nonlead..start_nonlead + nonlead.min(MAX_DIGITS)]);
    nb.scale = i32::try_from(nonlead).ok()?;
    if i == n {
        return Some(nb);
    }
    let mut after = 0;
    if c == b'.' {
        i += 1;
        let start_after = i;
        while i < n {
            c = src[i];
            let v = c.wrapping_sub(b'0');
            if v > 9 {
                break;
            }
            i += 1;
            count += 1;
            if count >= MAX_DIGITS {
                tail |= v;
            }
        }
        nb.nonzero_tail = tail != 0;
        after = i - start_after;
        let mut s = start_after;
        if nb.digits.is_empty() {
            while s < i && src[s] == b'0' {
                nb.scale -= 1;
                s += 1;
            }
        }
        let take = (i - s).min(MAX_DIGITS - nb.digits.len());
        nb.digits.extend_from_slice(&src[s..s + take]);
        if i == n {
            return (before != 0 || after != 0).then_some(nb);
        }
    }
    if before == 0 && after == 0 || (c & !0x20) != b'E' {
        return None;
    }
    i += 1;
    c = *src.get(i)?;
    let mut neg_exp = false;
    if c == b'-' || c == b'+' {
        neg_exp = c == b'-';
        i += 1;
        c = *src.get(i)?;
    }
    if c.wrapping_sub(b'0') > 9 {
        return None;
    }
    let mut exp: u64 = 0;
    while i < n && src[i].is_ascii_digit() {
        exp = (exp * 10 + u64::from(src[i] - b'0')).min(u64::from(u32::MAX) + 1);
        i += 1;
    }
    if i != n {
        return None;
    }
    let abs = i64::try_from(exp.min(u64::from(u32::MAX))).ok()?;
    let scale = i64::from(nb.scale);
    if neg_exp {
        nb.scale = i32::try_from(scale - abs).unwrap_or(i32::MIN);
    } else {
        nb.scale = i32::try_from(scale + abs).ok()?;
    }
    Some(nb)
}

/// Whitespace for .NET number parsing: U+0009..U+000D and U+0020.
fn is_white(c: char) -> bool {
    c == ' ' || ('\t'..='\r').contains(&c)
}

/// `decimal.TryParse(s, NumberStyles.Number, InvariantCulture)`.
pub fn parse_number_style(s: &str) -> Option<NetDecimal> {
    to_decimal(&style_number(s)?)
}

/// `Number.TryParseNumber` with `NumberStyles.Number` and invariant symbols, then the
/// trailing-NUL rule of the caller.
fn style_number(s: &str) -> Option<NumberBuffer> {
    let chars: Vec<char> = s.chars().collect();
    let at = |i: usize| chars.get(i).copied().unwrap_or('\0');
    let mut nb = NumberBuffer::default();
    let (mut sign, mut digits, mut nonzero, mut decimal) = (false, false, false, false);
    let mut i = 0;
    loop {
        let ch = at(i);
        if is_white(ch) && !sign {
        } else if !sign && (ch == '+' || ch == '-') {
            sign = true;
            nb.negative = ch == '-';
        } else {
            break;
        }
        i += 1;
    }
    let mut count = 0usize;
    loop {
        let ch = at(i);
        if ch.is_ascii_digit() {
            digits = true;
            if ch != '0' || nonzero {
                if count < MAX_DIGITS {
                    nb.digits.push(ch as u8);
                } else if ch != '0' {
                    nb.nonzero_tail = true;
                }
                if !decimal {
                    nb.scale += 1;
                }
                count += 1;
                nonzero = true;
            } else if decimal {
                nb.scale -= 1;
            }
        } else if ch == '.' && !decimal {
            decimal = true;
        } else if ch == ',' && digits && !decimal {
        } else {
            break;
        }
        i += 1;
    }
    if !digits {
        return None;
    }
    loop {
        let ch = at(i);
        if is_white(ch) {
        } else if !sign && (ch == '+' || ch == '-') {
            sign = true;
            nb.negative = ch == '-';
        } else {
            break;
        }
        i += 1;
    }
    chars[i.min(chars.len())..]
        .iter()
        .all(|&c| c == '\0')
        .then_some(nb)
}

/// `Number.TryNumberToDecimal`: `None` on overflow.
fn to_decimal(nb: &NumberBuffer) -> Option<NetDecimal> {
    let d = &nb.digits;
    let digit = |p: usize| d.get(p).copied().unwrap_or(0);
    let mut p = 0usize;
    let mut e = nb.scale;
    let mut c = digit(p);
    if c == 0 {
        let scale = (-i64::from(e)).clamp(0, 28) as u8;
        return Some(NetDecimal {
            mantissa: 0,
            scale,
            negative: nb.negative,
        });
    }
    if e > PRECISION {
        return None;
    }
    let mut low64: u64 = 0;
    while e > -28 {
        e -= 1;
        low64 = low64 * 10 + u64::from(c - b'0');
        p += 1;
        c = digit(p);
        if low64 >= u64::MAX / 10 {
            break;
        }
        if c == 0 {
            while e > 0 {
                e -= 1;
                low64 *= 10;
                if low64 >= u64::MAX / 10 {
                    break;
                }
            }
            break;
        }
    }
    let mut high: u32 = 0;
    while (e > 0 || (c != 0 && e > -28))
        && (high < u32::MAX / 10
            || (high == u32::MAX / 10
                && (low64 < 0x9999_9999_9999_9999
                    || (low64 == 0x9999_9999_9999_9999 && c <= b'5'))))
    {
        let tmp_low = u64::from(low64 as u32) * 10;
        let tmp64 = (low64 >> 32) * 10 + (tmp_low >> 32);
        low64 = (tmp_low & 0xFFFF_FFFF) + (tmp64 << 32);
        high = (tmp64 >> 32) as u32 + high * 10;
        if c != 0 {
            let v = u64::from(c - b'0');
            low64 = low64.wrapping_add(v);
            if low64 < v {
                high += 1;
            }
            p += 1;
            c = digit(p);
        }
        e -= 1;
    }
    let mut round = c >= b'5';
    if round && c == b'5' && low64 & 1 == 0 {
        p += 1;
        c = digit(p);
        let mut zero_tail = !nb.nonzero_tail;
        while c != 0 && zero_tail {
            zero_tail &= c == b'0';
            p += 1;
            c = digit(p);
        }
        round = !zero_tail;
    }
    if round {
        low64 = low64.wrapping_add(1);
        if low64 == 0 {
            high = high.wrapping_add(1);
            if high == 0 {
                low64 = 0x9999_9999_9999_999A;
                high = u32::MAX / 10;
                e += 1;
            }
        }
    }
    if e > 0 {
        return None;
    }
    if e <= -PRECISION {
        return Some(NetDecimal {
            mantissa: 0,
            scale: (PRECISION - 1) as u8,
            negative: nb.negative,
        });
    }
    Some(NetDecimal {
        mantissa: (u128::from(high) << 64) | u128::from(low64),
        scale: (-e) as u8,
        negative: nb.negative,
    })
}

/// `int.TryParse` / `long.TryParse` with `NumberStyles.Integer` and invariant symbols.
pub fn parse_integer(s: &str, min: i128, max: i128) -> Option<i128> {
    let t = s.trim_end_matches('\0');
    let t = t.trim_start_matches(is_white).trim_end_matches(is_white);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: i128 = 0;
    for b in digits.bytes() {
        v = v.checked_mul(10)?.checked_add(i128::from(b - b'0'))?;
        if v > max.max(-min) {
            return None;
        }
    }
    let v = if neg { -v } else { v };
    (min..=max).contains(&v).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(s: &str) -> Option<String> {
        parse_number_style(s).map(|d| d.to_string())
    }

    #[test]
    fn number_style_matches_dotnet() {
        assert_eq!(style("1,234.5").as_deref(), Some("1234.5"));
        assert_eq!(style("1.5-").as_deref(), Some("-1.5"));
        assert_eq!(style("-0.0").as_deref(), Some("0.0"));
        assert_eq!(style("1e3"), None);
        assert_eq!(style(",12"), None);
        assert_eq!(
            style("999999999999999999999999999.95").as_deref(),
            Some("1000000000000000000000000000.0")
        );
    }

    #[test]
    fn json_numbers_normalise_through_g29() {
        let g = |s: &str| parse_json(s).map(|d| d.g29());
        assert_eq!(g("1.50").as_deref(), Some("1.5"));
        assert_eq!(g("1.2e1").as_deref(), Some("12"));
        assert_eq!(g("0.00001").as_deref(), Some("1E-05"));
        assert_eq!(g("1e40"), None);
    }

    #[test]
    fn integers_follow_the_integer_style() {
        assert_eq!(
            parse_integer(" -7 ", i64::MIN.into(), i64::MAX.into()),
            Some(-7)
        );
        assert_eq!(parse_integer("42\0", 0, 100), Some(42));
        assert_eq!(
            parse_integer("2147483648", i32::MIN.into(), i32::MAX.into()),
            None
        );
        assert_eq!(parse_integer("+", 0, 100), None);
    }
}
