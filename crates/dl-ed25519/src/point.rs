//! Points of edwards25519 (-x^2 + y^2 = 1 + d x^2 y^2) in extended coordinates.

use crate::field::{D, Fe, SQRT_M1};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

/// The base point's encoding (y = 4/5, x even).
const BASE: [u8; 32] = [
    0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
];

impl Point {
    pub(crate) const IDENTITY: Point = Point {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };

    pub(crate) fn base() -> Point {
        match Point::decode(&BASE) {
            Some(p) => p,
            None => unreachable!("the base point encoding is valid"),
        }
    }

    /// Decodes a point. Like Go's `crypto/ed25519` (and most implementations), a y ≥ p is
    /// read modulo p and a "negative zero" x is accepted; anything off the curve is
    /// refused.
    pub(crate) fn decode(b: &[u8; 32]) -> Option<Point> {
        let y = Fe::from_bytes(b);
        let sign = b[31] >> 7;
        let d = Fe::from_bytes(&D);
        let yy = y.square();
        let u = yy.sub(&Fe::ONE);
        let v = d.mul(&yy).add(&Fe::ONE);
        // x = u v^3 (u v^7)^((p-5)/8)
        let v3 = v.square().mul(&v);
        let v7 = v3.square().mul(&v);
        let mut x = u.mul(&v3).mul(&u.mul(&v7).pow22523());
        let vxx = v.mul(&x.square());
        if !vxx.ct_eq(&u) {
            if vxx.ct_eq(&u.neg()) {
                x = x.mul(&Fe::from_bytes(&SQRT_M1));
            } else {
                return None;
            }
        }
        if x.is_odd() != sign {
            x = x.neg();
        }
        Some(Point {
            x,
            y,
            z: Fe::ONE,
            t: x.mul(&y),
        })
    }

    pub(crate) fn encode(&self) -> [u8; 32] {
        let zi = self.z.invert();
        let x = self.x.mul(&zi);
        let y = self.y.mul(&zi);
        let mut out = y.to_bytes();
        out[31] |= x.is_odd() << 7;
        out
    }

    pub(crate) fn neg(&self) -> Point {
        Point {
            x: self.x.neg(),
            y: self.y,
            z: self.z,
            t: self.t.neg(),
        }
    }

    /// Complete addition (RFC 8032 section 5.1.4); also correct for doubling.
    pub(crate) fn add(&self, q: &Point) -> Point {
        let d2 = Fe::from_bytes(&D).add(&Fe::from_bytes(&D));
        let a = self.y.sub(&self.x).mul(&q.y.sub(&q.x));
        let b = self.y.add(&self.x).mul(&q.y.add(&q.x));
        let c = self.t.mul(&d2).mul(&q.t);
        let zz = self.z.mul(&q.z);
        let d = zz.add(&zz);
        let e = b.sub(&a);
        let f = d.sub(&c);
        let g = d.add(&c);
        let h = b.add(&a);
        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    fn cswap(a: &mut Point, b: &mut Point, choice: u64) {
        Fe::cswap(&mut a.x, &mut b.x, choice);
        Fe::cswap(&mut a.y, &mut b.y, choice);
        Fe::cswap(&mut a.z, &mut b.z, choice);
        Fe::cswap(&mut a.t, &mut b.t, choice);
    }

    /// `[k]P` for a little-endian 256-bit `k`, as a Montgomery ladder: the same sequence
    /// of operations for every `k`.
    pub(crate) fn mul(&self, k: &[u8; 32]) -> Point {
        let mut r0 = Point::IDENTITY;
        let mut r1 = *self;
        for i in (0..256).rev() {
            let bit = u64::from((k[i / 8] >> (i % 8)) & 1);
            Point::cswap(&mut r0, &mut r1, bit);
            r1 = r0.add(&r1);
            r0 = r0.add(&r0);
            Point::cswap(&mut r0, &mut r1, bit);
        }
        r0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_point_round_trips_and_has_order_l() {
        let b = Point::base();
        assert_eq!(b.encode(), BASE);
        // [L]B is the identity.
        let l: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
        ];
        assert_eq!(b.mul(&l).encode(), Point::IDENTITY.encode());
        // [2]B by the ladder equals B + B.
        let mut two = [0u8; 32];
        two[0] = 2;
        assert_eq!(b.mul(&two).encode(), b.add(&b).encode());
        // P + (-P) is the identity.
        assert_eq!(b.add(&b.neg()).encode(), Point::IDENTITY.encode());
    }

    #[test]
    fn decoding_finds_curve_points_and_refuses_others() {
        let d = Fe::from_bytes(&D);
        let (mut ok, mut refused) = (0, 0);
        for n in 2u8..40 {
            let mut enc = [0u8; 32];
            enc[0] = n;
            match Point::decode(&enc) {
                Some(p) => {
                    // -x^2 + y^2 == 1 + d x^2 y^2
                    let (xx, yy) = (p.x.square(), p.y.square());
                    let lhs = yy.sub(&xx);
                    let rhs = Fe::ONE.add(&d.mul(&xx).mul(&yy));
                    assert!(lhs.ct_eq(&rhs), "y = {n}");
                    assert_eq!(p.encode(), enc, "y = {n}");
                    ok += 1;
                }
                None => refused += 1,
            }
        }
        // About half of all y values are on the curve.
        assert!(ok > 5 && refused > 5, "{ok} {refused}");
    }
}
