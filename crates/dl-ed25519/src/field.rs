//! Arithmetic modulo p = 2^255 - 19 with five 51-bit limbs.
//!
//! Every operation is free of secret-dependent branches and memory accesses. Outputs of
//! [`Fe::add`], [`Fe::sub`] and [`Fe::mul`] have limbs below 2^52, which every operation
//! accepts as input.

const MASK: u64 = (1 << 51) - 1;

/// A field element; not necessarily fully reduced.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fe(pub(crate) [u64; 5]);

impl Fe {
    pub(crate) const ZERO: Fe = Fe([0; 5]);
    pub(crate) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// Propagates carries so every limb is below 2^51 (plus a tiny excess in limb 0).
    fn carry(mut l: [u64; 5]) -> Fe {
        let c = l[0] >> 51;
        l[0] &= MASK;
        l[1] += c;
        let c = l[1] >> 51;
        l[1] &= MASK;
        l[2] += c;
        let c = l[2] >> 51;
        l[2] &= MASK;
        l[3] += c;
        let c = l[3] >> 51;
        l[3] &= MASK;
        l[4] += c;
        let c = l[4] >> 51;
        l[4] &= MASK;
        l[0] += c * 19;
        Fe(l)
    }

    pub(crate) fn add(&self, b: &Fe) -> Fe {
        let a = &self.0;
        let b = &b.0;
        Fe::carry([
            a[0] + b[0],
            a[1] + b[1],
            a[2] + b[2],
            a[3] + b[3],
            a[4] + b[4],
        ])
    }

    /// `self - b`, computed as `self + 4p - b` so no limb underflows.
    pub(crate) fn sub(&self, b: &Fe) -> Fe {
        const P4_0: u64 = 4 * ((1 << 51) - 19);
        const P4_I: u64 = 4 * ((1 << 51) - 1);
        let a = &self.0;
        let b = &b.0;
        Fe::carry([
            a[0] + P4_0 - b[0],
            a[1] + P4_I - b[1],
            a[2] + P4_I - b[2],
            a[3] + P4_I - b[3],
            a[4] + P4_I - b[4],
        ])
    }

    pub(crate) fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    pub(crate) fn mul(&self, b: &Fe) -> Fe {
        let m = |x: u64, y: u64| u128::from(x) * u128::from(y);
        let [a0, a1, a2, a3, a4] = self.0;
        let [b0, b1, b2, b3, b4] = b.0;
        let (b1_19, b2_19, b3_19, b4_19) = (b1 * 19, b2 * 19, b3 * 19, b4 * 19);
        let r0 = m(a0, b0) + m(a1, b4_19) + m(a2, b3_19) + m(a3, b2_19) + m(a4, b1_19);
        let r1 = m(a0, b1) + m(a1, b0) + m(a2, b4_19) + m(a3, b3_19) + m(a4, b2_19);
        let r2 = m(a0, b2) + m(a1, b1) + m(a2, b0) + m(a3, b4_19) + m(a4, b3_19);
        let r3 = m(a0, b3) + m(a1, b2) + m(a2, b1) + m(a3, b0) + m(a4, b4_19);
        let r4 = m(a0, b4) + m(a1, b3) + m(a2, b2) + m(a3, b1) + m(a4, b0);
        let mask = u128::from(MASK);
        let c = r0 >> 51;
        let l0 = (r0 & mask) as u64;
        let r1 = r1 + c;
        let c = r1 >> 51;
        let l1 = (r1 & mask) as u64;
        let r2 = r2 + c;
        let c = r2 >> 51;
        let l2 = (r2 & mask) as u64;
        let r3 = r3 + c;
        let c = r3 >> 51;
        let l3 = (r3 & mask) as u64;
        let r4 = r4 + c;
        let c = (r4 >> 51) as u64;
        let l4 = (r4 & mask) as u64;
        // c < 2^62, so c * 19 fits in u64.
        let l0 = l0 + c * 19;
        let l1 = l1 + (l0 >> 51);
        Fe([l0 & MASK, l1, l2, l3, l4])
    }

    pub(crate) fn square(&self) -> Fe {
        self.mul(self)
    }

    fn pow2k(&self, k: u32) -> Fe {
        let mut x = *self;
        for _ in 0..k {
            x = x.square();
        }
        x
    }

    /// Returns `(self^(2^250 - 1), self^11)`, the common prefix of inversion and
    /// `pow22523`.
    fn pow_250(&self) -> (Fe, Fe) {
        let z2 = self.square();
        let z9 = self.mul(&z2.pow2k(2));
        let z11 = z2.mul(&z9);
        let z_5_0 = z9.mul(&z11.square());
        let z_10_0 = z_5_0.pow2k(5).mul(&z_5_0);
        let z_20_0 = z_10_0.pow2k(10).mul(&z_10_0);
        let z_40_0 = z_20_0.pow2k(20).mul(&z_20_0);
        let z_50_0 = z_40_0.pow2k(10).mul(&z_10_0);
        let z_100_0 = z_50_0.pow2k(50).mul(&z_50_0);
        let z_200_0 = z_100_0.pow2k(100).mul(&z_100_0);
        let z_250_0 = z_200_0.pow2k(50).mul(&z_50_0);
        (z_250_0, z11)
    }

    /// `self^(p - 2)`; 0 for 0.
    pub(crate) fn invert(&self) -> Fe {
        let (z_250_0, z11) = self.pow_250();
        z_250_0.pow2k(5).mul(&z11)
    }

    /// `self^((p - 5) / 8)`, used for square roots.
    pub(crate) fn pow22523(&self) -> Fe {
        let (z_250_0, _) = self.pow_250();
        z_250_0.pow2k(2).mul(self)
    }

    /// Reads 32 little-endian bytes; the top bit is ignored and values ≥ p are reduced.
    pub(crate) fn from_bytes(b: &[u8; 32]) -> Fe {
        let load = |i: usize| {
            let mut w = [0u8; 8];
            w.copy_from_slice(&b[i..i + 8]);
            u64::from_le_bytes(w)
        };
        Fe::carry([
            load(0) & MASK,
            (load(6) >> 3) & MASK,
            (load(12) >> 6) & MASK,
            (load(19) >> 1) & MASK,
            (load(24) >> 12) & MASK,
        ])
    }

    /// The canonical 32-byte encoding (fully reduced, top bit clear).
    pub(crate) fn to_bytes(self) -> [u8; 32] {
        let mut l = Fe::carry(self.0).0;
        // q = 1 when l >= p.
        let mut q = (l[0] + 19) >> 51;
        q = (l[1] + q) >> 51;
        q = (l[2] + q) >> 51;
        q = (l[3] + q) >> 51;
        q = (l[4] + q) >> 51;
        l[0] += 19 * q;
        l[1] += l[0] >> 51;
        l[0] &= MASK;
        l[2] += l[1] >> 51;
        l[1] &= MASK;
        l[3] += l[2] >> 51;
        l[2] &= MASK;
        l[4] += l[3] >> 51;
        l[3] &= MASK;
        l[4] &= MASK;
        let words = [
            l[0] | (l[1] << 51),
            (l[1] >> 13) | (l[2] << 38),
            (l[2] >> 26) | (l[3] << 25),
            (l[3] >> 39) | (l[4] << 12),
        ];
        let mut out = [0u8; 32];
        for (i, w) in words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// Constant-time equality of the reduced values.
    pub(crate) fn ct_eq(&self, b: &Fe) -> bool {
        let (x, y) = (self.to_bytes(), b.to_bytes());
        let mut d = 0u8;
        for i in 0..32 {
            d |= x[i] ^ y[i];
        }
        d == 0
    }

    /// The low bit of the reduced value ("negative" in RFC 8032).
    pub(crate) fn is_odd(&self) -> u8 {
        self.to_bytes()[0] & 1
    }

    /// Swaps `a` and `b` when `choice` is 1, without branching.
    pub(crate) fn cswap(a: &mut Fe, b: &mut Fe, choice: u64) {
        let mask = 0u64.wrapping_sub(choice);
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}

/// d = -121665 / 121666.
pub(crate) const D: [u8; 32] = [
    0xa3, 0x78, 0x59, 0x13, 0xca, 0x4d, 0xeb, 0x75, 0xab, 0xd8, 0x41, 0x41, 0x4d, 0x0a, 0x70, 0x00,
    0x98, 0xe8, 0x79, 0x77, 0x79, 0x40, 0xc7, 0x8c, 0x73, 0xfe, 0x6f, 0x2b, 0xee, 0x6c, 0x03, 0x52,
];

/// sqrt(-1) = 2^((p - 1) / 4).
pub(crate) const SQRT_M1: [u8; 32] = [
    0xb0, 0xa0, 0x0e, 0x4a, 0x27, 0x1b, 0xee, 0xc4, 0x78, 0xe4, 0x2f, 0xad, 0x06, 0x18, 0x43, 0x2f,
    0xa7, 0xd7, 0xfb, 0x3d, 0x99, 0x00, 0x4d, 0x2b, 0x0b, 0xdf, 0xc1, 0x4f, 0x80, 0x24, 0x83, 0x2b,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn small(n: u64) -> Fe {
        Fe([n, 0, 0, 0, 0])
    }

    #[test]
    fn constants() {
        let d = Fe::from_bytes(&D);
        // d * 121666 == -121665
        assert!(d.mul(&small(121666)).ct_eq(&small(121665).neg()));
        let i = Fe::from_bytes(&SQRT_M1);
        assert!(i.square().ct_eq(&Fe::ONE.neg()));
    }

    #[test]
    fn inversion_and_reduction() {
        let x = Fe::from_bytes(&[7u8; 32]);
        assert!(x.mul(&x.invert()).ct_eq(&Fe::ONE));
        // p itself encodes as zero; p + 1 as one.
        let mut p = [0xffu8; 32];
        p[0] = 0xed;
        p[31] = 0x7f;
        assert_eq!(Fe::from_bytes(&p).to_bytes(), [0u8; 32]);
        p[0] = 0xee;
        assert_eq!(Fe::from_bytes(&p).to_bytes(), Fe::ONE.to_bytes());
        // (p - 1) + 1 == 0 and 0 - 1 == p - 1.
        let mut m1 = [0xffu8; 32];
        m1[0] = 0xec;
        m1[31] = 0x7f;
        assert_eq!(Fe::from_bytes(&m1).add(&Fe::ONE).to_bytes(), [0u8; 32]);
        assert_eq!(Fe::ZERO.sub(&Fe::ONE).to_bytes(), m1);
        // Round trip of a canonical value.
        let mut v = [0x5au8; 32];
        v[31] = 0x3a;
        assert_eq!(Fe::from_bytes(&v).to_bytes(), v);
    }

    #[test]
    fn swap() {
        let (mut a, mut b) = (small(1), small(2));
        Fe::cswap(&mut a, &mut b, 0);
        assert_eq!((a.0[0], b.0[0]), (1, 2));
        Fe::cswap(&mut a, &mut b, 1);
        assert_eq!((a.0[0], b.0[0]), (2, 1));
    }
}
