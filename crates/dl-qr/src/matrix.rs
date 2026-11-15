//! Module placement, data masking and format / version information
//! (ISO/IEC 18004 sections 6.3, 7.7 - 7.10).

use crate::Ecc;
use crate::tables::{alignment_positions, size};
use alloc::vec::Vec;

/// Penalty weights N1..N4 (ISO/IEC 18004 7.8.3.1, Table 11).
const N1: u32 = 3;
const N2: u32 = 3;
const N3: u32 = 40;
const N4: u32 = 10;

/// A symbol under construction. Coordinates are `(x, y)` = (column, row), origin top left.
pub(crate) struct Grid {
    size: usize,
    /// `true` = dark.
    modules: Vec<bool>,
    /// `true` for function patterns and reserved areas (not data, not masked).
    function: Vec<bool>,
}

/// 15-bit format information: 2 ECC bits, 3 mask bits, BCH(15,5) remainder, XOR 0x5412.
pub(crate) fn format_bits(ecc: Ecc, mask: u8) -> u32 {
    let level = match ecc {
        Ecc::Low => 0b01,
        Ecc::Medium => 0b00,
        Ecc::Quartile => 0b11,
        Ecc::High => 0b10,
    };
    let data = (level << 3) | mask as u32;
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | (rem & 0x3FF)) ^ 0x5412
}

/// 18-bit version information: 6 version bits and BCH(18,6) remainder. Versions >= 7 only.
pub(crate) fn version_bits(version: u8) -> u32 {
    let data = version as u32;
    let mut rem = data;
    for _ in 0..12 {
        rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
    }
    (data << 12) | (rem & 0xFFF)
}

/// Whether mask pattern `mask` (0..=7) inverts the module at column `x`, row `y`.
fn mask_bit(mask: u8, x: usize, y: usize) -> bool {
    match mask {
        0 => (x + y).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (x + y).is_multiple_of(3),
        4 => (x / 3 + y / 2).is_multiple_of(2),
        5 => (x * y) % 2 + (x * y) % 3 == 0,
        6 => ((x * y) % 2 + (x * y) % 3).is_multiple_of(2),
        _ => ((x + y) % 2 + (x * y) % 3).is_multiple_of(2),
    }
}

impl Grid {
    /// Empty symbol with all function patterns drawn and format / version areas reserved.
    pub(crate) fn new(version: u8) -> Self {
        let n = size(version);
        let mut g = Grid {
            size: n,
            modules: alloc::vec![false; n * n],
            function: alloc::vec![false; n * n],
        };
        // Timing patterns (row 6 and column 6). Finders overwrite their ends.
        for i in 0..n {
            g.set_function(6, i, i % 2 == 0);
            g.set_function(i, 6, i % 2 == 0);
        }
        // Finder patterns with separators.
        g.draw_finder(3, 3);
        g.draw_finder(n - 4, 3);
        g.draw_finder(3, n - 4);
        // Alignment patterns, except where they would overlap a finder.
        let (pos, count) = alignment_positions(version);
        for i in 0..count {
            for j in 0..count {
                let corner =
                    (i == 0 && j == 0) || (i == 0 && j == count - 1) || (i == count - 1 && j == 0);
                if !corner {
                    g.draw_alignment(pos[i], pos[j]);
                }
            }
        }
        // Reserve format areas (real bits are drawn after masking) and draw version info.
        g.draw_format(Ecc::Low, 0);
        g.draw_version(version);
        g
    }

    pub(crate) fn into_modules(self) -> Vec<bool> {
        self.modules
    }

    fn set_function(&mut self, x: usize, y: usize, dark: bool) {
        let i = y * self.size + x;
        self.modules[i] = dark;
        self.function[i] = true;
    }

    /// 7x7 finder centred at (x, y) plus the 1-module light separator, clipped to the symbol.
    fn draw_finder(&mut self, x: usize, y: usize) {
        for dy in -4isize..=4 {
            for dx in -4isize..=4 {
                let (xx, yy) = (x as isize + dx, y as isize + dy);
                if xx < 0 || yy < 0 || xx >= self.size as isize || yy >= self.size as isize {
                    continue;
                }
                let dist = dx.abs().max(dy.abs());
                self.set_function(xx as usize, yy as usize, dist != 2 && dist != 4);
            }
        }
    }

    /// 5x5 alignment pattern centred at (x, y).
    fn draw_alignment(&mut self, x: usize, y: usize) {
        for dy in 0..5 {
            for dx in 0..5 {
                let dist = (dx as isize - 2).abs().max((dy as isize - 2).abs());
                self.set_function(x + dx - 2, y + dy - 2, dist != 1);
            }
        }
    }

    /// Both copies of the format information, plus the dark module.
    pub(crate) fn draw_format(&mut self, ecc: Ecc, mask: u8) {
        let bits = format_bits(ecc, mask);
        let bit = |i: usize| (bits >> i) & 1 != 0;
        let n = self.size;
        // Copy 1, around the top-left finder.
        for i in 0..6 {
            self.set_function(8, i, bit(i));
        }
        self.set_function(8, 7, bit(6));
        self.set_function(8, 8, bit(7));
        self.set_function(7, 8, bit(8));
        for i in 9..15 {
            self.set_function(14 - i, 8, bit(i));
        }
        // Copy 2, split between the top-right and bottom-left finders.
        for i in 0..8 {
            self.set_function(n - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set_function(8, n - 15 + i, bit(i));
        }
        self.set_function(8, n - 8, true);
    }

    /// Both copies of the version information (versions 7..=40).
    fn draw_version(&mut self, version: u8) {
        if version < 7 {
            return;
        }
        let bits = version_bits(version);
        let n = self.size;
        for i in 0..18 {
            let dark = (bits >> i) & 1 != 0;
            let (a, b) = (n - 11 + i % 3, i / 3);
            self.set_function(a, b, dark);
            self.set_function(b, a, dark);
        }
    }

    /// Places the final codeword sequence in the two-column zig-zag order of 7.7.3.
    /// Modules left over after the last bit (remainder bits) stay light.
    pub(crate) fn draw_codewords(&mut self, data: &[u8]) {
        let n = self.size;
        let total = data.len() * 8;
        let mut i = 0;
        let mut right = n - 1;
        loop {
            if right == 6 {
                // Skip the vertical timing pattern.
                right = 5;
            }
            let upward = (right + 1) & 2 == 0;
            for vert in 0..n {
                let y = if upward { n - 1 - vert } else { vert };
                for x in [right, right - 1] {
                    let idx = y * n + x;
                    if !self.function[idx] && i < total {
                        self.modules[idx] = (data[i >> 3] >> (7 - (i & 7))) & 1 != 0;
                        i += 1;
                    }
                }
            }
            if right < 2 {
                break;
            }
            right -= 2;
        }
    }

    /// XORs mask `mask` onto every data module. Applying the same mask twice undoes it.
    pub(crate) fn apply_mask(&mut self, mask: u8) {
        let mut out = Vec::with_capacity(self.modules.len());
        self.masked_into(mask, &mut out);
        self.modules = out;
    }

    /// Replaces `out` with the modules after XORing mask `mask` onto the data modules.
    fn masked_into(&self, mask: u8, out: &mut Vec<bool>) {
        out.clear();
        // One monomorphised loop per pattern, so the pattern test is inlined.
        match mask {
            0 => self.extend_masked(out, |x, y| mask_bit(0, x, y)),
            1 => self.extend_masked(out, |x, y| mask_bit(1, x, y)),
            2 => self.extend_masked(out, |x, y| mask_bit(2, x, y)),
            3 => self.extend_masked(out, |x, y| mask_bit(3, x, y)),
            4 => self.extend_masked(out, |x, y| mask_bit(4, x, y)),
            5 => self.extend_masked(out, |x, y| mask_bit(5, x, y)),
            6 => self.extend_masked(out, |x, y| mask_bit(6, x, y)),
            _ => self.extend_masked(out, |x, y| mask_bit(7, x, y)),
        }
    }

    fn extend_masked(&self, out: &mut Vec<bool>, invert: impl Fn(usize, usize) -> bool) {
        let n = self.size;
        let rows = self
            .modules
            .chunks_exact(n)
            .zip(self.function.chunks_exact(n));
        for (y, (row, function)) in rows.enumerate() {
            let cells = row.iter().zip(function).enumerate();
            out.extend(cells.map(|(x, (&dark, &f))| dark ^ (!f && invert(x, y))));
        }
    }

    /// Tries all eight masks (with their format information drawn) and returns the one
    /// with the lowest penalty; ties go to the lowest mask number. Leaves the data modules
    /// unmasked.
    pub(crate) fn best_mask(&mut self, ecc: Ecc) -> u8 {
        let mut best = (u32::MAX, 0u8);
        let mut scratch = Vec::with_capacity(self.modules.len());
        for mask in 0..8 {
            self.draw_format(ecc, mask);
            self.masked_into(mask, &mut scratch);
            let p = penalty(&scratch, self.size).iter().sum();
            if p < best.0 {
                best = (p, mask);
            }
        }
        best.1
    }
}

/// Penalty scores `[N1 rule, N2 rule, N3 rule, N4 rule]` of a square `modules` grid of side
/// `n` (ISO/IEC 18004 7.8.3.1). Modules outside the symbol count as light for rule 3.
pub(crate) fn penalty(modules: &[bool], n: usize) -> [u32; 4] {
    let mut p = [0u32; 4];
    for row in modules.chunks_exact(n) {
        line_penalty(row.iter().copied(), &mut p);
    }
    for x in 0..n {
        line_penalty(modules[x..].iter().step_by(n).copied(), &mut p);
    }
    // Rule 2: each 2x2 block of one colour.
    for (top, bottom) in modules.chunks_exact(n).zip(modules.chunks_exact(n).skip(1)) {
        for x in 0..n - 1 {
            let c = top[x];
            if c == top[x + 1] && c == bottom[x] && c == bottom[x + 1] {
                p[1] += N2;
            }
        }
    }
    // Rule 4: k = floor(|dark% - 50%| / 5%), computed exactly in integers.
    let total = n * n;
    let dark = modules.iter().filter(|&&d| d).count();
    if let Some(k) = (dark * 20).abs_diff(total * 10).checked_div(total) {
        p[3] = k as u32 * N4;
    }
    p
}

/// Rule 3 core dark-light-dark-dark-dark-light-dark, oldest module in the highest bit.
const FINDER_CORE: u32 = 0b101_1101;

/// Whether the window (last 15 modules, newest in bit 0) ends with a rule 3 pattern that
/// has not been counted yet.
fn finder_like(window: u32) -> bool {
    let w11 = window & 0x7FF;
    // 0000 1011101: the core preceded by 4 light modules.
    let light_before = w11 == FINDER_CORE;
    // 1011101 0000: the core followed by 4 light modules, unless the 4 modules before it
    // were light too (0000 1011101 0000), which was counted 4 modules earlier.
    let light_after = w11 == (FINDER_CORE << 4) && (window & 0x7FFF) != (FINDER_CORE << 4);
    light_before || light_after
}

/// Rules 1 and 3 on one row or column, in a single pass.
///
/// Rule 3 counts every occurrence of the 1:1:3:1:1 core that has 4 light modules on at
/// least one side, once, with modules outside the symbol counted as light.
fn line_penalty(line: impl Iterator<Item = bool>, p: &mut [u32; 4]) {
    let mut run = 0u32;
    let mut prev = false;
    // Starting at 0 makes the modules before the line light.
    let mut window = 0u32;
    for dark in line {
        // Rule 1: runs of 5 + i same-coloured modules score N1 + i.
        if dark == prev {
            run += 1;
        } else {
            if run >= 5 {
                p[0] += N1 + run - 5;
            }
            run = 1;
            prev = dark;
        }
        window = (window << 1 | u32::from(dark)) & 0x7FFF;
        if finder_like(window) {
            p[2] += N3;
        }
    }
    if run >= 5 {
        p[0] += N1 + run - 5;
    }
    // The 4 light modules after the line.
    for _ in 0..4 {
        window = (window << 1) & 0x7FFF;
        if finder_like(window) {
            p[2] += N3;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_information() {
        // ISO/IEC 18004 Table C.1 / thonky.com format strings.
        assert_eq!(format_bits(Ecc::Medium, 0), 0b101010000010010);
        assert_eq!(format_bits(Ecc::Low, 0), 0b111011111000100);
        assert_eq!(format_bits(Ecc::Quartile, 0), 0b011010101011111);
        assert_eq!(format_bits(Ecc::High, 0), 0b001011010001001);
        assert_eq!(format_bits(Ecc::Low, 4), 0b110011000101111);
        assert_eq!(format_bits(Ecc::High, 7), 0b000100000111011);
        // ISO/IEC 18004 Annex C example: M, mask 101 -> 100000011001110.
        assert_eq!(format_bits(Ecc::Medium, 5), 0b100000011001110);
    }

    #[test]
    fn version_information() {
        // ISO/IEC 18004 Table D.1.
        assert_eq!(version_bits(7), 0b000111110010010100);
        assert_eq!(version_bits(8), 0b001000010110111100);
        assert_eq!(version_bits(21), 0b010101011010000011);
        assert_eq!(version_bits(40), 0b101000110001101001);
    }

    #[test]
    fn function_patterns_v1() {
        let g = Grid::new(1);
        let n = g.size;
        assert_eq!(n, 21);
        let dark = |x: usize, y: usize| g.modules[y * n + x];
        // Finder corners, centre and separator.
        assert!(dark(0, 0) && dark(6, 6) && dark(3, 3) && !dark(1, 1) && !dark(7, 7));
        assert!(dark(20, 0) && dark(0, 20) && !dark(13, 0));
        // Timing pattern alternates and starts dark.
        assert!(dark(8, 6) && !dark(9, 6) && dark(10, 6));
        assert!(dark(6, 8) && !dark(6, 9));
        // Dark module.
        assert!(dark(8, 13));
        // Data area: 26 codewords * 8 = 208 modules.
        assert_eq!(g.function.iter().filter(|&&f| !f).count(), 208);
    }

    #[test]
    fn data_module_counts() {
        for v in 1..=40 {
            let g = Grid::new(v);
            let free = g.function.iter().filter(|&&f| !f).count();
            assert_eq!(free, crate::tables::raw_data_modules(v), "v{v}");
        }
    }

    /// Rule 3 with segno's search order: after a counted pattern at `i` the search resumes
    /// at `i + 7`, after an uncounted one at `i + 4`.
    fn rule3_segno_style(line: &[bool]) -> u32 {
        const CORE: [bool; 7] = [true, false, true, true, true, false, true];
        let n = line.len();
        let mut score = 0;
        let mut i = 0;
        while i + 7 <= n {
            if line[i..i + 7] != CORE {
                i += 1;
                continue;
            }
            let before = line[i.saturating_sub(4)..i].iter().all(|&d| !d);
            let after = line[i + 7..(i + 11).min(n)].iter().all(|&d| !d);
            if before || after {
                score += N3;
                i += 7;
            } else {
                i += 4;
            }
        }
        score
    }

    #[test]
    fn rule3_overlapping_patterns() {
        // Column 4 of segno vector "byte-7l": patterns at 0 and 6 share a module; the second
        // is followed by 4 light modules and counts.
        let line: std::vec::Vec<bool> = "1011101011101000010".chars().map(|c| c == '1').collect();
        let mut p = [0; 4];
        line_penalty(line.iter().copied(), &mut p);
        assert_eq!(p[2], 80);
        assert_eq!(rule3_segno_style(&line), 40);
    }

    #[test]
    fn rule3_counts_each_occurrence() {
        let mut p = [0; 4];
        // Pattern at the start: outside the symbol counts as light.
        let line: std::vec::Vec<bool> = "1011101000010".chars().map(|c| c == '1').collect();
        line_penalty(line.iter().copied(), &mut p);
        assert_eq!(p[2], 40);
        let mut p = [0; 4];
        // Light on neither side.
        let line: std::vec::Vec<bool> = "110111010011".chars().map(|c| c == '1').collect();
        line_penalty(line.iter().copied(), &mut p);
        assert_eq!(p[2], 0);
    }

    /// Scores every reference matrix from segno (`tests/vectors.txt`) and compares each of
    /// the four rules with segno's `mask_scores`.
    #[test]
    fn penalty_matches_segno() {
        let text = include_str!("../tests/vectors.txt");
        let mut lines = text.lines();
        let (mut name, mut expect, mut checked) = ("", [0u32; 4], 0);
        let mut mismatches = std::vec::Vec::new();
        while let Some(line) = lines.next() {
            if let Some(n) = line.strip_prefix("case ") {
                name = n;
            } else if let Some(p) = line.strip_prefix("penalty ") {
                let v: std::vec::Vec<u32> = p.split(' ').map(|x| x.parse().unwrap()).collect();
                expect = [v[0], v[1], v[2], v[3]];
            } else if let Some(r) = line.strip_prefix("rows ") {
                let n: usize = r.parse().unwrap();
                let mut modules = std::vec::Vec::with_capacity(n * n);
                for _ in 0..n {
                    let row = lines.next().unwrap();
                    let bits = row.chars().flat_map(|c| {
                        let v = c.to_digit(16).unwrap();
                        (0..4).rev().map(move |i| (v >> i) & 1 == 1)
                    });
                    modules.extend(bits.take(n));
                }
                let got = penalty(&modules, n);
                // Rules 1, 2 and 4 must agree exactly. For rule 3, segno resumes its search
                // 7 modules after a counted pattern and so misses a second pattern that
                // overlaps it (e.g. 1011101011101 0000). ISO/IEC 18004 counts every
                // occurrence, as ZXing and Nayuki's library do. Emulating segno's search on
                // top of the same pattern test must reproduce its score exactly.
                let segno_n3: u32 = (0..n)
                    .map(|i| {
                        let row = &modules[i * n..(i + 1) * n];
                        let col: std::vec::Vec<bool> = (0..n).map(|y| modules[y * n + i]).collect();
                        rule3_segno_style(row) + rule3_segno_style(&col)
                    })
                    .sum();
                if [got[0], got[1], segno_n3, got[3]] != expect || got[2] < segno_n3 {
                    mismatches.push(std::format!("{name}: {got:?} != segno {expect:?}"));
                }
                checked += 1;
            }
        }
        assert!(checked >= 25);
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// Direct transcription of rules 1 and 3: scan every position, compare slices.
    fn line_penalty_reference(line: &[bool]) -> [u32; 4] {
        let mut p = [0; 4];
        let n = line.len();
        let mut start = 0;
        for i in 1..=n {
            if i == n || line[i] != line[start] {
                if i - start >= 5 {
                    p[0] += N1 + (i - start - 5) as u32;
                }
                start = i;
            }
        }
        const CORE: [bool; 7] = [true, false, true, true, true, false, true];
        for i in 0..n.saturating_sub(6) {
            if line[i..i + 7] == CORE {
                let before = line[i.saturating_sub(4)..i].iter().all(|&d| !d);
                let after = line[i + 7..(i + 11).min(n)].iter().all(|&d| !d);
                if before || after {
                    p[2] += N3;
                }
            }
        }
        p
    }

    #[test]
    fn line_penalty_matches_reference() {
        // xorshift64; lines biased towards finder-like content.
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let pieces: [&[bool]; 4] = [
            &[true, false, true, true, true, false, true],
            &[false, false, false, false],
            &[true],
            &[false],
        ];
        for _ in 0..20_000 {
            let len = (next() % 60) as usize;
            let mut line = std::vec::Vec::new();
            while line.len() < len {
                line.extend_from_slice(pieces[(next() % 4) as usize]);
            }
            line.truncate(len);
            let mut p = [0; 4];
            line_penalty(line.iter().copied(), &mut p);
            assert_eq!(p, line_penalty_reference(&line), "{line:?}");
        }
    }

    #[test]
    fn rule1_runs() {
        let mut p = [0; 4];
        let line: std::vec::Vec<bool> = "11111011111100000001".chars().map(|c| c == '1').collect();
        line_penalty(line.iter().copied(), &mut p);
        // 5 dark -> 3, 6 dark -> 4, 7 light -> 5.
        assert_eq!(p[0], 12);
    }
}
