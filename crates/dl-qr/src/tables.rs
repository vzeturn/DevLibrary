//! Version and error correction tables from ISO/IEC 18004:2015, Table 9 and Annex E.

use crate::Ecc;

/// Smallest and largest QR Code Model 2 version.
pub(crate) const MIN_VERSION: u8 = 1;
pub(crate) const MAX_VERSION: u8 = 40;

/// Error correction codewords per block, indexed by `[ecc][version]` (index 0 unused).
const ECC_PER_BLOCK: [[u8; 41]; 4] = [
    // Low
    [
        0, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28,
        30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    // Medium
    [
        0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28,
        28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    ],
    // Quartile
    [
        0, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30,
        30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    // High
    [
        0, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
];

/// Number of error correction blocks (group 1 + group 2), indexed by `[ecc][version]`.
const NUM_BLOCKS: [[u8; 41]; 4] = [
    // Low
    [
        0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13,
        14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25,
    ],
    // Medium
    [
        0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21,
        23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
    ],
    // Quartile
    [
        0, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29,
        34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68,
    ],
    // High
    [
        0, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32,
        35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81,
    ],
];

/// Side length in modules of a symbol of `version`.
pub(crate) const fn size(version: u8) -> usize {
    version as usize * 4 + 17
}

/// Number of modules available for codewords (and remainder bits) after all function
/// patterns, format and version information are placed.
pub(crate) const fn raw_data_modules(version: u8) -> usize {
    let v = version as usize;
    let mut n = (16 * v + 128) * v + 64;
    if v >= 2 {
        let align = v / 7 + 2;
        n -= (25 * align - 10) * align - 55;
        if v >= 7 {
            n -= 36;
        }
    }
    n
}

/// Block structure of one version / ECC level.
///
/// The first `short_blocks` blocks carry `short_data` data codewords, the remaining
/// `blocks - short_blocks` carry `short_data + 1`. Every block has `ecc_len` ECC codewords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockLayout {
    pub(crate) blocks: usize,
    pub(crate) short_blocks: usize,
    pub(crate) short_data: usize,
    pub(crate) ecc_len: usize,
}

/// Block structure for `version` (1..=40) at level `ecc`.
pub(crate) fn block_layout(version: u8, ecc: Ecc) -> BlockLayout {
    let e = ecc as usize;
    let v = version as usize;
    let raw = raw_data_modules(version) / 8;
    let blocks = NUM_BLOCKS[e][v] as usize;
    let ecc_len = ECC_PER_BLOCK[e][v] as usize;
    BlockLayout {
        blocks,
        short_blocks: blocks - raw % blocks,
        short_data: raw / blocks - ecc_len,
        ecc_len,
    }
}

/// Number of data codewords (8-bit) for `version` at level `ecc`.
pub(crate) fn data_codewords(version: u8, ecc: Ecc) -> usize {
    let e = ecc as usize;
    let v = version as usize;
    raw_data_modules(version) / 8 - ECC_PER_BLOCK[e][v] as usize * NUM_BLOCKS[e][v] as usize
}

/// Number of data bits for `version` at level `ecc`.
pub(crate) fn data_bits(version: u8, ecc: Ecc) -> usize {
    data_codewords(version, ecc) * 8
}

/// Centre coordinates of the alignment patterns along one axis. Returns the positions
/// array and how many entries are used (0 for version 1, at most 7).
pub(crate) fn alignment_positions(version: u8) -> ([usize; 7], usize) {
    let mut out = [0usize; 7];
    if version == 1 {
        return (out, 0);
    }
    let v = version as usize;
    let count = v / 7 + 2;
    let step = (v * 8 + count * 3 + 5) / (count * 4 - 4) * 2;
    out[0] = 6;
    let mut pos = size(version) - 7;
    for i in (1..count).rev() {
        out[i] = pos;
        pos = pos.saturating_sub(step);
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const ECCS: [Ecc; 4] = [Ecc::Low, Ecc::Medium, Ecc::Quartile, Ecc::High];

    fn ecc_from_letter(s: &str) -> Ecc {
        match s {
            "L" => Ecc::Low,
            "M" => Ecc::Medium,
            "Q" => Ecc::Quartile,
            "H" => Ecc::High,
            _ => panic!("bad ecc {s}"),
        }
    }

    /// Compares every entry against the tables exported from segno by `tests/gen_vectors.py`.
    #[test]
    fn matches_segno_tables() {
        let text = include_str!("../tests/tables.txt");
        let (mut blocks_seen, mut align_seen) = (0, 0);
        for line in text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f[0] {
                // block <version> <ecc> <g1 blocks> <g1 total> <g1 data> [<g2 blocks> <g2 total> <g2 data>]
                "block" => {
                    let v: u8 = f[1].parse().unwrap();
                    let ecc = ecc_from_letter(f[2]);
                    let n: Vec<usize> = f[3..].iter().map(|x| x.parse().unwrap()).collect();
                    let l = block_layout(v, ecc);
                    assert_eq!(l.short_blocks, n[0], "v{v} {ecc:?}");
                    assert_eq!(l.short_data + l.ecc_len, n[1], "v{v} {ecc:?}");
                    assert_eq!(l.short_data, n[2], "v{v} {ecc:?}");
                    if n.len() > 3 {
                        assert_eq!(l.blocks - l.short_blocks, n[3], "v{v} {ecc:?}");
                        assert_eq!(l.short_data + 1 + l.ecc_len, n[4], "v{v} {ecc:?}");
                        assert_eq!(l.short_data + 1, n[5], "v{v} {ecc:?}");
                    } else {
                        assert_eq!(l.blocks, l.short_blocks, "v{v} {ecc:?}");
                    }
                    let data: usize = n.chunks(3).map(|g| g[0] * g[2]).sum();
                    assert_eq!(data_codewords(v, ecc), data);
                    blocks_seen += 1;
                }
                // align <version> <positions...>
                "align" => {
                    let v: u8 = f[1].parse().unwrap();
                    let expect: Vec<usize> = f[2..].iter().map(|x| x.parse().unwrap()).collect();
                    let (pos, n) = alignment_positions(v);
                    assert_eq!(&pos[..n], &expect[..], "v{v}");
                    align_seen += 1;
                }
                other => panic!("unknown record {other}"),
            }
        }
        assert_eq!(blocks_seen, 160);
        assert_eq!(align_seen, 40);
    }

    #[test]
    fn spot_checks() {
        // ISO/IEC 18004 Table 9: total codewords per version.
        assert_eq!(raw_data_modules(1) / 8, 26);
        assert_eq!(raw_data_modules(7) / 8, 196);
        assert_eq!(raw_data_modules(40) / 8, 3706);
        // Remainder bits: 0 for v1, 7 for v2..=6, 0 for v7..=13.
        assert_eq!(raw_data_modules(1) % 8, 0);
        assert_eq!(raw_data_modules(2) % 8, 7);
        assert_eq!(raw_data_modules(7) % 8, 0);
        assert_eq!(raw_data_modules(14) % 8, 3);
        assert_eq!(data_codewords(1, Ecc::Low), 19);
        assert_eq!(data_codewords(1, Ecc::High), 9);
        assert_eq!(data_codewords(40, Ecc::Low), 2956);
        assert_eq!(data_codewords(40, Ecc::High), 1276);
        for v in MIN_VERSION..=MAX_VERSION {
            for e in ECCS {
                let l = block_layout(v, e);
                let total = l.blocks * l.ecc_len
                    + l.short_blocks * l.short_data
                    + (l.blocks - l.short_blocks) * (l.short_data + 1);
                assert_eq!(total, raw_data_modules(v) / 8);
            }
        }
    }
}
