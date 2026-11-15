//! Reed-Solomon error correction over GF(256) with the QR field polynomial
//! x^8 + x^4 + x^3 + x^2 + 1 (0x11D), generator element alpha = 2.

use crate::Ecc;
use crate::tables::block_layout;
use alloc::vec::Vec;

/// Largest number of ECC codewords per block used by any version / level.
const MAX_ECC_LEN: usize = 30;

const fn build_tables() -> ([u8; 256], [u8; 256]) {
    let mut exp = [0u8; 256];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11D;
        }
        i += 1;
    }
    exp[255] = exp[0];
    (exp, log)
}

const TABLES: ([u8; 256], [u8; 256]) = build_tables();
/// `EXP[i]` = alpha^i.
const EXP: [u8; 256] = TABLES.0;
/// `LOG[x]` = i such that alpha^i = x (undefined for 0).
const LOG: [u8; 256] = TABLES.1;

/// Multiplication in GF(256).
pub(crate) fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        EXP[(LOG[a as usize] as usize + LOG[b as usize] as usize) % 255]
    }
}

/// Generator polynomial prod_{i=0}^{degree-1} (x - alpha^i), monic.
/// Returns the `degree` coefficients after the leading 1, highest power first.
pub(crate) fn generator(degree: usize) -> Vec<u8> {
    let mut poly = alloc::vec![0u8; degree];
    if degree == 0 {
        return poly;
    }
    // Start with the constant polynomial 1 (stored in the last slot).
    poly[degree - 1] = 1;
    let mut root = 1u8;
    for _ in 0..degree {
        // Multiply by (x - root): shift left, then add root * poly.
        for j in 0..degree {
            poly[j] = mul(poly[j], root);
            if j + 1 < degree {
                poly[j] ^= poly[j + 1];
            }
        }
        root = mul(root, 2);
    }
    poly
}

/// Remainder of `data(x) * x^n` divided by the generator, where `n = gen.len()`.
/// Writes the `n` ECC codewords into `out[..n]`.
fn remainder(data: &[u8], gen_poly: &[u8], out: &mut [u8]) {
    let n = gen_poly.len();
    let out = &mut out[..n];
    out.fill(0);
    for &b in data {
        let factor = b ^ out[0];
        out.copy_within(1.., 0);
        out[n - 1] = 0;
        for (o, &g) in out.iter_mut().zip(gen_poly) {
            *o ^= mul(g, factor);
        }
    }
}

/// Splits `data` (exactly `data_codewords(version, ecc)` bytes) into blocks, appends
/// Reed-Solomon ECC to each block, and interleaves them into the final codeword sequence.
pub(crate) fn encode_blocks(data: &[u8], version: u8, ecc: Ecc) -> Vec<u8> {
    let l = block_layout(version, ecc);
    let gen_poly = generator(l.ecc_len);
    let long_data = l.short_data + 1;
    let total = data.len() + l.blocks * l.ecc_len;
    let mut ecc_bytes = alloc::vec![0u8; l.blocks * l.ecc_len];
    let mut starts = Vec::with_capacity(l.blocks);

    let mut offset = 0;
    for b in 0..l.blocks {
        let len = if b < l.short_blocks {
            l.short_data
        } else {
            long_data
        };
        let block = &data[offset..offset + len];
        let mut rem = [0u8; MAX_ECC_LEN];
        remainder(block, &gen_poly, &mut rem);
        ecc_bytes[b * l.ecc_len..(b + 1) * l.ecc_len].copy_from_slice(&rem[..l.ecc_len]);
        starts.push((offset, len));
        offset += len;
    }

    let mut out = Vec::with_capacity(total);
    for i in 0..long_data {
        for &(start, len) in &starts {
            if i < len {
                out.push(data[start + i]);
            }
        }
    }
    for i in 0..l.ecc_len {
        for b in 0..l.blocks {
            out.push(ecc_bytes[b * l.ecc_len + i]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::data_codewords;

    #[test]
    fn field_arithmetic() {
        // alpha^8 = x^4 + x^3 + x^2 + 1 = 0x1D.
        assert_eq!(EXP[8], 0x1D);
        assert_eq!(EXP[255], 1);
        assert_eq!(LOG[1], 0);
        assert_eq!(LOG[2], 1);
        assert_eq!(mul(0, 0x53), 0);
        assert_eq!(mul(1, 0x53), 0x53);
        assert_eq!(mul(2, 0x80), 0x1D);
        assert_eq!(mul(0x53, 0xCA), mul(0xCA, 0x53));
        // Every non-zero element has an inverse: x * alpha^(255 - log x) = 1.
        for x in 1..=255u8 {
            let inv = EXP[(255 - LOG[x as usize] as usize) % 255];
            assert_eq!(mul(x, inv), 1);
        }
        // EXP and LOG are inverse permutations of the 255 non-zero elements.
        let mut seen = [false; 256];
        for &e in &EXP[..255] {
            assert!(!seen[e as usize]);
            seen[e as usize] = true;
        }
        assert!(!seen[0]);
        // Distributivity spot check.
        for (a, b, c) in [(3u8, 7u8, 200u8), (0x8E, 0x47, 0x11), (255, 254, 1)] {
            assert_eq!(mul(a, b ^ c), mul(a, b) ^ mul(a, c));
        }
    }

    #[test]
    fn generator_polynomials() {
        // Degree 7 (version 1-L): x^7 + a^87 x^6 + a^229 x^5 + a^146 x^4 + a^149 x^3
        // + a^238 x^2 + a^102 x + a^21 (ISO/IEC 18004 Annex A, thonky.com).
        let g = generator(7);
        let exps: std::vec::Vec<u8> = g.iter().map(|&c| LOG[c as usize]).collect();
        assert_eq!(exps, [87, 229, 146, 149, 238, 102, 21]);
        assert_eq!(g, [127, 122, 154, 164, 11, 68, 117]);
        // Degree 10 (version 1-M).
        let g = generator(10);
        let exps: std::vec::Vec<u8> = g.iter().map(|&c| LOG[c as usize]).collect();
        assert_eq!(exps, [251, 67, 46, 61, 118, 70, 64, 94, 32, 45]);
    }

    #[test]
    fn hello_world_1q() {
        // "HELLO WORLD" in alphanumeric mode, version 1-Q (thonky.com QR tutorial). The ECC
        // bytes were recomputed with a from-scratch Python GF(256) division, and the whole
        // symbol is the segno vector "hello-world-1q" in tests/vectors.txt.
        let data = [32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236];
        assert_eq!(data_codewords(1, Ecc::Quartile), data.len());
        let out = encode_blocks(&data, 1, Ecc::Quartile);
        assert_eq!(&out[..13], &data);
        assert_eq!(
            &out[13..],
            &[168, 72, 22, 82, 217, 54, 156, 0, 46, 15, 180, 122, 16]
        );
    }

    #[test]
    fn hello_world_1m() {
        let data = [
            32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17,
        ];
        let out = encode_blocks(&data, 1, Ecc::Medium);
        assert_eq!(&out[16..], &[196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }

    #[test]
    fn interleaving_5q() {
        // Version 5-Q: 2 blocks of 15 data codewords, then 2 blocks of 16.
        let data: std::vec::Vec<u8> = (0..62).collect();
        let out = encode_blocks(&data, 5, Ecc::Quartile);
        assert_eq!(out.len(), 134);
        assert_eq!(&out[..8], &[0, 15, 30, 46, 1, 16, 31, 47]);
        // Last data column only exists in the two long blocks.
        assert_eq!(&out[58..62], &[44, 60, 45, 61]);
    }
}
