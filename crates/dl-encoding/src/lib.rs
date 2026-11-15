#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod base32;
pub mod base64;
pub mod hex;

mod radix;

use core::fmt;

/// Decoding error. Indexes are byte offsets into the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A byte that is not part of the alphabet (or misplaced padding).
    InvalidByte {
        /// Offset of the offending byte.
        index: usize,
        /// The offending byte.
        byte: u8,
    },
    /// The input length cannot be produced by this encoding.
    InvalidLength {
        /// Length of the input.
        len: usize,
    },
    /// The last symbol carries bits that do not fit in the decoded output.
    /// Such input is not canonical and is rejected.
    TrailingBits {
        /// Offset of the last symbol.
        index: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::InvalidByte { index, byte } => {
                write!(f, "invalid byte 0x{byte:02x} at offset {index}")
            }
            Error::InvalidLength { len } => write!(f, "invalid input length {len}"),
            Error::TrailingBits { index } => {
                write!(f, "non-zero trailing bits in symbol at offset {index}")
            }
        }
    }
}

impl core::error::Error for Error {}
