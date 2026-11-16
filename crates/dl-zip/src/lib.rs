#![doc = include_str!("../README.md")]

mod cp437;
mod path;
mod read;
mod time;
mod write;

use std::fmt;

pub use dl_flate::Level;
pub use read::{Entry, ExtractLimits, ZipArchive};
pub use time::DosDateTime;
pub use write::{Method, Modified, Options, ZipWriter};

pub(crate) const SIG_LOCAL: u32 = 0x0403_4b50;
pub(crate) const SIG_CENTRAL: u32 = 0x0201_4b50;
pub(crate) const SIG_EOCD: u32 = 0x0605_4b50;
pub(crate) const SIG_EOCD64: u32 = 0x0606_4b50;
pub(crate) const SIG_LOCATOR64: u32 = 0x0706_4b50;

/// Everything that can go wrong while reading, writing or extracting an archive.
///
/// Offsets are byte positions in the archive; indexes are positions in
/// [`ZipArchive::entries`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An I/O error from the underlying reader, writer or file system.
    Io(std::io::Error),
    /// No end of central directory record in the last 65 557 bytes: not a ZIP archive.
    NotZip,
    /// A structure is malformed, inconsistent or cut short.
    Malformed {
        /// What is wrong.
        what: &'static str,
        /// Where the structure starts.
        offset: u64,
    },
    /// The archive is split over several disks (multi-volume archives are not supported).
    MultiDisk,
    /// The entry uses a compression method other than stored (0) or deflate (8).
    UnsupportedMethod {
        /// Entry index.
        index: usize,
        /// The method code from the central directory.
        method: u16,
    },
    /// The entry is encrypted (general purpose flag bit 0).
    Encrypted {
        /// Entry index.
        index: usize,
    },
    /// The entry's deflate stream is invalid; the inner error's offset is relative to the
    /// start of the entry's compressed data.
    Deflate {
        /// Entry index.
        index: usize,
        /// The decompression error.
        source: dl_flate::Error,
    },
    /// The CRC-32 of the extracted data does not match the central directory.
    ChecksumMismatch {
        /// Entry index.
        index: usize,
        /// CRC-32 from the central directory.
        expected: u32,
        /// CRC-32 of the data.
        actual: u32,
    },
    /// The extracted data is longer or shorter than the size in the central directory.
    SizeMismatch {
        /// Entry index.
        index: usize,
        /// The uncompressed size from the central directory.
        declared: u64,
    },
    /// The entry's declared size is above the caller's limit.
    TooLarge {
        /// Entry index.
        index: usize,
        /// The uncompressed size from the central directory.
        size: u64,
        /// The caller's limit.
        limit: u64,
    },
    /// The sum of the entry sizes is above [`ExtractLimits::max_total_size`].
    TotalTooLarge {
        /// The limit in bytes.
        limit: u64,
    },
    /// The archive has more entries than [`ExtractLimits::max_entries`].
    TooManyEntries {
        /// Number of entries in the archive.
        count: usize,
        /// The limit.
        limit: usize,
    },
    /// No entry has this name.
    NotFound(String),
    /// The index is not below the number of entries.
    IndexOutOfRange {
        /// The index asked for.
        index: usize,
        /// Number of entries.
        len: usize,
    },
    /// The entry name would land outside the target directory (absolute path, drive
    /// letter, `..`, NUL, ...); nothing was extracted.
    UnsafeName {
        /// Entry index.
        index: usize,
        /// The decoded name.
        name: String,
    },
    /// The writer refused an entry name.
    InvalidName {
        /// The name as given.
        name: String,
        /// Why it was refused.
        reason: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::NotZip => f.write_str("not a ZIP archive (no end of central directory record)"),
            Error::Malformed { what, offset } => write!(f, "{what} at offset {offset}"),
            Error::MultiDisk => f.write_str("multi-disk archives are not supported"),
            Error::UnsupportedMethod { index, method } => {
                write!(f, "entry {index}: unsupported compression method {method}")
            }
            Error::Encrypted { index } => write!(f, "entry {index} is encrypted"),
            Error::Deflate { index, source } => write!(f, "entry {index}: {source}"),
            Error::ChecksumMismatch {
                index,
                expected,
                actual,
            } => write!(
                f,
                "entry {index}: CRC-32 mismatch (expected {expected:08x}, got {actual:08x})"
            ),
            Error::SizeMismatch { index, declared } => write!(
                f,
                "entry {index}: data does not match the declared size of {declared} bytes"
            ),
            Error::TooLarge { index, size, limit } => write!(
                f,
                "entry {index}: {size} bytes exceed the limit of {limit} bytes"
            ),
            Error::TotalTooLarge { limit } => {
                write!(f, "entries exceed the total limit of {limit} bytes")
            }
            Error::TooManyEntries { count, limit } => {
                write!(f, "{count} entries exceed the limit of {limit}")
            }
            Error::NotFound(name) => write!(f, "no entry named {name:?}"),
            Error::IndexOutOfRange { index, len } => {
                write!(f, "entry index {index} out of range ({len} entries)")
            }
            Error::UnsafeName { index, name } => {
                write!(f, "entry {index}: unsafe name {name:?}")
            }
            Error::InvalidName { name, reason } => {
                write!(f, "invalid entry name {name:?}: {reason}")
            }
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Deflate { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests;
