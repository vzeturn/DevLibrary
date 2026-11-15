//! JSON numbers.

use alloc::boxed::Box;
use alloc::string::ToString;
use core::fmt;

/// A JSON number.
///
/// Integers that fit are stored exactly as `u64` (non-negative) or `i64` (negative);
/// everything else is a finite `f64`. With
/// [`ParseOptions::keep_raw_numbers`](crate::ParseOptions::keep_raw_numbers) the parser
/// instead keeps the exact source text (a *raw* number), which is serialized verbatim.
/// Use that for money and other values that must not go through binary floating point.
///
/// Equality is numeric: `1`, `1.0` and raw `1.00` are all equal. Two raw numbers are
/// compared as exact decimals; a raw number and a non-raw one are compared after
/// converting the raw text the way the default parser would.
///
/// ```
/// use dl_json::Number;
///
/// let n = Number::from(42u8);
/// assert_eq!(n.as_u64(), Some(42));
/// assert_eq!(n.as_f64(), Some(42.0));
/// assert!(Number::from_f64(f64::NAN).is_none());
/// assert_eq!(Number::from_raw("10.50").unwrap().to_string(), "10.50");
/// ```
#[derive(Clone)]
pub struct Number {
    pub(crate) n: N,
}

/// Internal representation. Invariants: `NegInt` is `< 0`, `Float` is finite and `Raw`
/// is a valid JSON number lexeme.
#[derive(Clone)]
pub(crate) enum N {
    PosInt(u64),
    NegInt(i64),
    Float(f64),
    Raw(Box<str>),
}

/// Result of scanning a number lexeme.
pub(crate) struct Scanned {
    /// Index one past the last byte of the lexeme.
    pub end: usize,
    /// The lexeme has no fraction and no exponent.
    pub is_int: bool,
}

/// Scans a JSON number starting at `start` (which must be `-` or a digit for a
/// meaningful result). On failure returns the offset of the offending byte.
pub(crate) fn scan(b: &[u8], start: usize) -> Result<Scanned, usize> {
    let mut i = start;
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    match b.get(i) {
        Some(b'0') => {
            i += 1;
            // Leading zeros are not allowed: `01`, `-00`.
            if matches!(b.get(i), Some(b'0'..=b'9')) {
                return Err(i);
            }
        }
        Some(b'1'..=b'9') => {
            i += 1;
            while matches!(b.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        _ => return Err(i),
    }
    let mut is_int = true;
    if b.get(i) == Some(&b'.') {
        is_int = false;
        i += 1;
        if !matches!(b.get(i), Some(b'0'..=b'9')) {
            return Err(i);
        }
        while matches!(b.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        is_int = false;
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !matches!(b.get(i), Some(b'0'..=b'9')) {
            return Err(i);
        }
        while matches!(b.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
    }
    Ok(Scanned { end: i, is_int })
}

/// Converts a validated lexeme to a non-raw number. `None` if a float overflows.
pub(crate) fn from_lexeme(s: &str, is_int: bool) -> Option<N> {
    if is_int {
        if let Some(digits) = s.strip_prefix('-') {
            if let Ok(m) = digits.parse::<u64>() {
                if m == 0 {
                    // Keep the sign of `-0`; an integer cannot.
                    return Some(N::Float(-0.0));
                }
                if m <= 1 << 63 {
                    return Some(N::NegInt(0i64.wrapping_sub_unsigned(m)));
                }
            }
        } else if let Ok(m) = s.parse::<u64>() {
            return Some(N::PosInt(m));
        }
    }
    // Integers that overflow 64 bits and all floats.
    match s.parse::<f64>() {
        Ok(f) if f.is_finite() => Some(N::Float(f)),
        _ => None,
    }
}

impl Number {
    /// Wraps a finite float; `None` for NaN and infinities, which JSON cannot represent.
    pub fn from_f64(f: f64) -> Option<Number> {
        f.is_finite().then_some(Number { n: N::Float(f) })
    }

    /// Creates a raw number from JSON number text, kept and serialized verbatim.
    /// Returns `None` if `s` is not a valid JSON number (RFC 8259 section 6).
    ///
    /// ```
    /// use dl_json::Number;
    ///
    /// assert!(Number::from_raw("123456789012345678901234567890.10").is_some());
    /// assert!(Number::from_raw("1.").is_none());
    /// assert!(Number::from_raw("+1").is_none());
    /// ```
    pub fn from_raw(s: &str) -> Option<Number> {
        match scan(s.as_bytes(), 0) {
            Ok(sc) if sc.end == s.len() => Some(Number {
                n: N::Raw(s.into()),
            }),
            _ => None,
        }
    }

    /// The value as `i64` if it is an integer in range (raw numbers included).
    /// Floats return `None`, even when integral.
    pub fn as_i64(&self) -> Option<i64> {
        match &self.n {
            N::PosInt(u) => i64::try_from(*u).ok(),
            N::NegInt(i) => Some(*i),
            N::Float(_) => None,
            N::Raw(s) => s.parse().ok(),
        }
    }

    /// The value as `u64` if it is a non-negative integer in range (raw numbers included).
    /// Floats return `None`, even when integral.
    pub fn as_u64(&self) -> Option<u64> {
        match &self.n {
            N::PosInt(u) => Some(*u),
            N::NegInt(_) | N::Float(_) => None,
            N::Raw(s) => s.parse().ok(),
        }
    }

    /// The value as `f64`. Integers above 2^53 lose precision. Raw numbers are parsed
    /// and return `None` only if they overflow `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        match &self.n {
            N::PosInt(u) => Some(*u as f64),
            N::NegInt(i) => Some(*i as f64),
            N::Float(f) => Some(*f),
            N::Raw(s) => s.parse::<f64>().ok().filter(|f| f.is_finite()),
        }
    }

    /// `true` if [`as_i64`](Self::as_i64) returns `Some`.
    pub fn is_i64(&self) -> bool {
        self.as_i64().is_some()
    }

    /// `true` if [`as_u64`](Self::as_u64) returns `Some`.
    pub fn is_u64(&self) -> bool {
        self.as_u64().is_some()
    }

    /// `true` if the number is stored as a binary float (it had a fraction or exponent,
    /// overflowed 64-bit integers, or came from `f64`). Raw numbers return `false`.
    pub fn is_f64(&self) -> bool {
        matches!(self.n, N::Float(_))
    }

    /// `true` for raw numbers (see [`as_raw_str`](Self::as_raw_str)).
    pub fn is_raw(&self) -> bool {
        matches!(self.n, N::Raw(_))
    }

    /// The exact source text of a raw number; `None` for other numbers.
    pub fn as_raw_str(&self) -> Option<&str> {
        match &self.n {
            N::Raw(s) => Some(s),
            _ => None,
        }
    }

    /// Converts a raw number to the representation the default parser would produce.
    fn cooked(&self) -> Option<N> {
        match &self.n {
            N::Raw(s) => {
                let is_int = !s.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'));
                from_lexeme(s, is_int)
            }
            n => Some(n.clone()),
        }
    }
}

fn eq_cooked(a: &N, b: &N) -> bool {
    match (a, b) {
        (N::PosInt(x), N::PosInt(y)) => x == y,
        (N::NegInt(x), N::NegInt(y)) => x == y,
        (N::Float(x), N::Float(y)) => x == y,
        // In range, `as` truncates toward zero; the float is integral exactly when the
        // truncated value converts back to the same float, and then the cast is exact.
        (N::PosInt(u), N::Float(f)) | (N::Float(f), N::PosInt(u)) => {
            *f >= 0.0
                && *f < 18_446_744_073_709_551_616.0
                && (*f as u64) as f64 == *f
                && *f as u64 == *u
        }
        (N::NegInt(i), N::Float(f)) | (N::Float(f), N::NegInt(i)) => {
            *f < 0.0
                && *f >= -9_223_372_036_854_775_808.0
                && (*f as i64) as f64 == *f
                && *f as i64 == *i
        }
        _ => false,
    }
}

/// Splits a validated lexeme into (negative, significant digits, point position) so that
/// the value is `0.DIGITS * 10^point`. Zero has empty digits. `None` if the exponent does
/// not fit in `i64`.
fn decimal_parts(s: &str) -> Option<(bool, alloc::vec::Vec<u8>, i64)> {
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (mantissa, exp) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits: alloc::vec::Vec<u8> = int.bytes().chain(frac.bytes()).collect();
    let mut point = (int.len() as i64).checked_add(exp)?;
    let lead = digits.iter().take_while(|&&d| d == b'0').count();
    digits.drain(..lead);
    point = point.checked_sub(lead as i64)?;
    while digits.last() == Some(&b'0') {
        digits.pop();
    }
    if digits.is_empty() {
        return Some((false, digits, 0));
    }
    Some((neg, digits, point))
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        match (&self.n, &other.n) {
            (N::Raw(a), N::Raw(b)) => match (decimal_parts(a), decimal_parts(b)) {
                (Some(x), Some(y)) => x == y,
                _ => a == b,
            },
            _ => match (self.cooked(), other.cooked()) {
                (Some(a), Some(b)) => eq_cooked(&a, &b),
                _ => false,
            },
        }
    }
}

impl fmt::Display for Number {
    /// Writes the number exactly as the serializer does.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::ser::write_number(f, self)
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Number({self})")
    }
}

macro_rules! from_unsigned {
    ($($t:ty)*) => {$(
        impl From<$t> for Number {
            fn from(v: $t) -> Self {
                Number { n: N::PosInt(v as u64) }
            }
        }
    )*};
}
from_unsigned!(u8 u16 u32 u64 usize);

macro_rules! from_signed {
    ($($t:ty)*) => {$(
        impl From<$t> for Number {
            fn from(v: $t) -> Self {
                let v = v as i64;
                if v < 0 {
                    Number { n: N::NegInt(v) }
                } else {
                    Number { n: N::PosInt(v as u64) }
                }
            }
        }
    )*};
}
from_signed!(i8 i16 i32 i64 isize);

impl From<u128> for Number {
    /// Values above `u64::MAX` become raw numbers so no digit is lost.
    fn from(v: u128) -> Self {
        match u64::try_from(v) {
            Ok(u) => Number::from(u),
            Err(_) => Number {
                n: N::Raw(v.to_string().into()),
            },
        }
    }
}

impl From<i128> for Number {
    /// Values outside the 64-bit ranges become raw numbers so no digit is lost.
    fn from(v: i128) -> Self {
        if let Ok(i) = i64::try_from(v) {
            Number::from(i)
        } else if let Ok(u) = u64::try_from(v) {
            Number::from(u)
        } else {
            Number {
                n: N::Raw(v.to_string().into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn raw(s: &str) -> Number {
        Number::from_raw(s).unwrap()
    }

    #[test]
    fn accessors() {
        let n = Number::from(u64::MAX);
        assert_eq!((n.as_u64(), n.as_i64()), (Some(u64::MAX), None));
        let n = Number::from(i64::MIN);
        assert_eq!((n.as_u64(), n.as_i64()), (None, Some(i64::MIN)));
        let f = Number::from_f64(2.5).unwrap();
        assert!(f.is_f64() && !f.is_i64() && !f.is_u64() && !f.is_raw());
        assert_eq!(f.as_f64(), Some(2.5));
        assert!(Number::from_f64(f64::INFINITY).is_none());
        let r = raw("-12");
        assert_eq!(
            (r.as_i64(), r.as_u64(), r.as_raw_str()),
            (Some(-12), None, Some("-12"))
        );
        assert_eq!(raw("1e400").as_f64(), None);
        assert_eq!(raw("0.25").as_f64(), Some(0.25));
        assert!(raw("1.5").as_i64().is_none());
    }

    #[test]
    fn from_raw_validates() {
        for ok in [
            "0",
            "-0",
            "1.5",
            "1e5",
            "1E+5",
            "-1.25e-3",
            "12345678901234567890123",
        ] {
            assert!(Number::from_raw(ok).is_some(), "{ok}");
        }
        for bad in [
            "", "-", "01", "1.", ".5", "+1", "1e", "1e+", "1 ", "NaN", "0x10",
        ] {
            assert!(Number::from_raw(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn numeric_equality() {
        assert_eq!(Number::from(1u8), Number::from_f64(1.0).unwrap());
        assert_eq!(Number::from(-3i8), Number::from_f64(-3.0).unwrap());
        assert_eq!(Number::from(0u8), Number::from_f64(-0.0).unwrap());
        assert_ne!(Number::from(1u8), Number::from_f64(1.5).unwrap());
        // 2^64 as f64 is not u64::MAX.
        assert_ne!(
            Number::from(u64::MAX),
            Number::from_f64(18446744073709551616.0).unwrap()
        );
        assert_ne!(Number::from(i64::MIN), Number::from(0u8));
        assert_eq!(raw("10.50"), raw("10.5"));
        assert_eq!(raw("1050e-2"), raw("10.5"));
        assert_eq!(raw("-0.0"), raw("0"));
        assert_ne!(raw("10.51"), raw("10.5"));
        assert_ne!(raw("-1"), raw("1"));
        assert_eq!(raw("1.0"), Number::from(1u8));
        assert_eq!(raw("0.1"), Number::from_f64(0.1).unwrap());
        assert_ne!(raw("1e400"), Number::from_f64(f64::MAX).unwrap());
        assert_eq!(raw("1e99999999999999999999"), raw("1e99999999999999999999"));
    }

    #[test]
    fn wide_integers() {
        assert_eq!(
            Number::from(u128::MAX).to_string(),
            "340282366920938463463374607431768211455"
        );
        assert_eq!(
            Number::from(i128::MIN).to_string(),
            "-170141183460469231731687303715884105728"
        );
        assert_eq!(Number::from(5u128).as_u64(), Some(5));
        assert_eq!(Number::from(-5i128).as_i64(), Some(-5));
        assert_eq!(Number::from(u64::MAX as i128).as_u64(), Some(u64::MAX));
    }

    #[test]
    fn lexeme_conversion() {
        assert!(matches!(
            from_lexeme("-9223372036854775808", true),
            Some(N::NegInt(i64::MIN))
        ));
        assert!(matches!(
            from_lexeme("-9223372036854775809", true),
            Some(N::Float(_))
        ));
        assert!(matches!(
            from_lexeme("18446744073709551615", true),
            Some(N::PosInt(u64::MAX))
        ));
        assert!(matches!(from_lexeme("-0", true), Some(N::Float(f)) if f.is_sign_negative()));
        assert!(from_lexeme("1e400", false).is_none());
    }
}
