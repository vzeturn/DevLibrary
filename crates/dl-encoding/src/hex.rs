//! Base16 (hexadecimal).

use crate::Error;
use alloc::string::String;
use alloc::vec::Vec;

const LOWER: &[u8; 16] = b"0123456789abcdef";
const UPPER: &[u8; 16] = b"0123456789ABCDEF";

/// Encodes bytes as lowercase hex.
pub fn encode(data: impl AsRef<[u8]>) -> String {
    encode_with(data.as_ref(), LOWER)
}

/// Encodes bytes as uppercase hex.
pub fn encode_upper(data: impl AsRef<[u8]>) -> String {
    encode_with(data.as_ref(), UPPER)
}

fn encode_with(data: &[u8], digits: &[u8; 16]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for &b in data {
        s.push(digits[(b >> 4) as usize] as char);
        s.push(digits[(b & 0x0f) as usize] as char);
    }
    s
}

#[inline]
fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decodes hex in either case. The input length must be even.
pub fn decode(input: impl AsRef<[u8]>) -> Result<Vec<u8>, Error> {
    let input = input.as_ref();
    let mut out = Vec::with_capacity(input.len() / 2);
    decode_into(input, &mut out)?;
    Ok(out)
}

/// Decodes hex and appends the bytes to `out`. On error `out` may hold a partial result.
pub fn decode_into(input: impl AsRef<[u8]>, out: &mut Vec<u8>) -> Result<(), Error> {
    let input = input.as_ref();
    if input.len() % 2 != 0 {
        return Err(Error::InvalidLength { len: input.len() });
    }
    out.reserve(input.len() / 2);
    for (i, pair) in input.as_chunks::<2>().0.iter().enumerate() {
        let hi = nibble(pair[0]).ok_or(Error::InvalidByte {
            index: 2 * i,
            byte: pair[0],
        })?;
        let lo = nibble(pair[1]).ok_or(Error::InvalidByte {
            index: 2 * i + 1,
            byte: pair[1],
        })?;
        out.push((hi << 4) | lo);
    }
    Ok(())
}

/// Decodes hex into a fixed-size array, e.g. a 32-byte key.
pub fn decode_array<const N: usize>(input: impl AsRef<[u8]>) -> Result<[u8; N], Error> {
    let input = input.as_ref();
    if input.len() != N * 2 {
        return Err(Error::InvalidLength { len: input.len() });
    }
    let mut out = [0u8; N];
    for (i, (slot, pair)) in out.iter_mut().zip(input.as_chunks::<2>().0).enumerate() {
        let hi = nibble(pair[0]).ok_or(Error::InvalidByte {
            index: 2 * i,
            byte: pair[0],
        })?;
        let lo = nibble(pair[1]).ok_or(Error::InvalidByte {
            index: 2 * i + 1,
            byte: pair[1],
        })?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        assert_eq!(encode_upper(b""), "");
        assert_eq!(encode_upper(b"f"), "66");
        assert_eq!(encode_upper(b"foobar"), "666F6F626172");
        assert_eq!(decode("666F6F626172").unwrap(), b"foobar");
        assert_eq!(decode("666f6f626172").unwrap(), b"foobar");
    }

    #[test]
    fn errors() {
        assert_eq!(decode("abc"), Err(Error::InvalidLength { len: 3 }));
        assert_eq!(
            decode("0g"),
            Err(Error::InvalidByte {
                index: 1,
                byte: b'g'
            })
        );
        assert_eq!(
            decode_array::<2>("00"),
            Err(Error::InvalidLength { len: 2 })
        );
        assert_eq!(decode_array::<2>("beef").unwrap(), [0xbe, 0xef]);
    }

    #[test]
    fn roundtrip_all_bytes() {
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(encode(&data)).unwrap(), data);
    }
}
