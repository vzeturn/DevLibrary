//! Rounding modes and the rounding methods of [`Decimal`].

use crate::u256::{POW10, POW10_U128, U256};
use crate::{Decimal, Error, MAX_MANTISSA, MAX_SCALE};
use core::cmp::Ordering;

/// How to round a value that falls between two representable results.
///
/// Names follow C# `MidpointRounding` / Java `RoundingMode`; the Python `decimal`
/// equivalent is given for each. "Up" and "Down" refer to magnitude (away from / toward
/// zero), "Ceiling" and "Floor" to the number line.
///
/// | Mode | 2.5 | 1.5 | -2.5 | 2.4 | -2.6 | Python |
/// |---|---|---|---|---|---|---|
/// | `HalfEven` | 2 | 2 | -2 | 2 | -3 | `ROUND_HALF_EVEN` |
/// | `HalfUp` | 3 | 2 | -3 | 2 | -3 | `ROUND_HALF_UP` |
/// | `HalfDown` | 2 | 1 | -2 | 2 | -3 | `ROUND_HALF_DOWN` |
/// | `Up` | 3 | 2 | -3 | 3 | -3 | `ROUND_UP` |
/// | `Down` | 2 | 1 | -2 | 2 | -2 | `ROUND_DOWN` |
/// | `Ceiling` | 3 | 2 | -2 | 3 | -2 | `ROUND_CEILING` |
/// | `Floor` | 2 | 1 | -3 | 2 | -3 | `ROUND_FLOOR` |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundingMode {
    /// Round to nearest; ties go to the even neighbour (banker's rounding). Default of
    /// C# `Math.Round`, IEEE 754 and this crate's arithmetic.
    HalfEven,
    /// Round to nearest; ties go away from zero. What Excel `ROUND`, most invoice rules
    /// and Vietnamese e-invoice amounts use.
    HalfUp,
    /// Round to nearest; ties go toward zero.
    HalfDown,
    /// Away from zero.
    Up,
    /// Toward zero (truncate).
    Down,
    /// Toward positive infinity.
    Ceiling,
    /// Toward negative infinity.
    Floor,
}

impl RoundingMode {
    /// Whether the truncated magnitude must be incremented.
    ///
    /// `half` compares the discarded part with one half of the last kept unit,
    /// `nonzero` says whether anything non-zero was discarded, `odd` is the parity of the
    /// truncated magnitude and `neg` the sign of the value.
    #[inline]
    pub(crate) fn round_up(self, neg: bool, odd: bool, half: Ordering, nonzero: bool) -> bool {
        if !nonzero {
            return false;
        }
        match self {
            RoundingMode::HalfEven => half == Ordering::Greater || (half == Ordering::Equal && odd),
            RoundingMode::HalfUp => half != Ordering::Less,
            RoundingMode::HalfDown => half == Ordering::Greater,
            RoundingMode::Up => true,
            RoundingMode::Down => false,
            RoundingMode::Ceiling => !neg,
            RoundingMode::Floor => neg,
        }
    }
}

/// `n / d` rounded with `mode`; `d` must be non-zero. The result is at most `n / d + 1`.
#[inline]
pub(crate) fn div_round_u128(n: u128, d: u128, mode: RoundingMode, neg: bool) -> u128 {
    let (q, r) = (n / d, n % d);
    q + mode.round_up(neg, q & 1 == 1, r.cmp(&(d - r)), r != 0) as u128
}

/// `n / d` rounded with `mode`; `d` must be non-zero. `None` only if `n / d` is
/// `2^256 - 1` and must be incremented, which no caller can produce.
pub(crate) fn div_round_u256(n: U256, d: U256, mode: RoundingMode, neg: bool) -> Option<U256> {
    if let (Some(n), Some(d)) = (n.to_u128(), d.to_u128()) {
        // q <= n / 1 < 2^128 - 1 unless d == 1, in which case nothing is rounded.
        return Some(U256::from_u128(div_round_u128(n, d, mode, neg)));
    }
    let (q, r) = n.div_rem(d);
    // r < d, so d - r cannot underflow.
    let half = r.cmp(&d.checked_sub(r)?);
    if mode.round_up(neg, q.is_odd(), half, !r.is_zero()) {
        q.checked_add(U256::ONE)
    } else {
        Some(q)
    }
}

/// Builds the closest-fitting decimal for the exact value `±mag × 10^-scale`:
/// rounds once, half-even, to the largest scale `<= min(scale, 28)` whose mantissa has
/// at most 38 digits. `None` if the integer part needs more than 38 digits.
pub(crate) fn fit(mag: U256, neg: bool, scale: u32) -> Option<Decimal> {
    if let Some(m) = mag.to_u128()
        && m <= MAX_MANTISSA
        && scale <= MAX_SCALE
    {
        return Some(Decimal::from_mag(neg, m, scale));
    }
    let digits = mag.digits();
    let k = scale
        .saturating_sub(MAX_SCALE)
        .max(digits.saturating_sub(38));
    if k > scale {
        return None;
    }
    let mut s = scale - k;
    // k <= 77 because mag < 10^78; POW10 has entries up to 10^77.
    let mut q =
        div_round_u256(mag, *POW10.get(k as usize)?, RoundingMode::HalfEven, neg)?.to_u128()?;
    if q > MAX_MANTISSA {
        // Rounding carried into a 39th digit: q == 10^38 exactly.
        if s == 0 {
            return None;
        }
        q /= 10;
        s -= 1;
    }
    Some(Decimal::from_mag(neg, q, s))
}

impl Decimal {
    /// Rounds to `dp` decimal places with `mode`. Values that already have at most `dp`
    /// decimal places are returned unchanged (the scale is never increased; use
    /// [`with_scale`](Self::with_scale) for that). Never fails.
    ///
    /// ```
    /// use dl_decimal::{Decimal, RoundingMode};
    /// let x: Decimal = "2.345".parse().unwrap();
    /// assert_eq!(x.round_dp(2, RoundingMode::HalfEven).to_string(), "2.34");
    /// assert_eq!(x.round_dp(2, RoundingMode::HalfUp).to_string(), "2.35");
    /// ```
    pub fn round_dp(self, dp: u32, mode: RoundingMode) -> Decimal {
        let scale = self.scale();
        if dp >= scale {
            return self;
        }
        let neg = self.is_negative();
        let q = div_round_u128(
            self.mantissa.unsigned_abs(),
            POW10_U128[(scale - dp) as usize],
            mode,
            neg,
        );
        // q <= MAX_MANTISSA / 10 + 1, so it fits.
        Decimal::from_mag(neg, q, dp)
    }

    /// Rounds to `dp` decimal places, ties away from zero ([`RoundingMode::HalfUp`]).
    ///
    /// HalfUp is the default because it is what Excel `ROUND`, Vietnamese e-invoice
    /// rules and most invoicing software do; arithmetic results use HalfEven instead.
    pub fn round(self, dp: u32) -> Decimal {
        self.round_dp(dp, RoundingMode::HalfUp)
    }

    /// Truncates (rounds toward zero) to `dp` decimal places.
    pub fn trunc(self, dp: u32) -> Decimal {
        self.round_dp(dp, RoundingMode::Down)
    }

    /// Largest integer `<=` self, with scale 0.
    pub fn floor(self) -> Decimal {
        self.round_dp(0, RoundingMode::Floor)
    }

    /// Smallest integer `>=` self, with scale 0.
    pub fn ceil(self) -> Decimal {
        self.round_dp(0, RoundingMode::Ceiling)
    }

    /// The integer part (truncated toward zero), with scale 0.
    pub fn integer_part(self) -> Decimal {
        self.trunc(0)
    }

    /// The fractional part, `self - self.integer_part()`, with the sign of `self` and
    /// the same scale.
    pub fn fract(self) -> Decimal {
        // 10^28 < i128::MAX.
        let p = POW10_U128[self.scale as usize] as i128;
        Decimal {
            mantissa: self.mantissa % p,
            scale: self.scale,
        }
    }

    /// Returns the same value with exactly `dp` decimal places, rounding with `mode` if
    /// `dp` is smaller than the current scale and appending zeros if it is larger, e.g.
    /// to store or print every amount with two decimals.
    ///
    /// ```
    /// use dl_decimal::{Decimal, RoundingMode};
    /// let x: Decimal = "1.5".parse().unwrap();
    /// assert_eq!(x.rescale(2, RoundingMode::HalfUp).unwrap().to_string(), "1.50");
    /// let y: Decimal = "1.005".parse().unwrap();
    /// assert_eq!(y.rescale(2, RoundingMode::HalfUp).unwrap().to_string(), "1.01");
    /// assert_eq!(y.rescale(2, RoundingMode::HalfEven).unwrap().to_string(), "1.00");
    /// ```
    ///
    /// # Errors
    /// [`Error::ScaleTooLarge`] if `dp > 28`; [`Error::Overflow`] if the padded mantissa
    /// would exceed 38 digits.
    pub fn rescale(self, dp: u32, mode: RoundingMode) -> Result<Decimal, Error> {
        if dp > MAX_SCALE {
            return Err(Error::ScaleTooLarge);
        }
        let scale = self.scale();
        if dp <= scale {
            return Ok(self.round_dp(dp, mode));
        }
        let m = self
            .mantissa
            .unsigned_abs()
            .checked_mul(POW10_U128[(dp - scale) as usize]);
        match m {
            Some(m) if m <= MAX_MANTISSA => Ok(Decimal::from_mag(self.is_negative(), m, dp)),
            _ => Err(Error::Overflow),
        }
    }

    /// Alias of [`rescale`](Self::rescale).
    ///
    /// # Errors
    /// Same as [`rescale`](Self::rescale).
    #[inline]
    pub fn with_scale(self, dp: u32, mode: RoundingMode) -> Result<Decimal, Error> {
        self.rescale(dp, mode)
    }

    /// Rounds to the nearest multiple of `|step|` with `mode`, e.g. cash amounts in VND
    /// to 500 or 1000, or prices to 0.05. The result has the scale of `step` (fewer if
    /// that is the only way to fit 38 digits exactly).
    ///
    /// ```
    /// use dl_decimal::{Decimal, RoundingMode};
    /// let total = Decimal::from(123_250);
    /// let cash = total.round_to_multiple(Decimal::from(500), RoundingMode::HalfUp).unwrap();
    /// assert_eq!(cash, Decimal::from(123_500));
    /// ```
    ///
    /// # Errors
    /// [`Error::DivisionByZero`] if `step` is zero; [`Error::Overflow`] if the result does
    /// not fit.
    pub fn round_to_multiple(self, step: Decimal, mode: RoundingMode) -> Result<Decimal, Error> {
        if step.is_zero() {
            return Err(Error::DivisionByZero);
        }
        let neg = self.is_negative();
        let (s1, s2) = (self.scale(), step.scale());
        let s = s1.max(s2);
        let b = step.mantissa.unsigned_abs();
        // Both aligned magnitudes are < 10^38 * 10^28.
        let num = U256::mul_u128(self.mantissa.unsigned_abs(), POW10_U128[(s - s1) as usize]);
        let den = U256::mul_u128(b, POW10_U128[(s - s2) as usize]);
        let n = div_round_u256(num, den, mode, neg).ok_or(Error::Overflow)?;
        let mut prod = n.checked_mul(U256::from_u128(b)).ok_or(Error::Overflow)?;
        let mut scale = s2;
        let max = U256::from_u128(MAX_MANTISSA);
        let ten = U256::from_u128(10);
        while prod > max && scale > 0 {
            let (q, r) = prod.div_rem(ten);
            if !r.is_zero() {
                break;
            }
            prod = q;
            scale -= 1;
        }
        match prod.to_u128() {
            Some(m) if m <= MAX_MANTISSA => Ok(Decimal::from_mag(neg, m, scale)),
            _ => Err(Error::Overflow),
        }
    }
}
