#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod bits;
mod matrix;
mod render;
mod rs;
mod segment;
mod tables;

pub use render::SvgOptions;
pub use segment::{Mode, Segment};

use alloc::vec::Vec;
use bits::BitBuf;
use core::fmt;
use segment::{optimal_bits, optimal_segments, total_bits, version_range};
use tables::{MAX_VERSION, MIN_VERSION, data_bits};

/// Error correction level. Each level can restore roughly the given share of codewords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ecc {
    /// About 7 %.
    Low = 0,
    /// About 15 %.
    Medium = 1,
    /// About 25 %.
    Quartile = 2,
    /// About 30 %.
    High = 3,
}

/// Encoding error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The data does not fit in the largest allowed version at the requested level.
    DataTooLong {
        /// Bits needed at the largest allowed version.
        bits: usize,
        /// Data bits available at the largest allowed version.
        capacity: usize,
    },
    /// The version range is empty or outside `1..=40`.
    InvalidVersion {
        /// Requested smallest version.
        min: u8,
        /// Requested largest version.
        max: u8,
    },
    /// Mask pattern number above 7.
    InvalidMask(u8),
    /// A byte that the segment's mode cannot encode.
    InvalidCharacter {
        /// Byte offset of the offending character.
        index: usize,
        /// The mode that rejected it.
        mode: Mode,
    },
    /// ECI assignment number above 999 999.
    InvalidEci(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::DataTooLong { bits, capacity } => {
                write!(
                    f,
                    "data too long: needs {bits} bits, capacity is {capacity} bits"
                )
            }
            Error::InvalidVersion { min, max } => {
                write!(f, "invalid version range {min}..={max} (allowed 1..=40)")
            }
            Error::InvalidMask(m) => write!(f, "invalid mask pattern {m} (allowed 0..=7)"),
            Error::InvalidCharacter { index, mode } => {
                write!(
                    f,
                    "character at offset {index} is not valid in {mode:?} mode"
                )
            }
            Error::InvalidEci(n) => write!(f, "invalid ECI assignment number {n}"),
        }
    }
}

impl core::error::Error for Error {}

/// An encoded QR Code symbol: a square grid of dark and light modules.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct QrCode {
    version: u8,
    ecc: Ecc,
    mask: u8,
    size: usize,
    modules: Vec<bool>,
}

impl fmt::Debug for QrCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QrCode")
            .field("version", &self.version)
            .field("ecc", &self.ecc)
            .field("mask", &self.mask)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl QrCode {
    /// Encodes text in the smallest version that fits at level `ecc`, choosing the optimal
    /// mix of numeric, alphanumeric and byte (UTF-8) segments and the best mask.
    pub fn encode_text(text: &str, ecc: Ecc) -> Result<Self, Error> {
        QrBuilder::new().ecc(ecc).encode_text(text)
    }

    /// Encodes raw bytes as one byte-mode segment, in the smallest version that fits.
    pub fn encode_bytes(data: &[u8], ecc: Ecc) -> Result<Self, Error> {
        QrBuilder::new().ecc(ecc).encode_bytes(data)
    }

    /// Returns a builder for version range, mask, ECC boost and ECI options.
    pub fn builder() -> QrBuilder {
        QrBuilder::new()
    }

    /// Version number, 1..=40.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Side length in modules (`4 * version + 17`), without quiet zone.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Error correction level actually used (may be higher than requested with
    /// [`QrBuilder::boost_ecc`]).
    pub fn ecc(&self) -> Ecc {
        self.ecc
    }

    /// Mask pattern number, 0..=7.
    pub fn mask(&self) -> u8 {
        self.mask
    }

    /// Whether the module at column `x`, row `y` is dark. Returns `false` outside the symbol.
    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.modules[y * self.size + x]
    }

    /// The modules as rows (`matrix[y][x]`, `true` = dark), without quiet zone.
    pub fn to_matrix(&self) -> Vec<Vec<bool>> {
        self.modules
            .chunks(self.size)
            .map(<[bool]>::to_vec)
            .collect()
    }
}

/// Encoder options. Created by [`QrCode::builder`]; every setter returns the builder.
///
/// Defaults: [`Ecc::Medium`], versions 1..=40, automatic mask, no ECC boost, no ECI.
/// Invalid settings are reported by the `encode_*` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QrBuilder {
    ecc: Ecc,
    min_version: u8,
    max_version: u8,
    mask: Option<u8>,
    boost_ecc: bool,
    eci_utf8: bool,
}

impl Default for QrBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl QrBuilder {
    /// Default options.
    pub fn new() -> Self {
        Self {
            ecc: Ecc::Medium,
            min_version: MIN_VERSION,
            max_version: MAX_VERSION,
            mask: None,
            boost_ecc: false,
            eci_utf8: false,
        }
    }

    /// Minimum error correction level.
    pub fn ecc(mut self, ecc: Ecc) -> Self {
        self.ecc = ecc;
        self
    }

    /// Smallest version to use (1..=40).
    pub fn min_version(mut self, version: u8) -> Self {
        self.min_version = version;
        self
    }

    /// Largest version to use (1..=40). Data that does not fit fails with
    /// [`Error::DataTooLong`].
    pub fn max_version(mut self, version: u8) -> Self {
        self.max_version = version;
        self
    }

    /// Forces one version: same as `min_version(v).max_version(v)`.
    pub fn version(self, version: u8) -> Self {
        self.min_version(version).max_version(version)
    }

    /// Mask pattern 0..=7, or `None` to pick the one with the lowest penalty.
    pub fn mask(mut self, mask: Option<u8>) -> Self {
        self.mask = mask;
        self
    }

    /// After the version is chosen, raise the ECC level as far as the data still fits in
    /// that version. Off by default.
    pub fn boost_ecc(mut self, boost: bool) -> Self {
        self.boost_ecc = boost;
        self
    }

    /// Prefix text with an ECI designator for UTF-8 (assignment 26). Applies to
    /// [`encode_text`](Self::encode_text) only. Off by default.
    pub fn eci_utf8(mut self, eci: bool) -> Self {
        self.eci_utf8 = eci;
        self
    }

    fn validate(&self) -> Result<(), Error> {
        let (min, max) = (self.min_version, self.max_version);
        if min < MIN_VERSION || max > MAX_VERSION || min > max {
            return Err(Error::InvalidVersion { min, max });
        }
        match self.mask {
            Some(m) if m > 7 => Err(Error::InvalidMask(m)),
            _ => Ok(()),
        }
    }

    /// Smallest version in range whose capacity fits `bits_at(version)`. `bits_at` returns
    /// the bit length and whether every character count fits its field.
    fn select_version(
        &self,
        mut bits_at: impl FnMut(u8) -> (usize, bool),
    ) -> Result<(u8, usize), Error> {
        let mut last = 0;
        for v in self.min_version..=self.max_version {
            let (bits, fits) = bits_at(v);
            if fits && bits <= data_bits(v, self.ecc) {
                return Ok((v, bits));
            }
            last = bits;
        }
        Err(Error::DataTooLong {
            bits: last,
            capacity: data_bits(self.max_version, self.ecc),
        })
    }

    /// Encodes text with optimal segmentation (see [`QrCode::encode_text`]).
    pub fn encode_text(&self, text: &str) -> Result<QrCode, Error> {
        self.validate()?;
        let bytes = text.as_bytes();
        let eci = if self.eci_utf8 {
            Some(Segment::eci(26)?)
        } else {
            None
        };
        let eci_bits = eci
            .as_ref()
            .map_or(0, |s| total_bits(core::slice::from_ref(s), 1).0);
        // The optimal segmentation only depends on the version range; compute its length
        // without allocating so that oversized input is rejected cheaply. Within a range,
        // any segment short enough for the symbol capacity also fits its character count
        // field (e.g. versions 1-9: at most 552 digits, 335 alphanumeric characters or 230
        // bytes vs. fields of 1023 / 511 / 255), and `finish` re-checks it anyway.
        let mut cache: [Option<usize>; 3] = [None; 3];
        let (version, _) = self.select_version(|v| {
            let bits = *cache[version_range(v)]
                .get_or_insert_with(|| optimal_bits(bytes, v).saturating_add(eci_bits));
            (bits, true)
        })?;
        let mut segments = Vec::new();
        segments.extend(eci);
        segments.extend(optimal_segments(bytes, version));
        self.finish(&segments, version)
    }

    /// Encodes raw bytes as a single byte-mode segment (see [`QrCode::encode_bytes`]).
    pub fn encode_bytes(&self, data: &[u8]) -> Result<QrCode, Error> {
        self.validate()?;
        let len = data.len();
        let (version, _) = self.select_version(|v| {
            let cc = Mode::Byte.char_count_bits(v);
            (len.saturating_mul(8).saturating_add(4 + cc), len < 1 << cc)
        })?;
        self.finish(&[Segment::bytes(data)], version)
    }

    /// Encodes the given segments in order, without re-segmenting. Use this to force a mode
    /// or to insert ECI designators.
    ///
    /// ```
    /// use dl_qr::{Ecc, QrCode, Segment};
    ///
    /// let segments = [Segment::bytes(b"Order "), Segment::numeric("0012345678")?];
    /// let qr = QrCode::builder().ecc(Ecc::High).encode_segments(&segments)?;
    /// assert_eq!(qr.version(), 2);
    /// # Ok::<(), dl_qr::Error>(())
    /// ```
    pub fn encode_segments(&self, segments: &[Segment]) -> Result<QrCode, Error> {
        self.validate()?;
        let (version, _) = self.select_version(|v| total_bits(segments, v))?;
        self.finish(segments, version)
    }

    /// Builds the symbol once the version is known to fit.
    fn finish(&self, segments: &[Segment], version: u8) -> Result<QrCode, Error> {
        let (bits, fits) = total_bits(segments, version);
        let mut ecc = self.ecc;
        if !fits || bits > data_bits(version, ecc) {
            // Unreachable: callers only pass a version that `select_version` accepted.
            return Err(Error::DataTooLong {
                bits,
                capacity: data_bits(version, ecc),
            });
        }
        if self.boost_ecc {
            for e in [Ecc::Medium, Ecc::Quartile, Ecc::High] {
                if e > ecc && bits <= data_bits(version, e) {
                    ecc = e;
                }
            }
        }

        let data = data_codewords(segments, version, ecc);
        let codewords = rs::encode_blocks(&data, version, ecc);
        let mut grid = matrix::Grid::new(version);
        grid.draw_codewords(&codewords);
        let mask = match self.mask {
            Some(m) => m,
            None => grid.best_mask(ecc),
        };
        grid.apply_mask(mask);
        grid.draw_format(ecc, mask);
        Ok(QrCode {
            version,
            ecc,
            mask,
            size: tables::size(version),
            modules: grid.into_modules(),
        })
    }
}

/// Data codewords: segments, terminator, zero bits to a byte boundary, then alternating
/// pad codewords 0xEC 0x11 (ISO/IEC 18004 7.4.9 - 7.4.10). The segments must fit.
fn data_codewords(segments: &[Segment], version: u8, ecc: Ecc) -> Vec<u8> {
    let capacity = data_bits(version, ecc);
    let mut stream = BitBuf::with_capacity(capacity);
    for s in segments {
        s.write(&mut stream, version);
    }
    stream.push(0, capacity.saturating_sub(stream.len()).min(4));
    stream.push(0, (8 - stream.len() % 8) % 8);
    let mut pad = 0xEC;
    while stream.len() < capacity {
        stream.push(pad, 8);
        pad ^= 0xEC ^ 0x11;
    }
    stream.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;

    #[test]
    fn hello_world_data_codewords() {
        // thonky.com QR tutorial, verified with segno: "HELLO WORLD", version 1-Q and 1-M.
        let seg = [Segment::alphanumeric("HELLO WORLD").unwrap()];
        assert_eq!(
            data_codewords(&seg, 1, Ecc::Quartile),
            [32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236]
        );
        assert_eq!(
            data_codewords(&seg, 1, Ecc::Medium),
            [
                32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17
            ]
        );
        // The optimal segmentation of the same text is the single alphanumeric segment.
        let auto = segment::optimal_segments(b"HELLO WORLD", 1);
        assert_eq!(auto, seg);
    }

    fn capacity_edge(make: impl Fn(usize) -> String, max: usize, ecc: Ecc) {
        let qr = QrCode::builder().ecc(ecc).encode_text(&make(max)).unwrap();
        assert_eq!(qr.version(), 40);
        let err = QrCode::builder()
            .ecc(ecc)
            .encode_text(&make(max + 1))
            .unwrap_err();
        assert!(matches!(err, Error::DataTooLong { bits, capacity } if bits > capacity));
    }

    #[test]
    fn capacities() {
        let digits = |n: usize| "7".repeat(n);
        // Version 1-L holds 41 digits.
        let qr = QrCode::builder()
            .ecc(Ecc::Low)
            .max_version(1)
            .encode_text(&digits(41))
            .unwrap();
        assert_eq!(qr.version(), 1);
        assert!(matches!(
            QrCode::builder()
                .ecc(Ecc::Low)
                .max_version(1)
                .encode_text(&digits(42)),
            Err(Error::DataTooLong {
                bits: 154,
                capacity: 152
            })
        ));
        capacity_edge(digits, 7089, Ecc::Low);
        capacity_edge(digits, 3057, Ecc::High);
        capacity_edge(|n| "A".repeat(n), 4296, Ecc::Low);
        capacity_edge(|n| "a".repeat(n), 2953, Ecc::Low);
        capacity_edge(|n| "a".repeat(n), 1273, Ecc::High);
        let bytes = std::vec![0xA5u8; 2953];
        assert_eq!(
            QrCode::encode_bytes(&bytes, Ecc::Low).unwrap().version(),
            40
        );
        assert_eq!(
            QrCode::encode_bytes(&[0; 2954], Ecc::Low),
            Err(Error::DataTooLong {
                bits: 2954 * 8 + 20,
                capacity: 2956 * 8
            })
        );
    }

    #[test]
    fn huge_input_is_rejected_without_panic() {
        let big = "x".repeat(1 << 20);
        assert!(matches!(
            QrCode::encode_text(&big, Ecc::Low),
            Err(Error::DataTooLong { .. })
        ));
        assert!(matches!(
            QrCode::encode_bytes(big.as_bytes(), Ecc::Low),
            Err(Error::DataTooLong { .. })
        ));
    }

    #[test]
    fn invalid_options() {
        let b = QrCode::builder();
        assert_eq!(
            b.min_version(0).encode_text("a"),
            Err(Error::InvalidVersion { min: 0, max: 40 })
        );
        assert_eq!(
            b.max_version(41).encode_text("a"),
            Err(Error::InvalidVersion { min: 1, max: 41 })
        );
        assert_eq!(
            b.min_version(5).max_version(4).encode_bytes(b"a"),
            Err(Error::InvalidVersion { min: 5, max: 4 })
        );
        assert_eq!(
            b.mask(Some(8)).encode_segments(&[]),
            Err(Error::InvalidMask(8))
        );
        for e in [
            Error::DataTooLong {
                bits: 1,
                capacity: 0,
            },
            Error::InvalidVersion { min: 0, max: 0 },
            Error::InvalidMask(9),
            Error::InvalidCharacter {
                index: 3,
                mode: Mode::Numeric,
            },
            Error::InvalidEci(1_000_000),
        ] {
            assert!(!std::format!("{e}").is_empty());
        }
    }

    #[test]
    fn version_and_mask_selection() {
        let qr = QrCode::encode_text("", Ecc::High).unwrap();
        assert_eq!((qr.version(), qr.size()), (1, 21));
        let qr = QrCode::builder().min_version(7).encode_text("a").unwrap();
        assert_eq!(qr.version(), 7);
        for m in 0..8 {
            let qr = QrCode::builder().mask(Some(m)).encode_text("mask").unwrap();
            assert_eq!(qr.mask(), m);
        }
        assert!(!qr_get_outside());
    }

    fn qr_get_outside() -> bool {
        let qr = QrCode::encode_text("a", Ecc::Low).unwrap();
        qr.get(21, 0) || qr.get(0, 21) || qr.get(usize::MAX, usize::MAX)
    }

    #[test]
    fn boost_ecc_raises_level_within_version() {
        // 10 digits fit version 1 at every level.
        let qr = QrCode::builder()
            .ecc(Ecc::Low)
            .boost_ecc(true)
            .encode_text("0123456789")
            .unwrap();
        assert_eq!((qr.version(), qr.ecc()), (1, Ecc::High));
        // 17 bytes: version 1-L (capacity 17 bytes) but nothing higher in version 1.
        let qr = QrCode::builder()
            .ecc(Ecc::Low)
            .boost_ecc(true)
            .encode_text("abcdefghijklmnopq")
            .unwrap();
        assert_eq!((qr.version(), qr.ecc()), (1, Ecc::Low));
        let qr = QrCode::builder()
            .ecc(Ecc::Low)
            .encode_text("0123456789")
            .unwrap();
        assert_eq!(qr.ecc(), Ecc::Low);
    }

    #[test]
    fn eci_adds_twelve_bits() {
        // 14 bytes fill version 1-M exactly (4 + 8 + 112 = 124 of 128 bits); ECI pushes
        // it to version 2.
        let s = "abcdefghijklmn";
        assert_eq!(QrCode::encode_text(s, Ecc::Medium).unwrap().version(), 1);
        let qr = QrCode::builder().eci_utf8(true).encode_text(s).unwrap();
        assert_eq!(qr.version(), 2);
    }

    #[test]
    fn matrix_matches_get() {
        let qr = QrCode::encode_text("matrix", Ecc::Quartile).unwrap();
        let m = qr.to_matrix();
        assert_eq!(m.len(), qr.size());
        for (y, row) in m.iter().enumerate() {
            assert_eq!(row.len(), qr.size());
            for (x, &d) in row.iter().enumerate() {
                assert_eq!(d, qr.get(x, y));
            }
        }
    }
}
