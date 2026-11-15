//! Money helpers: percentages and remainder-exact allocation.

use crate::round::fit;
use crate::u256::{POW10_U128, U256};
use crate::{Decimal, Error, MAX_MANTISSA, MAX_SCALE};
use alloc::vec::Vec;

/// The ratios as integers at their largest (normalized) scale.
fn integer_weights<R: Copy + Into<Decimal>>(ratios: &[R]) -> Result<Vec<i128>, Error> {
    let ds: Vec<Decimal> = ratios.iter().map(|&r| r.into().normalize()).collect();
    let s = ds.iter().map(|d| d.scale()).max().unwrap_or(0);
    ds.iter()
        .map(|d| {
            // 10^28 < i128::MAX.
            let p = POW10_U128[(s - d.scale()) as usize] as i128;
            d.mantissa().checked_mul(p).ok_or(Error::Overflow)
        })
        .collect()
}

impl Decimal {
    /// `self × rate / 100`, e.g. VAT: `net.percent_of(Decimal::from(10))`. The exact
    /// result has scale `self.scale() + rate.scale() + 2` and is rounded half-even only
    /// beyond 28 decimal places or 38 digits; round it yourself to the currency unit.
    ///
    /// ```
    /// use dl_decimal::Decimal;
    /// let net = Decimal::from(1_234_567);
    /// let vat = net.percent_of(Decimal::from(8)).unwrap();
    /// assert_eq!(vat.to_string(), "98765.36");
    /// assert_eq!(vat.round(0), Decimal::from(98_765));
    /// ```
    ///
    /// # Errors
    /// [`Error::Overflow`] if the result does not fit.
    pub fn percent_of(self, rate: Decimal) -> Result<Decimal, Error> {
        let p = U256::mul_u128(self.mantissa.unsigned_abs(), rate.mantissa.unsigned_abs());
        fit(
            p,
            self.is_negative() != rate.is_negative(),
            self.scale() + rate.scale() + 2,
        )
        .ok_or(Error::Overflow)
    }

    /// `|self|` as a whole number of `10^-dp` units.
    fn units(self, dp: u32) -> Result<u128, Error> {
        if dp > MAX_SCALE {
            return Err(Error::ScaleTooLarge);
        }
        let mag = self.mantissa.unsigned_abs();
        let scale = self.scale();
        if dp >= scale {
            match mag.checked_mul(POW10_U128[(dp - scale) as usize]) {
                Some(u) if u <= MAX_MANTISSA => Ok(u),
                _ => Err(Error::Overflow),
            }
        } else {
            let p = POW10_U128[(scale - dp) as usize];
            if !mag.is_multiple_of(p) {
                return Err(Error::ScaleTooLarge);
            }
            Ok(mag / p)
        }
    }

    /// Splits `self` into parts proportional to `ratios`, each with exactly `dp` decimal
    /// places, so that the parts add up **exactly** to `self`.
    ///
    /// `ratios` can be integers (`&[1, 1, 1]`, `&[30u32, 70]`) or decimals, e.g. the line
    /// amounts an invoice-level discount is spread over.
    ///
    /// Each part first gets the exact share `|self| × ratio / sum(ratios)` rounded down
    /// to a whole number of `10^-dp` units; the units left over (fewer than
    /// `ratios.len()`) go one by one to the parts with the largest discarded fractions,
    /// earlier parts first on ties (the largest remainder, or Hamilton, method). So every
    /// part is within one unit of its exact share, and a ratio of 0 always gets 0.
    /// A negative amount is split like its absolute value and every part is negated, so
    /// `allocate(-x) == -allocate(x)`. Negative ratios are allowed as long as the ratios
    /// do not sum to zero; the same rule (round down, then hand out the leftover units)
    /// applies.
    ///
    /// ```
    /// use dl_decimal::Decimal;
    /// let parts = Decimal::from(100).allocate(&[1, 1, 1], 0).unwrap();
    /// assert_eq!(parts, [34, 33, 33].map(Decimal::from));
    /// let parts = Decimal::from(1).allocate(&[30, 70], 0).unwrap();
    /// assert_eq!(parts, [0, 1].map(Decimal::from));
    ///
    /// // A 50,000 VND invoice discount spread over the line amounts.
    /// let lines: [Decimal; 3] = ["120000", "45500", "9900"].map(|s| s.parse().unwrap());
    /// let parts = Decimal::from(50_000).allocate(&lines, 0).unwrap();
    /// assert_eq!(parts, [34208, 12970, 2822].map(Decimal::from));
    /// ```
    ///
    /// # Errors
    /// [`Error::DivisionByZero`] if `ratios` is empty or sums to 0;
    /// [`Error::ScaleTooLarge`] if `dp > 28` or `self` has non-zero digits beyond `dp`
    /// decimal places (it could not be split exactly); [`Error::Overflow`] if `self`
    /// needs more than 38 digits at `dp` decimal places, a part does not fit, or the
    /// ratios written with their largest scale do not fit in an `i128` (for example
    /// `1e30` next to `0.000000001`).
    pub fn allocate<R: Copy + Into<Decimal>>(
        self,
        ratios: &[R],
        dp: u32,
    ) -> Result<Vec<Decimal>, Error> {
        let units = self.units(dp)?;
        let weights = integer_weights(ratios)?;
        let total = weights
            .iter()
            .try_fold(0i128, |acc, &w| acc.checked_add(w))
            .ok_or(Error::Overflow)?;
        if total == 0 {
            return Err(Error::DivisionByZero);
        }
        let tmag = total.unsigned_abs();
        let tmag256 = U256::from_u128(tmag);
        // Exact share i = units × w_i / total = floor_i + frac_i / tmag, 0 <= frac_i < tmag.
        let mut floors: Vec<(bool, u128)> = Vec::with_capacity(weights.len());
        let mut fracs: Vec<u128> = Vec::with_capacity(weights.len());
        let mut frac_sum = U256::ZERO;
        for &w in &weights {
            let share_neg = (w < 0) != (total < 0);
            let (q, r) = U256::mul_u128(units, w.unsigned_abs()).div_rem(tmag256);
            // r < tmag, so it fits in u128.
            let r = r.to_u128().ok_or(Error::Overflow)?;
            // The part ends up within one unit of the share; anything beyond
            // MAX_MANTISSA + 1 cannot fit.
            let q = q
                .to_u128()
                .filter(|&q| q <= MAX_MANTISSA + 1)
                .ok_or(Error::Overflow)?;
            let (floor, frac) = if share_neg && r != 0 {
                // floor(-(q + r/t)) = -(q + 1), leaving (t - r) / t.
                ((true, q + 1), tmag - r)
            } else {
                ((share_neg, q), r)
            };
            floors.push(floor);
            fracs.push(frac);
            // At most 2^64 terms below 2^128 each: no overflow in 256 bits.
            frac_sum = frac_sum
                .checked_add(U256::from_u128(frac))
                .ok_or(Error::Overflow)?;
        }
        // units - sum(floor_i) = sum(frac_i) / tmag, an exact integer below ratios.len().
        let left = frac_sum
            .div_rem(tmag256)
            .0
            .to_u128()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(Error::Overflow)?;
        let mut bump = alloc::vec![false; weights.len()];
        if left > 0 {
            let mut order: Vec<usize> = (0..weights.len()).collect();
            order.sort_unstable_by(|&a, &b| fracs[b].cmp(&fracs[a]).then(a.cmp(&b)));
            for &i in &order[..left] {
                bump[i] = true;
            }
        }
        let flip = self.is_negative();
        floors
            .into_iter()
            .zip(bump)
            .map(|((neg, mag), up)| {
                // Adding one unit moves a negative floor toward zero.
                let (neg, mag) = match (neg, up) {
                    (_, false) => (neg, mag),
                    (false, true) => (false, mag + 1),
                    // Only parts with a non-zero fraction are bumped (the fractions sum to
                    // left × tmag and each is below tmag, so more than `left` of them are
                    // non-zero and they sort first). A negative floor with a non-zero
                    // fraction is -(q + 1), so mag >= 1 here.
                    (true, true) => (mag > 1, mag - 1),
                };
                if mag > MAX_MANTISSA {
                    return Err(Error::Overflow);
                }
                Ok(Decimal::from_mag(neg != flip && mag != 0, mag, dp))
            })
            .collect()
    }

    /// Splits `self` into `n` parts with exactly `dp` decimal places that add up exactly
    /// to `self`; the first `units % n` parts get one extra unit.
    ///
    /// ```
    /// use dl_decimal::Decimal;
    /// let parts = Decimal::from(100).split(3, 0).unwrap();
    /// assert_eq!(parts, [34, 33, 33].map(Decimal::from));
    /// let bill: Decimal = "-0.05".parse().unwrap();
    /// assert_eq!(bill.split(2, 2).unwrap(), ["-0.03", "-0.02"].map(|s| s.parse::<Decimal>().unwrap()));
    /// ```
    ///
    /// # Errors
    /// [`Error::DivisionByZero`] if `n == 0`; otherwise as [`allocate`](Self::allocate).
    pub fn split(self, n: usize, dp: u32) -> Result<Vec<Decimal>, Error> {
        if n == 0 {
            return Err(Error::DivisionByZero);
        }
        let units = self.units(dp)?;
        let neg = self.is_negative();
        let n128 = n as u128;
        let (base, rem) = (units / n128, (units % n128) as usize);
        Ok((0..n)
            .map(|i| Decimal::from_mag(neg, base + u128::from(i < rem), dp))
            .collect())
    }
}
