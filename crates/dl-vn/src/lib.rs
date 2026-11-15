#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod cccd;
pub mod phone;
pub mod tax;
pub mod text;
pub mod vietqr;
pub mod words;

use core::fmt;

/// Error type shared by every parser and builder in this crate.
///
/// `index` fields are byte offsets into the input string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The input is empty or contains no digits.
    Empty,
    /// The input has a number of digits this format never has.
    InvalidLength {
        /// Number of digits found.
        len: usize,
    },
    /// A character that is not allowed at this position.
    InvalidCharacter {
        /// Byte offset of the character.
        index: usize,
        /// The character.
        ch: char,
    },
    /// The number does not fit in a `u64`.
    Overflow,
    /// The amount has more fractional digits than the currency's minor unit.
    TooManyDecimals {
        /// Byte offset of the first digit that cannot be represented.
        index: usize,
    },
    /// The number of minor-unit digits passed to [`words::amount_to_words`] is not in `1..=18`.
    InvalidMinorDigits {
        /// The rejected value.
        digits: u32,
    },
    /// The check digit does not match.
    Checksum,
    /// A component that is never issued, such as an all-zero serial or branch number.
    InvalidComponent,
    /// A well-formed code that is not allocated: mobile prefix, area code or province code.
    UnknownCode {
        /// The numeric code, without leading zeros.
        code: u32,
    },
    /// A VietQR data object (tag, length, value) is malformed or repeated.
    MalformedTlv {
        /// Byte offset where the data object starts.
        index: usize,
    },
    /// A required VietQR data object is missing. `tag` is the dotted path, e.g. `"38.01.00"`.
    MissingField {
        /// Dotted tag path.
        tag: &'static str,
    },
    /// A VietQR data object has a value that is not allowed. `tag` is the dotted path.
    InvalidField {
        /// Dotted tag path.
        tag: &'static str,
    },
    /// The VietQR CRC (tag 63) does not match the payload.
    CrcMismatch {
        /// CRC computed over the payload.
        expected: u16,
        /// CRC found in the payload.
        found: u16,
    },
    /// Valid input of a kind this crate does not handle, such as a foreign phone number or a
    /// QR code that is not a NAPAS transfer.
    Unsupported {
        /// What is unsupported.
        what: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Empty => f.write_str("input is empty"),
            Error::InvalidLength { len } => write!(f, "invalid length: {len} digits"),
            Error::InvalidCharacter { index, ch } => {
                write!(f, "invalid character {ch:?} at offset {index}")
            }
            Error::Overflow => f.write_str("number too large"),
            Error::TooManyDecimals { index } => {
                write!(f, "too many decimal digits starting at offset {index}")
            }
            Error::InvalidMinorDigits { digits } => {
                write!(f, "minor unit digits must be 1..=18, got {digits}")
            }
            Error::Checksum => f.write_str("check digit mismatch"),
            Error::InvalidComponent => f.write_str("component is never issued"),
            Error::UnknownCode { code } => write!(f, "unallocated code {code}"),
            Error::MalformedTlv { index } => write!(f, "malformed data object at offset {index}"),
            Error::MissingField { tag } => write!(f, "missing field {tag}"),
            Error::InvalidField { tag } => write!(f, "invalid value in field {tag}"),
            Error::CrcMismatch { expected, found } => {
                write!(
                    f,
                    "CRC mismatch: computed {expected:04X}, found {found:04X}"
                )
            }
            Error::Unsupported { what } => write!(f, "unsupported: {what}"),
        }
    }
}

impl core::error::Error for Error {}

/// Returns the ASCII digits of `bytes` as `&str`. Callers only pass ASCII digit buffers.
pub(crate) fn ascii_str(bytes: &[u8]) -> &str {
    // Every caller stores only ASCII digits, so this never falls back.
    core::str::from_utf8(bytes).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::{String, ToString};

    /// Feeds pseudo-random strings built from characters that matter to the parsers into
    /// every parser. Any panic fails the test; results are not checked.
    #[test]
    fn parsers_never_panic() {
        const PIECES: [&str; 24] = [
            "0",
            "1",
            "2",
            "5",
            "8",
            "9",
            "84",
            "+",
            "-",
            ".",
            " ",
            "(",
            ")",
            "é",
            "Đ",
            "\u{301}",
            "６",
            "\u{0}",
            "0002010102",
            "38",
            "6304",
            "A000000727",
            "QRIBFTTA",
            "5303704",
        ];
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };
        for _ in 0..20_000 {
            let len = next() % 24;
            let mut s = String::new();
            for _ in 0..len {
                s.push_str(PIECES[next() % PIECES.len()]);
            }
            let _ = tax::TaxCode::parse(&s);
            let _ = cccd::Cccd::parse(&s);
            let _ = phone::normalize_phone(&s);
            let _ = vietqr::VietQr::parse(&s);
            let _ = words::amount_to_words(&s, "đồng", Some(("xu", 2)));
            let _ = words::amount_to_words(&s, "đồng", None);
            let _ = text::slugify(&s);
            let _ = text::normalize_name(&s);
            let _ = text::compare_vi(&s, "Nguyễn");
        }
    }

    #[test]
    fn error_messages() {
        assert_eq!(Error::Empty.to_string(), "input is empty");
        assert_eq!(
            Error::InvalidCharacter { index: 3, ch: 'x' }.to_string(),
            "invalid character 'x' at offset 3"
        );
        assert_eq!(
            Error::CrcMismatch {
                expected: 0x2E2E,
                found: 0xAB
            }
            .to_string(),
            "CRC mismatch: computed 2E2E, found 00AB"
        );
        assert_eq!(
            Error::MissingField { tag: "38.01.00" }.to_string(),
            "missing field 38.01.00"
        );
    }
}
