#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod arith;
mod fmt;
mod money;
mod parse;
mod round;
mod u256;

#[cfg(test)]
mod tests;

pub use round::RoundingMode;

use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use u256::{POW10_U128, U256, digits_u128};

/// Largest supported scale (number of decimal places).
pub const MAX_SCALE: u32 = 28;

/// Largest number of digits in a mantissa.
pub const MAX_DIGITS: u32 = 38;

/// 10^38 - 1, the largest mantissa magnitude.
const MAX_MANTISSA: u128 = 99_999_999_999_999_999_999_999_999_999_999_999_999;

/// Error returned by parsing, conversions and the non-`checked_*` fallible methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The string is not a decimal number.
    InvalidFormat {
        /// Byte offset of the first unexpected character (the input length if more
        /// characters were expected).
        offset: usize,
    },
    /// The integer part needs more than 38 digits, or the value cannot be represented
    /// exactly with 38 significant digits.
    Overflow,
    /// Division by zero, or allocation over ratios that sum to zero.
    DivisionByZero,
    /// More than 28 decimal places are written or requested, or an amount has more
    /// decimal places than [`Decimal::split`] / [`Decimal::allocate`] were asked for.
    ScaleTooLarge,
    /// Conversion from a NaN or infinite float.
    NotFinite,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Error::InvalidFormat { offset } => write!(f, "invalid decimal at offset {offset}"),
            Error::Overflow => f.write_str("decimal overflow: more than 38 digits needed"),
            Error::DivisionByZero => f.write_str("division by zero"),
            Error::ScaleTooLarge => f.write_str("more than 28 decimal places"),
            Error::NotFinite => f.write_str("float is NaN or infinite"),
        }
    }
}

impl core::error::Error for Error {}

/// A decimal number `mantissa × 10^-scale` with up to 38 digits and 28 decimal places.
///
/// Create one with [`FromStr`](core::str::FromStr) (`"12.50".parse()`), `From<integer>`,
/// [`Decimal::new`] or, in constants, [`Decimal::from_i128_scale`].
///
/// Equality, ordering and hashing compare numeric values, so `1.0 == 1.00` and both hash
/// the same. `Display` and `Debug` print the exact stored digits.
#[derive(Clone, Copy, Default)]
pub struct Decimal {
    /// Invariant: `|mantissa| <= MAX_MANTISSA`.
    mantissa: i128,
    /// Invariant: `scale <= MAX_SCALE`.
    scale: u8,
}

impl Decimal {
    /// 0.
    pub const ZERO: Decimal = Decimal {
        mantissa: 0,
        scale: 0,
    };
    /// 1.
    pub const ONE: Decimal = Decimal {
        mantissa: 1,
        scale: 0,
    };
    /// 10.
    pub const TEN: Decimal = Decimal {
        mantissa: 10,
        scale: 0,
    };
    /// 100.
    pub const ONE_HUNDRED: Decimal = Decimal {
        mantissa: 100,
        scale: 0,
    };
    /// 10^38 - 1, the largest value.
    pub const MAX: Decimal = Decimal {
        mantissa: MAX_MANTISSA as i128,
        scale: 0,
    };
    /// -(10^38 - 1), the smallest value.
    pub const MIN: Decimal = Decimal {
        mantissa: -(MAX_MANTISSA as i128),
        scale: 0,
    };
    /// Largest supported scale, 28.
    pub const MAX_SCALE: u32 = MAX_SCALE;

    /// `mantissa × 10^-scale`, e.g. `Decimal::new(1250, 2)` is 12.50.
    ///
    /// # Errors
    /// [`Error::ScaleTooLarge`] if `scale > 28`; [`Error::Overflow`] if
    /// `|mantissa| > 10^38 - 1`.
    pub const fn new(mantissa: i128, scale: u32) -> Result<Decimal, Error> {
        if scale > MAX_SCALE {
            Err(Error::ScaleTooLarge)
        } else if mantissa.unsigned_abs() > MAX_MANTISSA {
            Err(Error::Overflow)
        } else {
            Ok(Decimal {
                mantissa,
                scale: scale as u8,
            })
        }
    }

    /// Builds a decimal from a magnitude, a sign and a scale: `±magnitude × 10^-scale`.
    ///
    /// # Errors
    /// Same as [`new`](Self::new).
    pub const fn from_parts(magnitude: u128, negative: bool, scale: u32) -> Result<Decimal, Error> {
        if scale > MAX_SCALE {
            Err(Error::ScaleTooLarge)
        } else if magnitude > MAX_MANTISSA {
            Err(Error::Overflow)
        } else {
            Ok(Decimal::from_mag(negative, magnitude, scale))
        }
    }

    /// [`new`](Self::new) for constants: `const RATE: Decimal = Decimal::from_i128_scale(8, 2);`
    ///
    /// # Panics
    /// Panics if `scale > 28` or `|mantissa| > 10^38 - 1`. In a `const` item this is a
    /// compile-time error.
    pub const fn from_i128_scale(mantissa: i128, scale: u32) -> Decimal {
        match Decimal::new(mantissa, scale) {
            Ok(d) => d,
            Err(_) => panic!("Decimal::from_i128_scale: mantissa or scale out of range"),
        }
    }

    /// Internal constructor; callers guarantee `mag <= MAX_MANTISSA` and `scale <= 28`.
    #[inline]
    pub(crate) const fn from_mag(neg: bool, mag: u128, scale: u32) -> Decimal {
        debug_assert!(mag <= MAX_MANTISSA && scale <= MAX_SCALE);
        let m = mag as i128;
        Decimal {
            mantissa: if neg { -m } else { m },
            scale: scale as u8,
        }
    }

    /// The mantissa: `self == mantissa × 10^-scale`.
    #[inline]
    pub const fn mantissa(self) -> i128 {
        self.mantissa
    }

    /// The scale (number of stored decimal places, 0–28).
    #[inline]
    pub const fn scale(self) -> u32 {
        self.scale as u32
    }

    /// `true` if the value is zero (at any scale).
    #[inline]
    pub const fn is_zero(self) -> bool {
        self.mantissa == 0
    }

    /// `true` if the value is less than zero.
    #[inline]
    pub const fn is_negative(self) -> bool {
        self.mantissa < 0
    }

    /// `true` if the value is greater than zero.
    #[inline]
    pub const fn is_positive(self) -> bool {
        self.mantissa > 0
    }

    /// -1, 0 or 1 (scale 0) according to the sign.
    pub const fn signum(self) -> Decimal {
        Decimal {
            mantissa: self.mantissa.signum(),
            scale: 0,
        }
    }

    /// Absolute value. Never overflows: the range is symmetric.
    pub const fn abs(self) -> Decimal {
        Decimal {
            mantissa: self.mantissa.abs(),
            scale: self.scale,
        }
    }

    /// Number of digits in the mantissa, trailing zeros included (`1.50` has 3; zero
    /// has 1). This is the number of significant digits Python `decimal` reports.
    pub const fn digits(self) -> u32 {
        digits_u128(self.mantissa.unsigned_abs())
    }

    /// The same value with trailing zeros removed (`1.500` → `1.5`, `100` stays `100`,
    /// `0.00` → `0`).
    pub fn normalize(self) -> Decimal {
        let mut m = self.mantissa;
        let mut s = self.scale;
        if m == 0 {
            return Decimal::ZERO;
        }
        while s > 0 && m % 10 == 0 {
            m /= 10;
            s -= 1;
        }
        Decimal {
            mantissa: m,
            scale: s,
        }
    }

    /// The integer part as `i64` (truncated toward zero), or `None` if it does not fit.
    pub fn to_i64(self) -> Option<i64> {
        i64::try_from(self.mantissa / POW10_U128[self.scale as usize] as i128).ok()
    }

    /// The integer part as `i128` (truncated toward zero).
    ///
    /// Always `Some`, because `|self| < 10^38 < 2^127`; the `Option` keeps the signature
    /// the same as [`to_i64`](Self::to_i64).
    pub fn to_i128(self) -> Option<i128> {
        Some(self.mantissa / POW10_U128[self.scale as usize] as i128)
    }

    /// The nearest `f64` (correctly rounded, ties to even).
    pub fn to_f64(self) -> f64 {
        let mut buf = fmt::StackBuf::<48>::new();
        // The buffer holds any Decimal: at most 1 sign + 38 digits + 1 point + 1 zero.
        let _ = core::fmt::write(&mut buf, format_args!("{self}"));
        // Display always produces a valid float literal; NaN would mean a bug above.
        buf.as_str().parse::<f64>().unwrap_or(f64::NAN)
    }

    /// Magnitudes of `self` and `other` aligned to the larger scale.
    fn aligned(self, other: Decimal) -> (U256, U256, u32) {
        let (sa, sb) = (self.scale(), other.scale());
        let s = sa.max(sb);
        let a = U256::mul_u128(self.mantissa.unsigned_abs(), POW10_U128[(s - sa) as usize]);
        let b = U256::mul_u128(other.mantissa.unsigned_abs(), POW10_U128[(s - sb) as usize]);
        (a, b, s)
    }
}

macro_rules! from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Decimal {
            /// Exact conversion (every value fits in 38 digits).
            #[inline]
            fn from(v: $t) -> Self {
                Decimal { mantissa: v as i128, scale: 0 }
            }
        }
    )*};
}

from_int!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl TryFrom<i128> for Decimal {
    type Error = Error;
    /// Fails with [`Error::Overflow`] if `|v| > 10^38 - 1`.
    fn try_from(v: i128) -> Result<Self, Error> {
        Decimal::new(v, 0)
    }
}

impl TryFrom<u128> for Decimal {
    type Error = Error;
    /// Fails with [`Error::Overflow`] if `v > 10^38 - 1`.
    fn try_from(v: u128) -> Result<Self, Error> {
        Decimal::from_parts(v, false, 0)
    }
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.scale == other.scale {
            return self.mantissa.cmp(&other.mantissa);
        }
        let (sa, sb) = (self.mantissa.signum(), other.mantissa.signum());
        if sa != sb || sa == 0 {
            return sa.cmp(&sb);
        }
        // Fast path: scale the operand with fewer decimals up, if that fits in i128.
        // The scale difference is at most 28 and 10^28 fits in i128.
        if self.scale < other.scale {
            let p = POW10_U128[(other.scale - self.scale) as usize] as i128;
            if let Some(a) = self.mantissa.checked_mul(p) {
                return a.cmp(&other.mantissa);
            }
        } else {
            let p = POW10_U128[(self.scale - other.scale) as usize] as i128;
            if let Some(b) = other.mantissa.checked_mul(p) {
                return self.mantissa.cmp(&b);
            }
        }
        let (a, b, _) = self.aligned(*other);
        let mag = a.cmp(&b);
        if sa < 0 { mag.reverse() } else { mag }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Decimal {
    /// Hashes the normalized value, so equal values hash equally at any scale.
    fn hash<H: Hasher>(&self, state: &mut H) {
        let n = self.normalize();
        n.mantissa.hash(state);
        n.scale.hash(state);
    }
}
