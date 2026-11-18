#![doc = include_str!("../README.md")]
#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod deflate;
mod inflate;

use alloc::vec::Vec;
use core::fmt;

/// How hard the compressor looks for matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    /// No compression: stored blocks only.
    Store,
    /// Short hash chains, no lazy matching.
    Fast,
    /// Balanced (the default).
    #[default]
    Default,
    /// Long hash chains and lazy matching.
    Best,
}

/// What went wrong while decompressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The input ends inside the stream.
    UnexpectedEof,
    /// Block type 3.
    InvalidBlockType,
    /// A stored block's length and its complement disagree.
    StoredLength,
    /// A Huffman code is over-subscribed, incomplete or malformed.
    InvalidCode,
    /// A length symbol 286/287 or a distance symbol 30/31.
    InvalidSymbol,
    /// A match refers to data before the start of the output.
    DistanceTooFar,
    /// The output would exceed the caller's limit.
    TooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// The gzip or zlib header is not valid (magic, method, flags, header checksum).
    InvalidHeader,
    /// The CRC-32 (gzip) or Adler-32 (zlib) of the output does not match the trailer.
    ChecksumMismatch,
    /// The gzip trailer's length (ISIZE) does not match the output.
    LengthMismatch,
    /// Bytes follow the zlib stream.
    TrailingData,
}

/// A decompression error with the byte offset in the input where it was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    offset: usize,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, offset: usize) -> Error {
        Error { kind, offset }
    }

    /// What went wrong.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Byte offset in the input.
    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            ErrorKind::UnexpectedEof => "unexpected end of input",
            ErrorKind::InvalidBlockType => "invalid block type",
            ErrorKind::StoredLength => "stored block length mismatch",
            ErrorKind::InvalidCode => "invalid Huffman code",
            ErrorKind::InvalidSymbol => "invalid length or distance symbol",
            ErrorKind::DistanceTooFar => "distance too far back",
            ErrorKind::TooLarge { limit } => {
                return write!(
                    f,
                    "decompressed data exceeds the limit of {limit} bytes (at input offset {})",
                    self.offset
                );
            }
            ErrorKind::InvalidHeader => "invalid header",
            ErrorKind::ChecksumMismatch => "checksum mismatch",
            ErrorKind::LengthMismatch => "length mismatch",
            ErrorKind::TrailingData => "data after the end of the stream",
        };
        write!(f, "{what} at input offset {}", self.offset)
    }
}

impl core::error::Error for Error {}

/// Raw DEFLATE (RFC 1951).
pub mod deflate_raw {
    use super::*;

    /// Compresses `data` into a raw DEFLATE stream.
    pub fn compress(data: &[u8], level: Level) -> Vec<u8> {
        deflate::deflate_into(data, level, Vec::new())
    }

    /// Decompresses a raw DEFLATE stream, producing at most `limit` bytes. Bytes after
    /// the final block are ignored; [`decompress_with_end`] reports where it ended.
    ///
    /// # Errors
    /// See [`ErrorKind`].
    pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
        decompress_with_end(data, limit).map(|(v, _)| v)
    }

    /// Like [`decompress`], also returning the input offset just after the stream.
    ///
    /// # Errors
    /// See [`ErrorKind`].
    pub fn decompress_with_end(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), Error> {
        let mut out = Vec::new();
        let end = inflate::inflate_into(data, 0, &mut out, limit)?;
        Ok((out, end))
    }
}

/// gzip (RFC 1952).
pub mod gzip {
    use super::*;

    /// Compresses `data` as one gzip member with no name and modification time 0, like
    /// Go's `gzip.NewWriterLevel` with an empty header (OS byte 255).
    ///
    /// ```
    /// let gz = dl_flate::gzip::compress(b"{\"type\":\"result\"}", dl_flate::Level::Default);
    /// assert_eq!(&gz[..3], &[0x1f, 0x8b, 8]);
    /// assert_eq!(dl_flate::gzip::decompress(&gz, 1 << 20).unwrap(), b"{\"type\":\"result\"}");
    /// ```
    pub fn compress(data: &[u8], level: Level) -> Vec<u8> {
        let xfl = match level {
            Level::Best => 2,
            Level::Fast => 4,
            _ => 0,
        };
        let header = alloc::vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, xfl, 255];
        let mut out = deflate::deflate_into(data, level, header);
        out.extend_from_slice(&dl_hash::crc32(data).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out
    }

    /// Decompresses gzip data, producing at most `limit` bytes. Concatenated members are
    /// decompressed one after the other (as Go's `gzip.Reader` and `gunzip` do); every
    /// member's CRC-32 and length are checked.
    ///
    /// # Errors
    /// See [`ErrorKind`].
    pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            pos = member(data, pos, &mut out, limit)?;
            if pos == data.len() {
                return Ok(out);
            }
        }
    }

    fn member(data: &[u8], start: usize, out: &mut Vec<u8>, limit: usize) -> Result<usize, Error> {
        let bad = |at| Error::new(ErrorKind::InvalidHeader, at);
        let eof = || Error::new(ErrorKind::UnexpectedEof, data.len());
        let h = data.get(start..start + 10).ok_or_else(eof)?;
        if h[0] != 0x1f || h[1] != 0x8b || h[2] != 8 {
            return Err(bad(start));
        }
        let flags = h[3];
        if flags & 0xE0 != 0 {
            return Err(bad(start + 3));
        }
        let mut pos = start + 10;
        if flags & 4 != 0 {
            let x = data.get(pos..pos + 2).ok_or_else(eof)?;
            pos += 2 + usize::from(u16::from_le_bytes([x[0], x[1]]));
        }
        for bit in [8u8, 16] {
            if flags & bit != 0 {
                let rest = data.get(pos..).ok_or_else(eof)?;
                let z = rest.iter().position(|&b| b == 0).ok_or_else(eof)?;
                pos += z + 1;
            }
        }
        if flags & 2 != 0 {
            let x = data.get(pos..pos + 2).ok_or_else(eof)?;
            let crc = dl_hash::crc32(&data[start..pos]) & 0xFFFF;
            if u32::from(u16::from_le_bytes([x[0], x[1]])) != crc {
                return Err(bad(pos));
            }
            pos += 2;
        }
        if pos > data.len() {
            return Err(eof());
        }
        let begin = out.len();
        let used = limit.saturating_sub(begin);
        let end = inflate::inflate_into(data, pos, out, used).map_err(|e| match e.kind {
            ErrorKind::TooLarge { .. } => Error::new(ErrorKind::TooLarge { limit }, e.offset),
            _ => e,
        })?;
        let t = data.get(end..end + 8).ok_or_else(eof)?;
        let crc = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
        let size = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
        if dl_hash::crc32(&out[begin..]) != crc {
            return Err(Error::new(ErrorKind::ChecksumMismatch, end));
        }
        if (out.len() - begin) as u32 != size {
            return Err(Error::new(ErrorKind::LengthMismatch, end + 4));
        }
        Ok(end + 8)
    }
}

/// zlib (RFC 1950).
pub mod zlib {
    use super::*;

    /// Adler-32 checksum.
    pub fn adler32(data: &[u8]) -> u32 {
        const MOD: u32 = 65521;
        let (mut a, mut b) = (1u32, 0u32);
        for chunk in data.chunks(5552) {
            for &x in chunk {
                a += u32::from(x);
                b += a;
            }
            a %= MOD;
            b %= MOD;
        }
        (b << 16) | a
    }

    /// Compresses `data` as a zlib stream (32 KiB window, no dictionary).
    pub fn compress(data: &[u8], level: Level) -> Vec<u8> {
        let flevel = match level {
            Level::Store | Level::Fast => 0,
            Level::Default => 2,
            Level::Best => 3,
        };
        let cmf = 0x78u16;
        let mut flg = flevel << 6;
        flg += 31 - ((cmf << 8) | flg) % 31;
        let header = alloc::vec![cmf as u8, flg as u8];
        let mut out = deflate::deflate_into(data, level, header);
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    /// Decompresses a zlib stream, producing at most `limit` bytes.
    ///
    /// # Errors
    /// See [`ErrorKind`]; a preset dictionary is reported as [`ErrorKind::InvalidHeader`].
    pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
        if data.len() < 2 {
            return Err(Error::new(ErrorKind::UnexpectedEof, data.len()));
        }
        let (cmf, flg) = (data[0], data[1]);
        if cmf & 0x0F != 8 || cmf >> 4 > 7 || ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
            return Err(Error::new(ErrorKind::InvalidHeader, 0));
        }
        if flg & 0x20 != 0 {
            return Err(Error::new(ErrorKind::InvalidHeader, 1));
        }
        let mut out = Vec::new();
        let end = inflate::inflate_into(data, 2, &mut out, limit)?;
        let t = data
            .get(end..end + 4)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, data.len()))?;
        if u32::from_be_bytes([t[0], t[1], t[2], t[3]]) != adler32(&out) {
            return Err(Error::new(ErrorKind::ChecksumMismatch, end));
        }
        if end + 4 != data.len() {
            return Err(Error::new(ErrorKind::TrailingData, end + 4));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
