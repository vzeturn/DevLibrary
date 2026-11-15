//! Checked arithmetic and the operator traits.

use crate::round::{div_round_u256, fit};
use crate::u256::{POW10, POW10_U128, U256, digits_u128};
use crate::{Decimal, Error, MAX_MANTISSA, MAX_SCALE, RoundingMode};
use core::iter::{Product, Sum};
use core::ops::{
    Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Rem, RemAssign, Sub, SubAssign,
};

/// `round(a × 10^k / b)` for `k >= 0`, or `round(a / (b × 10^-k))` for `k < 0`.
/// `None` if the scaled numerator does not fit in 256 bits.
fn scaled_quotient(a: u128, b: u128, k: i32, mode: RoundingMode, neg: bool) -> Option<U256> {
    if k >= 0 {
        let n = U256::from_u128(a).checked_mul(*POW10.get(k as usize)?)?;
        div_round_u256(n, U256::from_u128(b), mode, neg)
    } else {
        let d = U256::mul_u128(b, *POW10_U128.get(k.unsigned_abs() as usize)?);
        div_round_u256(U256::from_u128(a), d, mode, neg)
    }
}

/// `floor(log10(a / b))` for `a, b > 0`.
fn floor_log10_ratio(a: u128, b: u128) -> i32 {
    let l = digits_u128(a) as i32 - digits_u128(b) as i32;
    // Both products are below 10^digits(a) or 10^digits(b) <= 10^38, so they fit.
    let ge = if l >= 0 {
        a >= b * POW10_U128[l as usize]
    } else {
        a * POW10_U128[l.unsigned_abs() as usize] >= b
    };
    if ge { l } else { l - 1 }
}

impl Decimal {
    /// `self + rhs`, or `None` on overflow. See the [crate docs](crate) for rounding.
    #[inline]
    pub fn checked_add(self, rhs: Decimal) -> Option<Decimal> {
        let (sa, sb) = (self.scale, rhs.scale);
        if sa == sb {
            if let Some(m) = self.mantissa.checked_add(rhs.mantissa)
                && m.unsigned_abs() <= MAX_MANTISSA
            {
                return Some(Decimal {
                    mantissa: m,
                    scale: sa,
                });
            }
        } else {
            let (hi, lo, s) = if sa > sb {
                (self, rhs, sa)
            } else {
                (rhs, self, sb)
            };
            // 10^28 fits in i128.
            let p = POW10_U128[(hi.scale - lo.scale) as usize] as i128;
            if let Some(x) = lo.mantissa.checked_mul(p)
                && let Some(m) = x.checked_add(hi.mantissa)
                && m.unsigned_abs() <= MAX_MANTISSA
            {
                return Some(Decimal {
                    mantissa: m,
                    scale: s,
                });
            }
        }
        add_slow(self, rhs)
    }

    /// `self - rhs`, or `None` on overflow.
    #[inline]
    pub fn checked_sub(self, rhs: Decimal) -> Option<Decimal> {
        self.checked_add(-rhs)
    }

    /// `self × rhs`, or `None` on overflow. The exact product has scale
    /// `self.scale() + rhs.scale()`; it is rounded half-even if that exceeds 28 or the
    /// mantissa exceeds 38 digits.
    #[inline]
    pub fn checked_mul(self, rhs: Decimal) -> Option<Decimal> {
        let s = self.scale() + rhs.scale();
        if s <= MAX_SCALE
            && let Some(m) = self.mantissa.checked_mul(rhs.mantissa)
            && m.unsigned_abs() <= MAX_MANTISSA
        {
            return Some(Decimal {
                mantissa: m,
                scale: s as u8,
            });
        }
        let p = U256::mul_u128(self.mantissa.unsigned_abs(), rhs.mantissa.unsigned_abs());
        fit(p, self.is_negative() != rhs.is_negative(), s)
    }

    /// `self / rhs`, or `None` if `rhs` is zero or the quotient overflows.
    ///
    /// The exact quotient is rounded half-even to 28 decimal places or 38 significant
    /// digits, whichever is fewer, and trailing zeros are stripped:
    /// `1 / 3 = 0.3333333333333333333333333333`, `2.50 / 1 = 2.5`, `10 / 0.5 = 20`.
    pub fn checked_div(self, rhs: Decimal) -> Option<Decimal> {
        if rhs.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some(Decimal::ZERO);
        }
        let neg = self.is_negative() != rhs.is_negative();
        let (a, b) = (self.mantissa.unsigned_abs(), rhs.mantissa.unsigned_abs());
        let (s1, s2) = (self.scale() as i32, rhs.scale() as i32);
        // Decimal exponent of the leading digit of the quotient.
        let adj = floor_log10_ratio(a, b) + s2 - s1;
        if adj >= 38 {
            return None;
        }
        // Keep 38 significant digits, but at most 28 decimal places.
        let mut t = (37 - adj).min(MAX_SCALE as i32) as u32;
        // q × 10^t < 10^38, so the numerator a × 10^k < 10^38 × b < 10^76 fits in U256.
        let mut q =
            scaled_quotient(a, b, s2 + t as i32 - s1, RoundingMode::HalfEven, neg)?.to_u128()?;
        if q > MAX_MANTISSA {
            // Rounded up to exactly 10^38.
            if t == 0 {
                return None;
            }
            q /= 10;
            t -= 1;
        }
        while t > 0 && q % 10 == 0 {
            q /= 10;
            t -= 1;
        }
        Some(Decimal::from_mag(neg, q, t))
    }

    /// `self / rhs` rounded once with `mode` to exactly `dp` decimal places (trailing
    /// zeros kept), e.g. a unit price from a line total:
    ///
    /// ```
    /// use dl_decimal::{Decimal, RoundingMode};
    /// let total: Decimal = "100000".parse().unwrap();
    /// let unit = total.div_round(Decimal::from(3), 2, RoundingMode::HalfUp).unwrap();
    /// assert_eq!(unit.to_string(), "33333.33");
    /// ```
    ///
    /// # Errors
    /// [`Error::ScaleTooLarge`] if `dp > 28`, [`Error::DivisionByZero`] if `rhs` is zero,
    /// [`Error::Overflow`] if the result needs more than 38 digits.
    pub fn div_round(self, rhs: Decimal, dp: u32, mode: RoundingMode) -> Result<Decimal, Error> {
        if dp > MAX_SCALE {
            return Err(Error::ScaleTooLarge);
        }
        if rhs.is_zero() {
            return Err(Error::DivisionByZero);
        }
        let neg = self.is_negative() != rhs.is_negative();
        let (a, b) = (self.mantissa.unsigned_abs(), rhs.mantissa.unsigned_abs());
        let k = rhs.scale() as i32 + dp as i32 - self.scale() as i32;
        // If digits(a) + k > 77 the quotient is >= 10^(digits(a) - 1 + k - 38) >= 10^39.
        if k >= 0 && digits_u128(a) as i32 + k > 77 {
            return Err(Error::Overflow);
        }
        let q = scaled_quotient(a, b, k, mode, neg).ok_or(Error::Overflow)?;
        match q.to_u128() {
            Some(m) if m <= MAX_MANTISSA => Ok(Decimal::from_mag(neg, m, dp)),
            _ => Err(Error::Overflow),
        }
    }

    /// `self % rhs` (sign of `self`, like Rust and C#), or `None` if `rhs` is zero. The
    /// result is exact, with scale `max(self.scale(), rhs.scale())`.
    pub fn checked_rem(self, rhs: Decimal) -> Option<Decimal> {
        if rhs.is_zero() {
            return None;
        }
        if self.scale == rhs.scale {
            // |mantissa| < 2^127, so i128::MIN % -1 cannot occur.
            return Some(Decimal {
                mantissa: self.mantissa % rhs.mantissa,
                scale: self.scale,
            });
        }
        let (a, b, s) = self.aligned(rhs);
        // r <= a and r < b; whichever operand had scale s is <= 10^38 - 1, so r fits.
        let r = a.div_rem(b).1.to_u128()?;
        if r > MAX_MANTISSA {
            return None;
        }
        Some(Decimal::from_mag(self.is_negative(), r, s))
    }

    /// `self^exp` by binary exponentiation, or `None` on overflow. `x^0 == 1`.
    ///
    /// Exact whenever the exact power fits in 38 digits and 28 decimal places.
    /// Otherwise each intermediate multiplication rounds as [`checked_mul`]
    /// does, so the last digit may differ from the correctly rounded power.
    ///
    /// [`checked_mul`]: Self::checked_mul
    pub fn checked_pow(self, exp: u32) -> Option<Decimal> {
        let mut acc = Decimal::ONE;
        let mut base = self;
        let mut e = exp;
        while e > 0 {
            if e & 1 == 1 {
                acc = acc.checked_mul(base)?;
            }
            e >>= 1;
            if e > 0 {
                base = base.checked_mul(base)?;
            }
        }
        Some(acc)
    }

    /// Sum of all items, or `None` on overflow. Adds left to right with
    /// [`checked_add`](Self::checked_add).
    pub fn checked_sum<I: IntoIterator<Item = Decimal>>(items: I) -> Option<Decimal> {
        items
            .into_iter()
            .try_fold(Decimal::ZERO, Decimal::checked_add)
    }
}

fn add_slow(a: Decimal, b: Decimal) -> Option<Decimal> {
    let (ma, mb, s) = a.aligned(b);
    let (na, nb) = (a.is_negative(), b.is_negative());
    // Aligned magnitudes are < 10^66, so neither operation can fail.
    let (neg, mag) = if na == nb {
        (na, ma.checked_add(mb)?)
    } else if ma >= mb {
        (na, ma.checked_sub(mb)?)
    } else {
        (nb, mb.checked_sub(ma)?)
    };
    fit(mag, neg, s)
}

#[inline]
fn add(a: Decimal, b: Decimal) -> Decimal {
    a.checked_add(b).expect("attempt to add with overflow")
}

#[inline]
fn sub(a: Decimal, b: Decimal) -> Decimal {
    a.checked_sub(b).expect("attempt to subtract with overflow")
}

#[inline]
fn mul(a: Decimal, b: Decimal) -> Decimal {
    a.checked_mul(b).expect("attempt to multiply with overflow")
}

#[inline]
fn div(a: Decimal, b: Decimal) -> Decimal {
    assert!(!b.is_zero(), "attempt to divide by zero");
    a.checked_div(b).expect("attempt to divide with overflow")
}

#[inline]
fn rem(a: Decimal, b: Decimal) -> Decimal {
    a.checked_rem(b)
        .expect("attempt to calculate the remainder with a divisor of zero")
}

macro_rules! binop {
    ($Tr:ident, $m:ident, $TrA:ident, $ma:ident, $f:ident, $doc:literal) => {
        impl $Tr for Decimal {
            type Output = Decimal;
            #[doc = $doc]
            #[inline]
            fn $m(self, rhs: Decimal) -> Decimal {
                $f(self, rhs)
            }
        }
        impl $Tr<&Decimal> for Decimal {
            type Output = Decimal;
            #[doc = $doc]
            #[inline]
            fn $m(self, rhs: &Decimal) -> Decimal {
                $f(self, *rhs)
            }
        }
        impl $Tr<Decimal> for &Decimal {
            type Output = Decimal;
            #[doc = $doc]
            #[inline]
            fn $m(self, rhs: Decimal) -> Decimal {
                $f(*self, rhs)
            }
        }
        impl $Tr<&Decimal> for &Decimal {
            type Output = Decimal;
            #[doc = $doc]
            #[inline]
            fn $m(self, rhs: &Decimal) -> Decimal {
                $f(*self, *rhs)
            }
        }
        impl $TrA for Decimal {
            #[doc = $doc]
            #[inline]
            fn $ma(&mut self, rhs: Decimal) {
                *self = $f(*self, rhs);
            }
        }
        impl $TrA<&Decimal> for Decimal {
            #[doc = $doc]
            #[inline]
            fn $ma(&mut self, rhs: &Decimal) {
                *self = $f(*self, *rhs);
            }
        }
        binop_int!($Tr, $m, $TrA, $ma, $f, $doc, i32, i64, u32, u64, usize);
    };
}

macro_rules! binop_int {
    ($Tr:ident, $m:ident, $TrA:ident, $ma:ident, $f:ident, $doc:literal, $($t:ty),*) => {$(
        impl $Tr<$t> for Decimal {
            type Output = Decimal;
            #[doc = $doc]
            #[inline]
            fn $m(self, rhs: $t) -> Decimal {
                $f(self, Decimal::from(rhs))
            }
        }
        impl $TrA<$t> for Decimal {
            #[doc = $doc]
            #[inline]
            fn $ma(&mut self, rhs: $t) {
                *self = $f(*self, Decimal::from(rhs));
            }
        }
    )*};
}

binop!(
    Add,
    add,
    AddAssign,
    add_assign,
    add,
    "Exact sum, rounded half-even only beyond 38 digits.\n\n# Panics\nOn overflow, like integer `+`. See [`Decimal::checked_add`]."
);
binop!(
    Sub,
    sub,
    SubAssign,
    sub_assign,
    sub,
    "Exact difference, rounded half-even only beyond 38 digits.\n\n# Panics\nOn overflow, like integer `-`. See [`Decimal::checked_sub`]."
);
binop!(
    Mul,
    mul,
    MulAssign,
    mul_assign,
    mul,
    "Product, rounded half-even beyond 28 decimal places or 38 digits.\n\n# Panics\nOn overflow, like integer `*`. See [`Decimal::checked_mul`]."
);
binop!(
    Div,
    div,
    DivAssign,
    div_assign,
    div,
    "Quotient to 28 decimal places / 38 digits, half-even, trailing zeros stripped.\n\n# Panics\nOn division by zero or overflow, like integer `/`. See [`Decimal::checked_div`]."
);
binop!(
    Rem,
    rem,
    RemAssign,
    rem_assign,
    rem,
    "Exact remainder with the sign of the dividend.\n\n# Panics\nIf the divisor is zero, like integer `%`. See [`Decimal::checked_rem`]."
);

impl Neg for Decimal {
    type Output = Decimal;
    /// Negation. Never overflows: the range is symmetric.
    #[inline]
    fn neg(self) -> Decimal {
        Decimal {
            mantissa: -self.mantissa,
            scale: self.scale,
        }
    }
}

impl Neg for &Decimal {
    type Output = Decimal;
    /// Negation. Never overflows: the range is symmetric.
    #[inline]
    fn neg(self) -> Decimal {
        -*self
    }
}

impl Sum for Decimal {
    /// Adds left to right.
    ///
    /// # Panics
    /// On overflow, like integer sums. See [`Decimal::checked_sum`].
    fn sum<I: Iterator<Item = Decimal>>(iter: I) -> Decimal {
        iter.fold(Decimal::ZERO, add)
    }
}

impl<'a> Sum<&'a Decimal> for Decimal {
    /// Adds left to right.
    ///
    /// # Panics
    /// On overflow, like integer sums. See [`Decimal::checked_sum`].
    fn sum<I: Iterator<Item = &'a Decimal>>(iter: I) -> Decimal {
        iter.fold(Decimal::ZERO, |a, b| add(a, *b))
    }
}

impl Product for Decimal {
    /// Multiplies left to right.
    ///
    /// # Panics
    /// On overflow, like integer products.
    fn product<I: Iterator<Item = Decimal>>(iter: I) -> Decimal {
        iter.fold(Decimal::ONE, mul)
    }
}

impl<'a> Product<&'a Decimal> for Decimal {
    /// Multiplies left to right.
    ///
    /// # Panics
    /// On overflow, like integer products.
    fn product<I: Iterator<Item = &'a Decimal>>(iter: I) -> Decimal {
        iter.fold(Decimal::ONE, |a, b| mul(a, *b))
    }
}
