//! xoshiro256** 1.0 by Blackman and Vigna (<https://prng.di.unimi.it/>).

use core::ops::{Bound, RangeBounds};

/// Fast, non-cryptographic 64-bit generator (xoshiro256**). Period 2^256 − 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    s: [u64; 4],
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Rng {
    /// Deterministic generator from a 64-bit seed, expanded with SplitMix64 as the
    /// authors recommend. Same seed, same sequence, on every platform.
    pub fn from_seed(seed: u64) -> Self {
        let mut sm = seed;
        Self {
            s: [
                splitmix64(&mut sm),
                splitmix64(&mut sm),
                splitmix64(&mut sm),
                splitmix64(&mut sm),
            ],
        }
    }

    /// Generator from a raw 256-bit state. An all-zero state is invalid and is replaced
    /// by `from_seed(0)`.
    pub fn from_state(s: [u64; 4]) -> Self {
        if s == [0; 4] {
            Self::from_seed(0)
        } else {
            Self { s }
        }
    }

    /// Generator seeded from the OS entropy source.
    pub fn from_os() -> Result<Self, crate::Error> {
        let b = crate::secure_bytes::<32>()?;
        let mut s = [0u64; 4];
        for (w, c) in s.iter_mut().zip(b.as_chunks::<8>().0) {
            *w = u64::from_le_bytes(*c);
        }
        Ok(Self::from_state(s))
    }

    /// Next 64 random bits.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Next 32 random bits (the high half, which has the best quality).
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform `f64` in `[0, 1)` with 53 bits of precision.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// `true` with probability 1/2.
    #[inline]
    pub fn next_bool(&mut self) -> bool {
        self.next_u64() >> 63 == 1
    }

    /// Fills `buf` with random bytes.
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        let (chunks, rest) = buf.as_chunks_mut::<8>();
        for c in chunks {
            *c = self.next_u64().to_le_bytes();
        }
        if !rest.is_empty() {
            let v = self.next_u64().to_le_bytes();
            rest.copy_from_slice(&v[..rest.len()]);
        }
    }

    /// Uniform integer in `0..n` without modulo bias (Lemire's method).
    ///
    /// # Panics
    /// Panics if `n == 0`.
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "below(0) has no valid output");
        let mut m = u128::from(self.next_u64()) * u128::from(n);
        if (m as u64) < n {
            let threshold = n.wrapping_neg() % n;
            while (m as u64) < threshold {
                m = u128::from(self.next_u64()) * u128::from(n);
            }
        }
        (m >> 64) as u64
    }

    /// Uniform value in `range`, e.g. `rng.range(1..=6)` or `rng.range(-5i32..5)`.
    ///
    /// # Panics
    /// Panics if the range is empty.
    pub fn range<T: Uniform>(&mut self, range: impl RangeBounds<T>) -> T {
        let low = match range.start_bound() {
            Bound::Included(&v) => v,
            Bound::Excluded(&v) => v.checked_next().expect("empty range"),
            Bound::Unbounded => T::MIN,
        };
        let high = match range.end_bound() {
            Bound::Included(&v) => v,
            Bound::Excluded(&v) => v.checked_prev().expect("empty range"),
            Bound::Unbounded => T::MAX,
        };
        assert!(low <= high, "empty range");
        T::sample_inclusive(self, low, high)
    }

    /// Fisher–Yates shuffle.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }

    /// A random element, or `None` for an empty slice.
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            items.get(self.below(items.len() as u64) as usize)
        }
    }
}

/// Integer types that [`Rng::range`] can sample.
pub trait Uniform: Copy + PartialOrd {
    /// Smallest value of the type.
    const MIN: Self;
    /// Largest value of the type.
    const MAX: Self;
    /// `self + 1`, or `None` on overflow.
    fn checked_next(self) -> Option<Self>;
    /// `self - 1`, or `None` on overflow.
    fn checked_prev(self) -> Option<Self>;
    /// Uniform value in `low..=high`; requires `low <= high`.
    fn sample_inclusive(rng: &mut Rng, low: Self, high: Self) -> Self;
}

macro_rules! uniform_int {
    ($($t:ty => $u:ty),*) => {$(
        impl Uniform for $t {
            const MIN: Self = <$t>::MIN;
            const MAX: Self = <$t>::MAX;
            fn checked_next(self) -> Option<Self> { self.checked_add(1) }
            fn checked_prev(self) -> Option<Self> { self.checked_sub(1) }
            fn sample_inclusive(rng: &mut Rng, low: Self, high: Self) -> Self {
                // Two's-complement distance, exact for signed and unsigned types.
                let span = high.wrapping_sub(low) as $u as u64;
                let offset = if span == u64::MAX { rng.next_u64() } else { rng.below(span + 1) };
                low.wrapping_add(offset as $u as $t)
            }
        }
    )*};
}

uniform_int!(u8 => u8, u16 => u16, u32 => u32, u64 => u64, usize => usize,
             i8 => u8, i16 => u16, i32 => u32, i64 => u64, isize => usize);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_sequence() {
        // Reference output of the C implementation for state {1, 2, 3, 4}.
        let mut r = Rng::from_state([1, 2, 3, 4]);
        let got: Vec<u64> = (0..6).map(|_| r.next_u64()).collect();
        assert_eq!(got, REFERENCE);
    }

    // First four values are the published ones for state {1,2,3,4}; all six were
    // cross-checked against an independent port of the reference C code.
    const REFERENCE: [u64; 6] = [
        11520,
        0,
        1509978240,
        1215971899390074240,
        1216172134540287360,
        607988272756665600,
    ];

    #[test]
    fn deterministic_seed() {
        let a: Vec<u64> = {
            let mut r = Rng::from_seed(42);
            (0..10).map(|_| r.next_u64()).collect()
        };
        let mut r = Rng::from_seed(42);
        let b: Vec<u64> = (0..10).map(|_| r.next_u64()).collect();
        assert_eq!(a, b);
        assert_ne!(Rng::from_seed(1).next_u64(), Rng::from_seed(2).next_u64());
    }

    #[test]
    fn ranges() {
        let mut r = Rng::from_seed(7);
        let mut seen = [false; 6];
        for _ in 0..1000 {
            let d = r.range(1..=6u8);
            assert!((1..=6).contains(&d));
            seen[(d - 1) as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
        for _ in 0..1000 {
            let v = r.range(-5i32..5);
            assert!((-5..5).contains(&v));
        }
        // Full-width ranges must not overflow.
        let _ = r.range(..=u64::MAX);
        let _ = r.range(i64::MIN..=i64::MAX);
        assert_eq!(r.range(3..4u32), 3);
    }

    #[test]
    #[should_panic(expected = "empty range")]
    fn empty_range_panics() {
        let mut r = Rng::from_seed(0);
        let _ = r.range(5..5u32);
    }

    #[test]
    fn uniformity_rough() {
        let mut r = Rng::from_seed(123);
        let mut buckets = [0u32; 10];
        for _ in 0..100_000 {
            buckets[r.below(10) as usize] += 1;
        }
        for b in buckets {
            assert!((9_000..11_000).contains(&b), "{buckets:?}");
        }
    }

    #[test]
    fn fill_and_choose() {
        let mut r = Rng::from_seed(9);
        let mut buf = [0u8; 13];
        r.fill_bytes(&mut buf);
        assert!(buf.iter().any(|&b| b != 0));
        assert!(r.choose::<u8>(&[]).is_none());
        assert!([1, 2, 3].contains(r.choose(&[1, 2, 3]).unwrap()));
    }
}
