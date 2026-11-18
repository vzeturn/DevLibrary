//! Integers modulo the group order L = 2^252 + 27742317777372353535851937790883648493.
//!
//! Reduction is bit-serial with a branch-free conditional subtraction, so it runs in the
//! same time for every input (the nonce `r` and the secret scalar pass through it).

/// L as little-endian 64-bit limbs.
const L: [u64; 4] = [
    0x5812_631a_5cf5_d3ed,
    0x14de_f9de_a2f7_9cd6,
    0,
    0x1000_0000_0000_0000,
];

pub(crate) fn from_bytes(b: &[u8; 32]) -> [u64; 4] {
    let mut l = [0u64; 4];
    for (i, limb) in l.iter_mut().enumerate() {
        let mut w = [0u8; 8];
        w.copy_from_slice(&b[i * 8..i * 8 + 8]);
        *limb = u64::from_le_bytes(w);
    }
    l
}

pub(crate) fn to_bytes(l: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, limb) in l.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    out
}

/// Whether the little-endian 32-byte value is below L (variable time; public data only).
pub(crate) fn is_canonical(b: &[u8; 32]) -> bool {
    let x = from_bytes(b);
    for i in (0..4).rev() {
        if x[i] != L[i] {
            return x[i] < L[i];
        }
    }
    false
}

/// `r - L` when `r >= L`, else `r`, without branching. Requires `r < 2L`.
fn cond_sub_l(r: [u64; 5]) -> [u64; 5] {
    let mut t = [0u64; 5];
    let mut borrow = 0u64;
    for i in 0..5 {
        let li = if i < 4 { L[i] } else { 0 };
        let (d1, b1) = r[i].overflowing_sub(li);
        let (d2, b2) = d1.overflowing_sub(borrow);
        t[i] = d2;
        borrow = u64::from(b1 | b2);
    }
    // borrow == 1 means r < L: keep r.
    let keep = 0u64.wrapping_sub(borrow);
    let mut out = [0u64; 5];
    for i in 0..5 {
        out[i] = (r[i] & keep) | (t[i] & !keep);
    }
    out
}

/// Reduces a little-endian integer of `limbs.len() * 64` bits modulo L.
fn reduce(limbs: &[u64]) -> [u64; 4] {
    let mut r = [0u64; 5];
    for i in (0..limbs.len() * 64).rev() {
        let bit = (limbs[i / 64] >> (i % 64)) & 1;
        // r = 2r + bit; r < L < 2^253, so it fits.
        let mut carry = bit;
        for limb in r.iter_mut() {
            let next = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        r = cond_sub_l(r);
    }
    [r[0], r[1], r[2], r[3]]
}

/// A 64-byte hash output modulo L.
pub(crate) fn reduce_wide(h: &[u8; 64]) -> [u64; 4] {
    let mut l = [0u64; 8];
    for (i, limb) in l.iter_mut().enumerate() {
        let mut w = [0u8; 8];
        w.copy_from_slice(&h[i * 8..i * 8 + 8]);
        *limb = u64::from_le_bytes(w);
    }
    reduce(&l)
}

/// `(k * a + r) mod L` for 256-bit inputs.
pub(crate) fn mul_add(k: &[u64; 4], a: &[u64; 4], r: &[u64; 4]) -> [u64; 4] {
    let mut p = [0u64; 9];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let t = u128::from(k[i]) * u128::from(a[j]) + u128::from(p[i + j]) + carry;
            p[i + j] = t as u64;
            carry = t >> 64;
        }
        p[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for (i, limb) in p.iter_mut().enumerate() {
        let add = if i < 4 { r[i] } else { 0 };
        let t = u128::from(*limb) + u128::from(add) + carry;
        *limb = t as u64;
        carry = t >> 64;
    }
    reduce(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduction() {
        // L reduces to 0, L + 5 to 5, 2^512 - 1 to (2^512 - 1) mod L.
        let mut l = [0u64; 8];
        l[..4].copy_from_slice(&L);
        assert_eq!(reduce(&l), [0; 4]);
        l[0] += 5;
        assert_eq!(reduce(&l), [5, 0, 0, 0]);
        assert!(is_canonical(&to_bytes(&[L[0] - 1, L[1], L[2], L[3]])));
        assert!(!is_canonical(&to_bytes(&L)));
        // (L - 1) * (L - 1) + 0 == 1 mod L.
        let lm1 = [L[0] - 1, L[1], L[2], L[3]];
        assert_eq!(mul_add(&lm1, &lm1, &[0; 4]), [1, 0, 0, 0]);
        // 2 * 3 + 4 == 10.
        assert_eq!(
            mul_add(&[2, 0, 0, 0], &[3, 0, 0, 0], &[4, 0, 0, 0]),
            [10, 0, 0, 0]
        );
    }
}
