//! Deterministic xorshift64* so shim losses are reproducible run to run.

/// Small PRNG; not for cryptography.
#[derive(Debug, Clone)]
pub struct XorShift(u64);

impl XorShift {
    /// Seeded generator (a zero seed is replaced, xorshift needs a non-zero state).
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// True with probability `p` (0..=1).
    pub fn chance(&mut self, p: f64) -> bool {
        if p <= 0.0 {
            return false;
        }
        ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chance_rate_is_close() {
        let mut r = XorShift::new(1);
        let hits = (0..100_000).filter(|_| r.chance(0.01)).count();
        assert!((800..1200).contains(&hits), "{hits}");
        assert!(!XorShift::new(2).chance(0.0));
    }
}
