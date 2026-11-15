//! Parsing from strings and conversion from floats.

use crate::fmt::StackBuf;
use crate::u256::POW10_U128;
use crate::{Decimal, Error, MAX_MANTISSA, MAX_SCALE, RoundingMode};
use core::cmp::Ordering;
use core::str::FromStr;

/// Exponents are clamped to this magnitude while parsing; anything larger already
/// overflows or rounds to zero.
const EXP_LIMIT: i64 = 1_000_000_000_000_000;

impl FromStr for Decimal {
    type Err = Error;

    /// Parses `[+|-] digits [. digits] [(e|E) [+|-] digits]`.
    ///
    /// - At least one digit is required before or after the point, so `"5."` and `".5"`
    ///   are accepted (like C# `decimal.Parse`), `"."` is not.
    /// - No whitespace, thousands separators, `NaN` or `Infinity`.
    /// - The scale written is kept exactly (`"1.50"` has scale 2), so parsing never
    ///   rounds and `to_string` gives the input back. An exponent shifts the scale:
    ///   `"1.5e3"` is `1500` (scale 0), `"2E-2"` is `0.02`, `"1.50e1"` is `15.0`.
    /// - The scale (fraction digits minus the exponent) must be at most 28, and the
    ///   digits from the first non-zero one to the last one written must be at most 38.
    ///   Leading zeros are free (`"000.5"`); trailing zeros count (`"1.000"` has 4
    ///   digits). Use [`Decimal::parse_rounded`] to round instead of failing.
    ///
    /// # Errors
    /// [`Error::InvalidFormat`] with the byte offset of the first unexpected byte (or
    /// the input length if the input ends too early); [`Error::Overflow`] if the integer
    /// part has more than 38 digits or more than 38 digits are written;
    /// [`Error::ScaleTooLarge`] if the scale is more than 28 (e.g. 29 fraction digits,
    /// even if they are zeros).
    ///
    /// ```
    /// use dl_decimal::{Decimal, Error};
    /// assert_eq!("-12.50".parse::<Decimal>().unwrap().scale(), 2);
    /// assert_eq!(".5".parse::<Decimal>().unwrap().to_string(), "0.5");
    /// assert_eq!("5.".parse::<Decimal>().unwrap().to_string(), "5");
    /// assert_eq!("1.5e3".parse::<Decimal>().unwrap().to_string(), "1500");
    /// assert_eq!("1,5".parse::<Decimal>(), Err(Error::InvalidFormat { offset: 1 }));
    /// assert_eq!("1e-29".parse::<Decimal>(), Err(Error::ScaleTooLarge));
    /// ```
    fn from_str(s: &str) -> Result<Decimal, Error> {
        parse(s.as_bytes(), None)
    }
}

impl Decimal {
    /// Like [`FromStr`], but rounds with `mode` instead of failing when the value has
    /// more than 28 decimal places or more than 38 significant digits. The result keeps
    /// the largest scale (at most the written one and at most 28) that fits in 38 digits.
    ///
    /// ```
    /// use dl_decimal::{Decimal, RoundingMode};
    /// let x = Decimal::parse_rounded("0.12345678901234567890123456789", RoundingMode::HalfUp);
    /// assert_eq!(x.unwrap().to_string(), "0.1234567890123456789012345679");
    /// ```
    ///
    /// # Errors
    /// [`Error::InvalidFormat`] as for `FromStr`; [`Error::Overflow`] if the integer part
    /// has more than 38 digits.
    pub fn parse_rounded(s: &str, mode: RoundingMode) -> Result<Decimal, Error> {
        parse(s.as_bytes(), Some(mode))
    }
}

fn digit_run(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// Strict (`mode == None`) or rounding parser.
fn parse(s: &[u8], mode: Option<RoundingMode>) -> Result<Decimal, Error> {
    let mut i = 0;
    let neg = match s.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let int_start = i;
    i = digit_run(s, i);
    let int = &s[int_start..i];
    let mut frac: &[u8] = &[];
    if s.get(i) == Some(&b'.') {
        let start = i + 1;
        i = digit_run(s, start);
        frac = &s[start..i];
    }
    if int.is_empty() && frac.is_empty() {
        return Err(Error::InvalidFormat { offset: i });
    }
    let mut exp: i64 = 0;
    if matches!(s.get(i), Some(b'e' | b'E')) {
        i += 1;
        let exp_neg = match s.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let start = i;
        while let Some(&c) = s.get(i).filter(|c| c.is_ascii_digit()) {
            exp = (exp * 10 + i64::from(c - b'0')).min(EXP_LIMIT);
            i += 1;
        }
        if i == start {
            return Err(Error::InvalidFormat { offset: i });
        }
        if exp_neg {
            exp = -exp;
        }
    }
    if i != s.len() {
        return Err(Error::InvalidFormat { offset: i });
    }
    build(neg, int, frac, exp, mode)
}

/// Accumulates ASCII digits into a `u128`; callers pass at most 38 digits.
fn accumulate<'a>(digits: impl Iterator<Item = &'a u8>) -> u128 {
    digits.fold(0u128, |m, &c| m * 10 + u128::from(c - b'0'))
}

/// Builds `±(int frac) × 10^(exp - frac.len())` from validated digit runs.
fn build(
    neg: bool,
    int: &[u8],
    frac: &[u8],
    exp: i64,
    mode: Option<RoundingMode>,
) -> Result<Decimal, Error> {
    let all = || int.iter().chain(frac.iter());
    let total = int.len() + frac.len();
    let lead = all().take_while(|&&c| c == b'0').count();
    // Significant digits: from the first non-zero digit to the last written digit.
    let sig = (total - lead) as i64;
    // Slices never exceed isize::MAX bytes and |exp| <= EXP_LIMIT, so this cannot overflow.
    let scale = frac.len() as i64 - exp;
    if sig == 0 {
        if mode.is_none() && scale > MAX_SCALE as i64 {
            return Err(Error::ScaleTooLarge);
        }
        return Ok(Decimal::from_mag(
            false,
            0,
            scale.clamp(0, MAX_SCALE as i64) as u32,
        ));
    }
    if sig - scale > 38 {
        // More than 38 integer digits (also covers appended exponent zeros).
        return Err(Error::Overflow);
    }
    if scale <= 0 {
        // Positive effective exponent: append zeros to an integer. sig - scale <= 38.
        let m = accumulate(all().skip(lead)) * POW10_U128[(-scale) as usize];
        return Ok(Decimal::from_mag(neg, m, 0));
    }
    let Some(mode) = mode else {
        // Strict: the written scale and digits are kept exactly, never changed.
        if scale > MAX_SCALE as i64 {
            return Err(Error::ScaleTooLarge);
        }
        if sig > 38 {
            return Err(Error::Overflow);
        }
        return Ok(Decimal::from_mag(
            neg,
            accumulate(all().skip(lead)),
            scale as u32,
        ));
    };
    // Digits to drop from the right so that scale <= 28 and digits <= 38. The integer
    // check above guarantees drop <= scale.
    let drop = (scale - MAX_SCALE as i64).max(sig - 38).max(0);
    let new_scale = (scale - drop) as u32;
    let trailing_zeros = all().rev().take_while(|&&c| c == b'0').count() as i64;
    if drop <= trailing_zeros {
        // Only zeros are dropped: exact.
        let keep = (sig - drop) as usize;
        let m = accumulate(all().skip(lead).take(keep));
        return Ok(Decimal::from_mag(neg, m, new_scale));
    }
    // Round: kept digits, the first dropped digit and whether anything after it is non-zero.
    let (kept, first, sticky) = if drop > sig {
        // Everything is dropped and the first dropped position is a leading zero.
        (0, 0, true)
    } else {
        let keep = (sig - drop) as usize;
        let mut rest = all().skip(lead + keep);
        let first = rest.next().map_or(0, |&c| c - b'0');
        let sticky = rest.any(|&c| c != b'0');
        (accumulate(all().skip(lead).take(keep)), first, sticky)
    };
    let half = match first.cmp(&5) {
        Ordering::Equal if sticky => Ordering::Greater,
        o => o,
    };
    let up = mode.round_up(neg, kept & 1 == 1, half, first != 0 || sticky);
    let mut m = kept + u128::from(up);
    let mut new_scale = new_scale;
    if m > MAX_MANTISSA {
        if new_scale == 0 {
            return Err(Error::Overflow);
        }
        m /= 10;
        new_scale -= 1;
    }
    Ok(Decimal::from_mag(neg, m, new_scale))
}

macro_rules! try_from_float {
    ($t:ty) => {
        impl TryFrom<$t> for Decimal {
            type Error = Error;

            /// The exact decimal of the shortest string that round-trips to the float,
            /// i.e. what `format!("{}", v)` prints: `0.1` gives `0.1`, not
            /// `0.1000000000000000055511151231257827`.
            ///
            /// # Errors
            /// [`Error::NotFinite`] for NaN and infinities; [`Error::Overflow`] if
            /// `|v| >= 10^38`; [`Error::ScaleTooLarge`] if more than 28 decimal places are
            /// needed (e.g. `1e-30`).
            fn try_from(v: $t) -> Result<Decimal, Error> {
                if !v.is_finite() {
                    return Err(Error::NotFinite);
                }
                // `{:e}` is the shortest round-trip form in scientific notation, at most
                // 25 bytes for f64 (e.g. "-2.2250738585072014e-308").
                let mut buf = StackBuf::<32>::new();
                core::fmt::write(&mut buf, format_args!("{v:e}")).map_err(|_| Error::Overflow)?;
                parse(buf.as_str().as_bytes(), None)
            }
        }
    };
}

try_from_float!(f64);
try_from_float!(f32);
