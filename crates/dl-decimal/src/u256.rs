//! Minimal unsigned 256-bit integer for exact intermediates.
//!
//! Only what the decimal code needs: full 128×128 products, multiplication with overflow
//! detection, add/sub, comparison, division with remainder (Knuth algorithm D on 64-bit
//! limbs) and digit counting. Every value the crate builds stays below 10^77 < 2^256.

use core::cmp::Ordering;

/// Little-endian 64-bit limbs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct U256(pub(crate) [u64; 4]);

/// 10^k as `u128` for k in 0..=38.
pub(crate) const POW10_U128: [u128; 39] = {
    let mut t = [1u128; 39];
    let mut i = 1;
    while i < 39 {
        t[i] = t[i - 1] * 10;
        i += 1;
    }
    t
};

/// 10^k as `U256` for k in 0..=77 (10^77 < 2^256 < 10^78).
pub(crate) const POW10: [U256; 78] = {
    let mut t = [U256::ONE; 78];
    let mut i = 1;
    while i < 78 {
        t[i] = t[i - 1].mul_small(10);
        i += 1;
    }
    t
};

impl U256 {
    pub(crate) const ZERO: U256 = U256([0; 4]);
    pub(crate) const ONE: U256 = U256([1, 0, 0, 0]);

    #[inline]
    pub(crate) const fn from_u128(v: u128) -> Self {
        U256([v as u64, (v >> 64) as u64, 0, 0])
    }

    /// The value as `u128`, or `None` if it does not fit.
    #[inline]
    pub(crate) const fn to_u128(self) -> Option<u128> {
        if self.0[2] == 0 && self.0[3] == 0 {
            Some(self.0[0] as u128 | (self.0[1] as u128) << 64)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) const fn is_zero(self) -> bool {
        self.0[0] == 0 && self.0[1] == 0 && self.0[2] == 0 && self.0[3] == 0
    }

    #[inline]
    pub(crate) const fn is_odd(self) -> bool {
        self.0[0] & 1 == 1
    }

    /// Full product of two `u128` values (never overflows 256 bits).
    #[inline]
    pub(crate) const fn mul_u128(a: u128, b: u128) -> Self {
        let (a0, a1) = (a as u64 as u128, a >> 64);
        let (b0, b1) = (b as u64 as u128, b >> 64);
        let p00 = a0 * b0;
        let p01 = a0 * b1;
        let p10 = a1 * b0;
        let p11 = a1 * b1;
        // Each term is < 2^64, so the sum is < 3 * 2^64.
        let mid = (p00 >> 64) + (p01 as u64 as u128) + (p10 as u64 as u128);
        // This is exactly floor(a * b / 2^128) < 2^128, and every partial sum is
        // bounded by the final value, so no step overflows.
        let hi = p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64);
        U256([p00 as u64, mid as u64, hi as u64, (hi >> 64) as u64])
    }

    /// `self * m` truncated to 256 bits. Only used on values known to fit (const tables).
    const fn mul_small(self, m: u64) -> Self {
        let mut out = [0u64; 4];
        let mut carry: u128 = 0;
        let mut i = 0;
        while i < 4 {
            let p = self.0[i] as u128 * m as u128 + carry;
            out[i] = p as u64;
            carry = p >> 64;
            i += 1;
        }
        U256(out)
    }

    /// `self * rhs`, or `None` if the product needs more than 256 bits.
    pub(crate) fn checked_mul(self, rhs: U256) -> Option<U256> {
        let mut out = [0u64; 8];
        for i in 0..4 {
            if self.0[i] == 0 {
                continue;
            }
            let mut carry: u128 = 0;
            for j in 0..4 {
                // (2^64-1)^2 + 2 * (2^64-1) = 2^128 - 1: never overflows.
                let t = self.0[i] as u128 * rhs.0[j] as u128 + out[i + j] as u128 + carry;
                out[i + j] = t as u64;
                carry = t >> 64;
            }
            out[i + 4] = carry as u64;
        }
        if out[4..].iter().any(|&l| l != 0) {
            return None;
        }
        Some(U256([out[0], out[1], out[2], out[3]]))
    }

    /// `self + rhs`, or `None` on overflow.
    pub(crate) fn checked_add(self, rhs: U256) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut carry = false;
        for (i, slot) in out.iter_mut().enumerate() {
            let (s1, c1) = self.0[i].overflowing_add(rhs.0[i]);
            let (s2, c2) = s1.overflowing_add(carry as u64);
            *slot = s2;
            carry = c1 || c2;
        }
        if carry { None } else { Some(U256(out)) }
    }

    /// `self - rhs`, or `None` if `rhs > self`.
    pub(crate) fn checked_sub(self, rhs: U256) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut borrow = false;
        for (i, slot) in out.iter_mut().enumerate() {
            let (d1, b1) = self.0[i].overflowing_sub(rhs.0[i]);
            let (d2, b2) = d1.overflowing_sub(borrow as u64);
            *slot = d2;
            borrow = b1 || b2;
        }
        if borrow { None } else { Some(U256(out)) }
    }

    /// Number of decimal digits; zero has one digit.
    pub(crate) fn digits(self) -> u32 {
        if let Some(v) = self.to_u128() {
            return digits_u128(v);
        }
        // Smallest k with self < 10^k. self >= 2^128 > 10^38, and self < 2^256 < 10^78.
        let (mut lo, mut hi) = (39usize, 78usize);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if mid < 78 && self < POW10[mid] {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        lo as u32
    }

    /// Number of limbs up to and including the highest non-zero one.
    fn limbs(self) -> usize {
        let mut n = 4;
        while n > 0 && self.0[n - 1] == 0 {
            n -= 1;
        }
        n
    }

    /// Quotient and remainder. Division by zero returns `(0, self)`; callers never do it.
    pub(crate) fn div_rem(self, d: U256) -> (U256, U256) {
        if d.is_zero() {
            return (U256::ZERO, self);
        }
        if let (Some(n), Some(dv)) = (self.to_u128(), d.to_u128()) {
            return (U256::from_u128(n / dv), U256::from_u128(n % dv));
        }
        if self < d {
            return (U256::ZERO, self);
        }
        let dl = d.limbs();
        if dl == 1 {
            let dv = d.0[0] as u128;
            let mut q = [0u64; 4];
            let mut r: u128 = 0;
            for i in (0..4).rev() {
                let cur = (r << 64) | self.0[i] as u128;
                q[i] = (cur / dv) as u64;
                r = cur % dv;
            }
            return (U256(q), U256::from_u128(r));
        }
        knuth_div(self, d, dl)
    }
}

/// Knuth TAOCP vol. 2, 4.3.1, algorithm D, with base 2^64. Requires `d` to have
/// `dl >= 2` limbs and `n >= d`.
fn knuth_div(n: U256, d: U256, dl: usize) -> (U256, U256) {
    const B: u128 = 1 << 64;
    // D1: normalize so the top limb of the divisor has its high bit set.
    let s = d.0[dl - 1].leading_zeros();
    let mut dn = [0u64; 4];
    let mut u = [0u64; 5];
    if s == 0 {
        dn = d.0;
        u[..4].copy_from_slice(&n.0);
    } else {
        for i in (1..dl).rev() {
            dn[i] = (d.0[i] << s) | (d.0[i - 1] >> (64 - s));
        }
        dn[0] = d.0[0] << s;
        u[4] = n.0[3] >> (64 - s);
        for i in (1..4).rev() {
            u[i] = (n.0[i] << s) | (n.0[i - 1] >> (64 - s));
        }
        u[0] = n.0[0] << s;
    }
    let top = dn[dl - 1] as u128;
    let second = dn[dl - 2] as u128;
    let mut q = [0u64; 4];
    for j in (0..=4 - dl).rev() {
        // D3: estimate qhat from the top two limbs, then correct it (at most twice).
        let num = ((u[j + dl] as u128) << 64) | u[j + dl - 1] as u128;
        let mut qhat = num / top;
        let mut rhat = num % top;
        // `qhat * second` is only evaluated when qhat < B, so it fits in u128.
        while qhat >= B || qhat * second > ((rhat << 64) | u[j + dl - 2] as u128) {
            qhat -= 1;
            rhat += top;
            if rhat >= B {
                break;
            }
        }
        // D4: multiply and subtract qhat * dn from u[j..=j+dl].
        let mut carry: u128 = 0;
        let mut borrow = false;
        for i in 0..dl {
            // qhat <= 2^64-1, so qhat * dn[i] + carry < 2^128.
            let p = qhat * dn[i] as u128 + carry;
            carry = p >> 64;
            let (t1, b1) = u[i + j].overflowing_sub(p as u64);
            let (t2, b2) = t1.overflowing_sub(borrow as u64);
            u[i + j] = t2;
            borrow = b1 || b2;
        }
        let (t1, b1) = u[j + dl].overflowing_sub(carry as u64);
        let (t2, b2) = t1.overflowing_sub(borrow as u64);
        u[j + dl] = t2;
        // D6: the estimate was one too large; add the divisor back.
        if b1 || b2 {
            qhat -= 1;
            let mut c = false;
            for i in 0..dl {
                let (s1, c1) = u[i + j].overflowing_add(dn[i]);
                let (s2, c2) = s1.overflowing_add(c as u64);
                u[i + j] = s2;
                c = c1 || c2;
            }
            u[j + dl] = u[j + dl].wrapping_add(c as u64);
        }
        q[j] = qhat as u64;
    }
    // D8: the remainder is u[0..dl] shifted back.
    let mut r = [0u64; 4];
    for i in 0..dl {
        r[i] = if s == 0 {
            u[i]
        } else {
            (u[i] >> s) | (u[i + 1] << (64 - s))
        };
    }
    (U256(q), U256(r))
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        for i in (0..4).rev() {
            match self.0[i].cmp(&other.0[i]) {
                Ordering::Equal => continue,
                o => return o,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Number of decimal digits of a `u128`; zero has one digit.
#[inline]
pub(crate) const fn digits_u128(v: u128) -> u32 {
    match v.checked_ilog10() {
        Some(l) => l + 1,
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dl_rand::Rng;

    /// Bit-by-bit restoring division: slow, but obviously correct.
    fn reference_div(n: U256, d: U256) -> (U256, U256) {
        let mut q = U256::ZERO;
        let mut r = U256::ZERO;
        for bit in (0..256).rev() {
            // r = (r << 1) | bit(n)
            let mut nr = [0u64; 4];
            for i in (0..4).rev() {
                nr[i] = (r.0[i] << 1) | if i > 0 { r.0[i - 1] >> 63 } else { 0 };
            }
            nr[0] |= (n.0[bit / 64] >> (bit % 64)) & 1;
            r = U256(nr);
            if r >= d {
                r = r.checked_sub(d).unwrap();
                q.0[bit / 64] |= 1 << (bit % 64);
            }
        }
        (q, r)
    }

    fn random_u256(rng: &mut Rng) -> U256 {
        let mut v = U256([
            rng.next_u64(),
            rng.next_u64(),
            rng.next_u64(),
            rng.next_u64(),
        ]);
        // Random limb count and occasional extreme limb values, to hit Knuth's corner cases.
        let limbs = rng.range(1..=4usize);
        for l in v.0.iter_mut().skip(limbs) {
            *l = 0;
        }
        for l in v.0.iter_mut().take(limbs) {
            match rng.below(6) {
                0 => *l = u64::MAX,
                1 => *l = 0,
                2 => *l = 1 << 63,
                _ => {}
            }
        }
        v
    }

    #[test]
    fn knuth_matches_reference() {
        let mut rng = Rng::from_seed(0x0dec_13a1);
        for _ in 0..20_000 {
            let n = random_u256(&mut rng);
            let d = random_u256(&mut rng);
            if d.is_zero() {
                continue;
            }
            assert_eq!(n.div_rem(d), reference_div(n, d), "{n:?} / {d:?}");
        }
    }

    #[test]
    fn products_and_powers() {
        assert_eq!(POW10[38].to_u128(), Some(POW10_U128[38]));
        assert_eq!(POW10[39].to_u128(), None);
        assert_eq!(POW10[38], U256::mul_u128(POW10_U128[19], POW10_U128[19]));
        assert_eq!(POW10[77].digits(), 78);
        assert_eq!(POW10[77].checked_mul(U256::from_u128(12)), None);
        assert_eq!((POW10[76].checked_sub(U256::ONE)).unwrap().digits(), 76);
        assert_eq!(U256::from_u128(u128::MAX).digits(), 39);
        assert_eq!(U256::ZERO.digits(), 1);
        let m = U256::mul_u128(u128::MAX, u128::MAX);
        // (2^128 - 1)^2 = 2^256 - 2^129 + 1
        assert_eq!(m, U256([1, 0, u64::MAX - 1, u64::MAX]));
        assert_eq!(
            m.div_rem(U256::from_u128(u128::MAX)),
            (U256::from_u128(u128::MAX), U256::ZERO)
        );
        let mut rng = Rng::from_seed(5);
        for _ in 0..2_000 {
            let a = u128::from(rng.next_u64()) << 64 | u128::from(rng.next_u64());
            let b = u128::from(rng.next_u64());
            let p = U256::mul_u128(a, b);
            assert_eq!(
                p,
                U256::from_u128(a).checked_mul(U256::from_u128(b)).unwrap()
            );
            let (q, r) = p.div_rem(U256::from_u128(b.max(1)));
            assert_eq!(
                (q.to_u128().unwrap(), r),
                (if b == 0 { 0 } else { a }, U256::ZERO)
            );
        }
        for (k, p) in POW10.iter().enumerate() {
            assert_eq!(p.digits(), k as u32 + 1);
            if k > 0 {
                assert_eq!(p.checked_sub(U256::ONE).unwrap().digits(), k as u32);
            }
        }
        for (k, &p) in POW10_U128.iter().enumerate() {
            assert_eq!(POW10[k].to_u128(), Some(p));
            assert_eq!(digits_u128(p - 1).max(1), (k as u32).max(1));
        }
    }
}
