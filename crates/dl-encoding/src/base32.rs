//! Base32 with the RFC 4648 alphabet (`A-Z2-7`).
//!
//! [`decode_secret`] is a lenient decoder for human-entered secrets such as TOTP keys
//! from authenticator apps: it ignores case, spaces, dashes and missing padding.

use crate::Error;
use crate::radix::{decode_group, decode_table};
use alloc::string::String;
use alloc::vec::Vec;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
static DECODE: [u8; 256] = decode_table(ALPHABET);

/// A Base32 padding policy.
#[derive(Debug, Clone, Copy)]
pub struct Engine {
    pad: bool,
}

/// RFC 4648 Base32, `=` padding required.
pub const STANDARD: Engine = Engine { pad: true };
/// RFC 4648 Base32, no padding.
pub const STANDARD_NO_PAD: Engine = Engine { pad: false };

/// Characters produced for a final group of 1..=5 bytes.
const CHARS_FOR_BYTES: [usize; 6] = [0, 2, 4, 5, 7, 8];

impl Engine {
    /// Number of characters `encode` produces for `n` input bytes.
    pub const fn encoded_len(&self, n: usize) -> usize {
        let full = n / 5 * 8;
        let rem = n % 5;
        if rem == 0 {
            full
        } else if self.pad {
            full + 8
        } else {
            full + CHARS_FOR_BYTES[rem]
        }
    }

    /// Encodes bytes into a new `String`.
    pub fn encode(&self, data: impl AsRef<[u8]>) -> String {
        let data = data.as_ref();
        let mut out = Vec::with_capacity(self.encoded_len(data.len()));
        for chunk in data.chunks(5) {
            let mut buf = [0u8; 8];
            buf[3..3 + chunk.len()].copy_from_slice(chunk);
            let v = u64::from_be_bytes(buf);
            let n = CHARS_FOR_BYTES[chunk.len()];
            for i in 0..n {
                out.push(ALPHABET[((v >> (35 - 5 * i)) & 31) as usize]);
            }
            if self.pad {
                out.resize(out.len() + 8 - n, b'=');
            }
        }
        // Every byte comes from the ASCII alphabet or is `=`.
        String::from_utf8(out).expect("base32 output is ASCII")
    }

    /// Decodes Base32 (uppercase only) into a new `Vec`.
    pub fn decode(&self, input: impl AsRef<[u8]>) -> Result<Vec<u8>, Error> {
        let input = input.as_ref();
        let mut end = input.len();
        if self.pad {
            if end % 8 != 0 {
                return Err(Error::InvalidLength { len: end });
            }
            let mut pads = 0;
            while pads < 6 && end > 0 && input[end - 1] == b'=' {
                end -= 1;
                pads += 1;
            }
        }
        let body = &input[..end];
        if matches!(body.len() % 8, 1 | 3 | 6) {
            return Err(Error::InvalidLength { len: input.len() });
        }
        let mut out = Vec::with_capacity(body.len() * 5 / 8);
        for (i, group) in body.chunks(8).enumerate() {
            decode_group(&DECODE, 5, group, i * 8, &mut out)?;
        }
        Ok(out)
    }
}

/// Leniently decodes a human-entered Base32 secret: case-insensitive, ignores ASCII
/// whitespace and `-`, and accepts input with or without trailing `=` padding.
pub fn decode_secret(input: &str) -> Result<Vec<u8>, Error> {
    let mut clean = Vec::with_capacity(input.len());
    for b in input.bytes() {
        if !(b.is_ascii_whitespace() || b == b'-') {
            clean.push(b.to_ascii_uppercase());
        }
    }
    while clean.last() == Some(&b'=') {
        clean.pop();
    }
    STANDARD_NO_PAD.decode(&clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &[(&str, &str)] = &[
        ("", ""),
        ("f", "MY======"),
        ("fo", "MZXQ===="),
        ("foo", "MZXW6==="),
        ("foob", "MZXW6YQ="),
        ("fooba", "MZXW6YTB"),
        ("foobar", "MZXW6YTBOI======"),
    ];

    #[test]
    fn rfc4648_vectors() {
        for &(raw, enc) in VECTORS {
            assert_eq!(STANDARD.encode(raw), enc);
            assert_eq!(STANDARD.decode(enc).unwrap(), raw.as_bytes());
            let nopad = enc.trim_end_matches('=');
            assert_eq!(STANDARD_NO_PAD.encode(raw), nopad);
            assert_eq!(STANDARD_NO_PAD.decode(nopad).unwrap(), raw.as_bytes());
        }
    }

    #[test]
    fn strictness() {
        assert_eq!(STANDARD.decode("MY"), Err(Error::InvalidLength { len: 2 }));
        assert_eq!(
            STANDARD.decode("MYA====="),
            Err(Error::InvalidLength { len: 8 })
        );
        assert_eq!(
            STANDARD.decode("my======"),
            Err(Error::InvalidByte {
                index: 0,
                byte: b'm'
            })
        );
        assert_eq!(
            STANDARD_NO_PAD.decode("MZ"),
            Err(Error::TrailingBits { index: 1 })
        );
    }

    #[test]
    fn secret() {
        // RFC 6238 SHA-1 seed "12345678901234567890".
        let key = decode_secret("gezd gnbv-gy3t qojq gezd gnbv gy3t qojq").unwrap();
        assert_eq!(key, b"12345678901234567890");
        assert_eq!(decode_secret("MZXW6===").unwrap(), b"foo");
    }

    #[test]
    fn roundtrip_lengths() {
        let data: Vec<u8> = (0..=255u8).collect();
        for n in 0..data.len() {
            for engine in [STANDARD, STANDARD_NO_PAD] {
                let enc = engine.encode(&data[..n]);
                assert_eq!(enc.len(), engine.encoded_len(n));
                assert_eq!(engine.decode(&enc).unwrap(), &data[..n]);
            }
        }
    }
}
