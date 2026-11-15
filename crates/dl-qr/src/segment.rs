//! Data segments (ISO/IEC 18004 section 7.4) and optimal mixed-mode segmentation.

use crate::Error;
use crate::bits::BitBuf;
use alloc::vec::Vec;

/// Encoding mode of a [`Segment`].
///
/// Non-exhaustive so that Kanji mode can be added later without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Mode {
    /// Digits `0`-`9`, 10 bits per 3 digits.
    Numeric,
    /// `0`-`9`, `A`-`Z` and ` $%*+-./:`, 11 bits per 2 characters.
    Alphanumeric,
    /// Arbitrary bytes, 8 bits each. Readers usually interpret them as UTF-8.
    Byte,
    /// Extended Channel Interpretation designator (e.g. 26 = UTF-8). Carries no characters.
    Eci,
}

impl Mode {
    /// 4-bit mode indicator.
    pub(crate) const fn indicator(self) -> u32 {
        match self {
            Mode::Numeric => 0b0001,
            Mode::Alphanumeric => 0b0010,
            Mode::Byte => 0b0100,
            Mode::Eci => 0b0111,
        }
    }

    /// Width of the character count indicator for `version`.
    pub(crate) const fn char_count_bits(self, version: u8) -> usize {
        let r = version_range(version);
        match self {
            Mode::Numeric => [10, 12, 14][r],
            Mode::Alphanumeric => [9, 11, 13][r],
            Mode::Byte => [8, 16, 16][r],
            Mode::Eci => 0,
        }
    }
}

/// Version range that determines character count widths: 0 = 1..=9, 1 = 10..=26, 2 = 27..=40.
pub(crate) const fn version_range(version: u8) -> usize {
    if version <= 9 {
        0
    } else if version <= 26 {
        1
    } else {
        2
    }
}

fn alphanumeric_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'Z' => Some(b - b'A' + 10),
        b' ' => Some(36),
        b'$' => Some(37),
        b'%' => Some(38),
        b'*' => Some(39),
        b'+' => Some(40),
        b'-' => Some(41),
        b'.' => Some(42),
        b'/' => Some(43),
        b':' => Some(44),
        _ => None,
    }
}

/// A run of data in a single mode, ready to be placed in a symbol.
///
/// [`QrCode::encode_text`](crate::QrCode::encode_text) builds segments automatically.
/// Build them by hand with [`QrBuilder::encode_segments`](crate::QrBuilder::encode_segments)
/// to force a mode or to add an ECI designator.
///
/// ```
/// use dl_qr::{Mode, Segment};
///
/// let s = Segment::numeric("0123456789")?;
/// assert_eq!((s.mode(), s.char_count()), (Mode::Numeric, 10));
/// assert!(Segment::alphanumeric("lower case").is_err());
/// # Ok::<(), dl_qr::Error>(())
/// ```
///
/// A segment stores its characters as given; the bit stream is produced when the symbol is
/// built, so even very large segments only cost their own size in memory.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Segment {
    mode: Mode,
    /// The characters (ASCII digits / alphanumeric bytes / raw bytes). Empty for ECI.
    data: Vec<u8>,
    /// ECI assignment number (ECI mode only).
    eci: u32,
}

impl Segment {
    /// Numeric segment. Fails with [`Error::InvalidCharacter`] on anything but `0`-`9`.
    pub fn numeric(digits: &str) -> Result<Self, Error> {
        let bytes = digits.as_bytes();
        if let Some(index) = bytes.iter().position(|b| !b.is_ascii_digit()) {
            return Err(Error::InvalidCharacter {
                index,
                mode: Mode::Numeric,
            });
        }
        Ok(Self::unchecked(Mode::Numeric, bytes))
    }

    /// Alphanumeric segment (`0`-`9`, `A`-`Z`, space and `$%*+-./:`).
    /// Fails with [`Error::InvalidCharacter`] on any other byte, including lowercase letters.
    pub fn alphanumeric(text: &str) -> Result<Self, Error> {
        let bytes = text.as_bytes();
        if let Some(index) = bytes.iter().position(|&b| alphanumeric_value(b).is_none()) {
            return Err(Error::InvalidCharacter {
                index,
                mode: Mode::Alphanumeric,
            });
        }
        Ok(Self::unchecked(Mode::Alphanumeric, bytes))
    }

    /// Byte segment holding `data` unchanged. Pass `text.as_bytes()` for UTF-8 text.
    pub fn bytes(data: &[u8]) -> Self {
        Self::unchecked(Mode::Byte, data)
    }

    /// ECI designator segment. `assignment` must be at most 999 999; 26 is UTF-8.
    /// It applies to the segments that follow it.
    pub fn eci(assignment: u32) -> Result<Self, Error> {
        if assignment >= 1_000_000 {
            return Err(Error::InvalidEci(assignment));
        }
        Ok(Self {
            mode: Mode::Eci,
            data: Vec::new(),
            eci: assignment,
        })
    }

    /// The encoding mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Number of characters (bytes for byte mode, 0 for ECI).
    pub fn char_count(&self) -> usize {
        self.data.len()
    }

    /// The characters of a numeric, alphanumeric or byte segment (empty for ECI).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Segment in `mode` (not ECI) without validating the characters. Characters that the
    /// mode cannot represent are encoded as value 0; callers only pass valid input.
    pub(crate) fn unchecked(mode: Mode, data: &[u8]) -> Self {
        Self {
            mode,
            data: data.to_vec(),
            eci: 0,
        }
    }

    /// Length of the data part (after mode indicator and character count) in bits.
    /// Saturates instead of overflowing on absurdly long segments.
    fn data_bits(&self) -> usize {
        let n = self.data.len();
        match self.mode {
            Mode::Numeric => (n / 3).saturating_mul(10).saturating_add([0, 4, 7][n % 3]),
            Mode::Alphanumeric => (n / 2).saturating_mul(11).saturating_add(n % 2 * 6),
            Mode::Byte => n.saturating_mul(8),
            Mode::Eci => match self.eci {
                0..0x80 => 8,
                0x80..0x4000 => 16,
                _ => 24,
            },
        }
    }

    /// Encoded length in bits at `version`, and whether the character count fits its field.
    pub(crate) fn bit_len(&self, version: u8) -> (usize, bool) {
        let cc = self.mode.char_count_bits(version);
        let fits = self.data.len() < 1 << cc;
        (self.data_bits().saturating_add(4 + cc), fits)
    }

    /// Appends mode indicator, character count and data to `out`. The character count
    /// must fit its field (checked by [`bit_len`](Self::bit_len)).
    pub(crate) fn write(&self, out: &mut BitBuf, version: u8) {
        out.push(self.mode.indicator(), 4);
        let cc = self.mode.char_count_bits(version);
        // `len < 2^cc <= 2^16` after the `bit_len` check, so the cast is lossless.
        out.push(self.data.len() as u32, cc);
        match self.mode {
            Mode::Numeric => {
                for chunk in self.data.chunks(3) {
                    let value = chunk.iter().fold(0u32, |acc, &d| {
                        acc * 10 + u32::from(d.wrapping_sub(b'0').min(9))
                    });
                    out.push(value, chunk.len() * 3 + 1);
                }
            }
            Mode::Alphanumeric => {
                let value = |b: u8| u32::from(alphanumeric_value(b).unwrap_or(0));
                for pair in self.data.chunks(2) {
                    match *pair {
                        [a, b] => out.push(value(a) * 45 + value(b), 11),
                        [a] => out.push(value(a), 6),
                        _ => {}
                    }
                }
            }
            Mode::Byte => {
                for &b in &self.data {
                    out.push(u32::from(b), 8);
                }
            }
            Mode::Eci => match self.eci {
                0..0x80 => out.push(self.eci, 8),
                0x80..0x4000 => out.push(0b10 << 14 | self.eci, 16),
                _ => out.push(0b110 << 21 | self.eci, 24),
            },
        }
    }
}

/// Total bit length of `segments` at `version`, and whether every character count fits.
pub(crate) fn total_bits(segments: &[Segment], version: u8) -> (usize, bool) {
    segments.iter().fold((0usize, true), |(sum, ok), s| {
        let (n, fits) = s.bit_len(version);
        (sum.saturating_add(n), ok && fits)
    })
}

// ---------------------------------------------------------------------------------------------
// Optimal segmentation.
//
// Dynamic programming over the input bytes, as in Nayuki's QR Code generator
// (QrSegmentAdvanced.makeSegmentsOptimally). Costs are kept in 1/6 bit units so that
// numeric (10/3 bits per digit) and alphanumeric (11/2 bits per char) are integers. A
// segment of n digits really costs ceil(10n/3) bits and n alphanumeric chars ceil(11n/2)
// bits, so rounding a segment up to a whole bit when it ends makes the model exact.
//
// Non-ASCII bytes are only encodable in byte mode, so a UTF-8 sequence is never split
// across segments.
// ---------------------------------------------------------------------------------------------

const BYTE: usize = 0;
const ALNUM: usize = 1;
const NUM: usize = 2;
const DP_MODES: [Mode; 3] = [Mode::Byte, Mode::Alphanumeric, Mode::Numeric];
const INF: u64 = u64::MAX / 4;

fn char_costs(b: u8) -> [u64; 3] {
    [
        48,
        if alphanumeric_value(b).is_some() {
            33
        } else {
            INF
        },
        if b.is_ascii_digit() { 20 } else { INF },
    ]
}

fn head_costs(version: u8) -> [u64; 3] {
    DP_MODES.map(|m| (4 + m.char_count_bits(version) as u64) * 6)
}

fn ceil6(x: u64) -> u64 {
    x.div_ceil(6) * 6
}

/// One DP step: the cost of ending byte `b` in each mode, and the mode the previous byte
/// ended in for that choice. `prev = None` for the first byte.
fn step(prev: Option<&[u64; 3]>, b: u8, head: &[u64; 3]) -> ([u64; 3], [u8; 3]) {
    let cc = char_costs(b);
    let mut cur = [INF; 3];
    let mut from = [0u8; 3];
    for m in 0..3 {
        if cc[m] >= INF {
            continue;
        }
        let (base, src) = match prev {
            None => (head[m], m),
            Some(p) => {
                // Continue the current segment, or close a segment in another mode and
                // open a new one. Ties keep the current segment (fewer segments).
                let mut best = (p[m], m);
                for (k, &pk) in p.iter().enumerate() {
                    if k != m && pk < INF {
                        let c = ceil6(pk) + head[m];
                        if c < best.0 {
                            best = (c, k);
                        }
                    }
                }
                best
            }
        };
        cur[m] = base + cc[m];
        from[m] = src as u8;
    }
    (cur, from)
}

/// Index of the cheapest final state (ties prefer byte, then alphanumeric).
fn best_end(costs: &[u64; 3]) -> usize {
    let mut best = BYTE;
    for m in [ALNUM, NUM] {
        if ceil6(costs[m]) < ceil6(costs[best]) {
            best = m;
        }
    }
    best
}

/// Bit length of the optimal segmentation of `text` at `version`. Runs in O(n) time and
/// O(1) memory, so it is safe to call on arbitrarily long input.
pub(crate) fn optimal_bits(text: &[u8], version: u8) -> usize {
    let head = head_costs(version);
    let mut costs: Option<[u64; 3]> = None;
    for &b in text {
        costs = Some(step(costs.as_ref(), b, &head).0);
    }
    match costs {
        None => 0,
        Some(c) => usize::try_from(ceil6(c[best_end(&c)]) / 6).unwrap_or(usize::MAX),
    }
}

/// The optimal segmentation of `text` at `version` (same for all versions in a range).
/// Returns an empty list for empty input.
pub(crate) fn optimal_segments(text: &[u8], version: u8) -> Vec<Segment> {
    if text.is_empty() {
        return Vec::new();
    }
    let head = head_costs(version);
    let mut back: Vec<[u8; 3]> = Vec::with_capacity(text.len());
    let mut costs: Option<[u64; 3]> = None;
    for &b in text {
        let (c, from) = step(costs.as_ref(), b, &head);
        back.push(from);
        costs = Some(c);
    }
    // `text` is non-empty, so `costs` was set at least once.
    let mut mode = costs.map_or(BYTE, |c| best_end(&c));
    let mut modes = alloc::vec![0u8; text.len()];
    for i in (0..text.len()).rev() {
        modes[i] = mode as u8;
        mode = back[i][mode] as usize;
    }

    let mut segments = Vec::new();
    let mut start = 0;
    for i in 1..=text.len() {
        if i == text.len() || modes[i] != modes[start] {
            let run = &text[start..i];
            segments.push(Segment::unchecked(DP_MODES[modes[start] as usize], run));
            start = i;
        }
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn modes_of(segs: &[Segment]) -> std::vec::Vec<(Mode, usize)> {
        segs.iter().map(|s| (s.mode(), s.char_count())).collect()
    }

    #[test]
    fn numeric_encoding() {
        // ISO/IEC 18004 7.4.3 example: "01234567" -> 0000001100 0101011001 1000011.
        let s = Segment::numeric("01234567").unwrap();
        assert_eq!(s.data_bits(), 27);
        assert_eq!(s.data(), b"01234567");
        let mut out = BitBuf::default();
        s.write(&mut out, 1);
        assert_eq!(out.len(), 4 + 10 + 27);
        // 0001 0000001000 0000001100 0101011001 1000011
        assert_eq!(out.into_bytes(), [0x10, 0x20, 0x0C, 0x56, 0x61, 0x80]);
        assert_eq!(
            Segment::numeric("12a"),
            Err(Error::InvalidCharacter {
                index: 2,
                mode: Mode::Numeric
            })
        );
    }

    #[test]
    fn alphanumeric_encoding() {
        // ISO/IEC 18004 7.4.4 example: "AC-42" -> 00111001110 11100111001 000010.
        let s = Segment::alphanumeric("AC-42").unwrap();
        let mut out = BitBuf::default();
        s.write(&mut out, 1);
        // 0010 000000101 00111001110 11100111001 000010
        assert_eq!(out.len(), 41);
        assert_eq!(out.into_bytes(), [0x20, 0x29, 0xCE, 0xE7, 0x21, 0x00]);
        // ISO/IEC 18004 Table 5, in code order.
        let charset = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";
        assert_eq!(charset.len(), 45);
        assert_eq!(
            (0..=255u8)
                .filter(|&b| alphanumeric_value(b).is_some())
                .count(),
            45
        );
        for (i, &c) in charset.iter().enumerate() {
            assert_eq!(alphanumeric_value(c), Some(i as u8));
        }
        assert_eq!(
            Segment::alphanumeric("ABc"),
            Err(Error::InvalidCharacter {
                index: 2,
                mode: Mode::Alphanumeric
            })
        );
    }

    #[test]
    fn eci_encoding() {
        let mut out = BitBuf::default();
        Segment::eci(26).unwrap().write(&mut out, 1);
        assert_eq!(out.into_bytes(), [0x71, 0xA0]);
        // ISO/IEC 18004 Table 4: 1, 2 or 3 designator bytes with prefix 0, 10, 110.
        for (n, expect) in [
            (127, std::vec![0x7F]),
            (128, std::vec![0x80, 0x80]),
            (1000, std::vec![0x83, 0xE8]),
            (16_383, std::vec![0xBF, 0xFF]),
            (16_384, std::vec![0xC0, 0x40, 0x00]),
            (999_999, std::vec![0xCF, 0x42, 0x3F]),
        ] {
            let s = Segment::eci(n).unwrap();
            let mut out = BitBuf::default();
            s.write(&mut out, 1);
            assert_eq!(out.len(), 4 + expect.len() * 8);
            assert_eq!(s.bit_len(1), (4 + expect.len() * 8, true));
            let bytes = out.into_bytes();
            // Drop the 4-bit mode indicator 0111.
            let shifted: std::vec::Vec<u8> = (0..expect.len())
                .map(|i| bytes[i] << 4 | bytes[i + 1] >> 4)
                .collect();
            assert_eq!(bytes[0] >> 4, 0b0111);
            assert_eq!(shifted, expect, "ECI {n}");
        }
        assert_eq!(Segment::eci(1_000_000), Err(Error::InvalidEci(1_000_000)));
    }

    #[test]
    fn char_count_overflow_detected() {
        let s = Segment::bytes(&[0u8; 256]);
        assert!(!total_bits(core::slice::from_ref(&s), 9).1);
        assert!(total_bits(core::slice::from_ref(&s), 10).1);
    }

    /// Minimum bit length over every possible assignment of modes to characters.
    fn brute_force_bits(text: &[u8], version: u8) -> usize {
        let options: std::vec::Vec<std::vec::Vec<usize>> = text
            .iter()
            .map(|&b| (0..3).filter(|&m| char_costs(b)[m] < INF).collect())
            .collect();
        let mut best = usize::MAX;
        let mut choice = vec![0usize; text.len()];
        loop {
            let modes: std::vec::Vec<usize> =
                choice.iter().zip(&options).map(|(&c, o)| o[c]).collect();
            let mut bits = 0;
            let mut start = 0;
            for i in 1..=text.len() {
                if i == text.len() || modes[i] != modes[start] {
                    let run = &text[start..i];
                    bits += Segment::unchecked(DP_MODES[modes[start]], run)
                        .bit_len(version)
                        .0;
                    start = i;
                }
            }
            best = best.min(bits);
            // Next assignment (odometer).
            let mut i = 0;
            loop {
                if i == choice.len() {
                    return best;
                }
                choice[i] += 1;
                if choice[i] < options[i].len() {
                    break;
                }
                choice[i] = 0;
                i += 1;
            }
        }
    }

    /// Small deterministic PRNG for test inputs (xorshift64*).
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    fn random_text(rng: &mut Rng, len: usize, alphabet: &[u8]) -> std::vec::Vec<u8> {
        (0..len)
            .map(|_| alphabet[(rng.next() % alphabet.len() as u64) as usize])
            .collect()
    }

    const MIXED: &[u8] = b"0123456789012345678901234ABCXYZ $%:/-abcz?&=\xC3";

    #[test]
    fn dp_matches_brute_force() {
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        for len in 1..=8 {
            for _ in 0..25 {
                let text = random_text(&mut rng, len, MIXED);
                for v in [1, 10, 27] {
                    let segs = optimal_segments(&text, v);
                    let (bits, ok) = total_bits(&segs, v);
                    assert!(ok);
                    assert_eq!(bits, optimal_bits(&text, v), "{text:?} v{v}");
                    assert_eq!(bits, brute_force_bits(&text, v), "{text:?} v{v}");
                }
            }
        }
    }

    #[test]
    fn never_longer_than_single_mode() {
        let mut rng = Rng(42);
        let alphabets: [&[u8]; 4] = [b"0123456789", b"0123456789ABC $", MIXED, b"abc012"];
        for alphabet in alphabets {
            for _ in 0..300 {
                let len = (rng.next() % 400) as usize;
                let text = random_text(&mut rng, len, alphabet);
                for v in [1, 9, 10, 26, 27, 40] {
                    let segs = optimal_segments(&text, v);
                    let (bits, _) = total_bits(&segs, v);
                    let single_byte = Segment::bytes(&text).bit_len(v).0;
                    assert!(bits <= single_byte);
                    if let Ok(s) = core::str::from_utf8(&text) {
                        if let Ok(seg) = Segment::alphanumeric(s) {
                            assert!(bits <= seg.bit_len(v).0);
                        }
                        if let Ok(seg) = Segment::numeric(s) {
                            assert!(bits <= seg.bit_len(v).0);
                        }
                    }
                    // Concatenated segment data reproduces the input.
                    let total: usize = segs.iter().map(Segment::char_count).sum();
                    assert_eq!(total, text.len());
                }
            }
        }
    }

    #[test]
    fn segmentation_examples() {
        assert!(optimal_segments(b"", 1).is_empty());
        assert_eq!(optimal_bits(b"", 1), 0);
        assert_eq!(
            modes_of(&optimal_segments(b"0123456789", 1)),
            [(Mode::Numeric, 10)]
        );
        assert_eq!(
            modes_of(&optimal_segments(b"HELLO WORLD", 1)),
            [(Mode::Alphanumeric, 11)]
        );
        assert_eq!(modes_of(&optimal_segments(b"hello", 1)), [(Mode::Byte, 5)]);
        // Short digit runs inside text are not worth a mode switch.
        assert_eq!(
            modes_of(&optimal_segments(b"abc123def", 1)),
            [(Mode::Byte, 9)]
        );
        // A long digit run is.
        assert_eq!(
            modes_of(&optimal_segments(
                b"https://x.vn/c?id=000201010212263800",
                1
            )),
            [(Mode::Byte, 18), (Mode::Numeric, 18)]
        );
        assert_eq!(
            modes_of(&optimal_segments(b"INVOICE 2024 000123456789 ok", 1)),
            [
                (Mode::Alphanumeric, 13),
                (Mode::Numeric, 12),
                (Mode::Byte, 3)
            ]
        );
        // VietQR payload: the EMVCo TLV fields mix digit runs, uppercase IDs and free text.
        let vietqr = b"00020101021238570010A00000072701270006970436011300110123456780208QRIBFTTA\
                       5303704540715000005802VN62350831Thanh toan hoa don HD2024000123630446AF";
        assert_eq!(
            modes_of(&optimal_segments(vietqr, 1)),
            [
                (Mode::Numeric, 20),
                (Mode::Alphanumeric, 1),
                (Mode::Numeric, 44),
                (Mode::Alphanumeric, 8),
                (Mode::Numeric, 22),
                (Mode::Alphanumeric, 2),
                (Mode::Numeric, 8),
                (Mode::Byte, 21),
                (Mode::Numeric, 16),
                (Mode::Alphanumeric, 2),
            ]
        );
        let (mixed, _) = total_bits(&optimal_segments(vietqr, 1), 1);
        assert_eq!(mixed, optimal_bits(vietqr, 1));
        assert!(mixed < Segment::bytes(vietqr).bit_len(1).0 * 7 / 10);
        // UTF-8 stays in one byte segment.
        let s = "Thanh toán hóa đơn 123";
        assert_eq!(
            modes_of(&optimal_segments(s.as_bytes(), 1)),
            [(Mode::Byte, s.len())]
        );
    }
}
