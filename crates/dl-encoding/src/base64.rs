//! Base64 with the standard (`+/`) and URL-safe (`-_`) alphabets.
//!
//! Pick one of the four ready-made engines:
//!
//! | Engine | Alphabet | Padding | Typical use |
//! |---|---|---|---|
//! | [`STANDARD`] | `+/` | required | MIME, .NET `Convert.ToBase64String` |
//! | [`STANDARD_NO_PAD`] | `+/` | rejected | |
//! | [`URL_SAFE`] | `-_` | required | |
//! | [`URL_SAFE_NO_PAD`] | `-_` | rejected | JWT, URLs |

use crate::Error;
use crate::radix::{decode_group, decode_table};
use alloc::string::String;
use alloc::vec::Vec;

const STD_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
static STD_DECODE: [u8; 256] = decode_table(STD_ALPHABET);
static URL_DECODE: [u8; 256] = decode_table(URL_ALPHABET);

/// A Base64 configuration: alphabet plus padding policy.
#[derive(Debug, Clone, Copy)]
pub struct Engine {
    encode: &'static [u8; 64],
    decode: &'static [u8; 256],
    pad: bool,
}

/// Standard alphabet, `=` padding required.
pub const STANDARD: Engine = Engine {
    encode: STD_ALPHABET,
    decode: &STD_DECODE,
    pad: true,
};
/// Standard alphabet, no padding.
pub const STANDARD_NO_PAD: Engine = Engine {
    encode: STD_ALPHABET,
    decode: &STD_DECODE,
    pad: false,
};
/// URL-safe alphabet, `=` padding required.
pub const URL_SAFE: Engine = Engine {
    encode: URL_ALPHABET,
    decode: &URL_DECODE,
    pad: true,
};
/// URL-safe alphabet, no padding (used by JWT).
pub const URL_SAFE_NO_PAD: Engine = Engine {
    encode: URL_ALPHABET,
    decode: &URL_DECODE,
    pad: false,
};

impl Engine {
    /// Number of characters `encode` produces for `n` input bytes.
    pub const fn encoded_len(&self, n: usize) -> usize {
        let full = n / 3 * 4;
        match (n % 3, self.pad) {
            (0, _) => full,
            (_, true) => full + 4,
            (1, false) => full + 2,
            _ => full + 3,
        }
    }

    /// Encodes bytes into a new `String`.
    pub fn encode(&self, data: impl AsRef<[u8]>) -> String {
        let data = data.as_ref();
        let mut buf = Vec::with_capacity(self.encoded_len(data.len()));
        self.encode_into(data, &mut buf);
        // Every byte pushed by `encode_into` comes from an ASCII alphabet or is `=`.
        String::from_utf8(buf).expect("base64 output is ASCII")
    }

    /// Encodes bytes and appends the ASCII output to `out`.
    pub fn encode_into(&self, data: impl AsRef<[u8]>, out: &mut Vec<u8>) {
        let data = data.as_ref();
        let t = self.encode;
        out.reserve(self.encoded_len(data.len()));
        let (chunks, rest) = data.as_chunks::<3>();
        for c in chunks {
            let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
            out.extend_from_slice(&[
                t[(n >> 18) as usize & 63],
                t[(n >> 12) as usize & 63],
                t[(n >> 6) as usize & 63],
                t[n as usize & 63],
            ]);
        }
        match *rest {
            [a] => {
                let n = u32::from(a) << 16;
                out.extend_from_slice(&[t[(n >> 18) as usize & 63], t[(n >> 12) as usize & 63]]);
                if self.pad {
                    out.extend_from_slice(b"==");
                }
            }
            [a, b] => {
                let n = (u32::from(a) << 16) | (u32::from(b) << 8);
                out.extend_from_slice(&[
                    t[(n >> 18) as usize & 63],
                    t[(n >> 12) as usize & 63],
                    t[(n >> 6) as usize & 63],
                ]);
                if self.pad {
                    out.push(b'=');
                }
            }
            _ => {}
        }
    }

    /// Decodes Base64 into a new `Vec`.
    pub fn decode(&self, input: impl AsRef<[u8]>) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.decode_into(input, &mut out)?;
        Ok(out)
    }

    /// Decodes Base64 and appends the bytes to `out`. On error `out` may hold a partial
    /// result.
    pub fn decode_into(&self, input: impl AsRef<[u8]>, out: &mut Vec<u8>) -> Result<(), Error> {
        let input = input.as_ref();
        let mut end = input.len();
        if self.pad {
            if end % 4 != 0 {
                return Err(Error::InvalidLength { len: end });
            }
            let mut pads = 0;
            while pads < 2 && end > 0 && input[end - 1] == b'=' {
                end -= 1;
                pads += 1;
            }
        }
        let body = &input[..end];
        if body.len() % 4 == 1 {
            return Err(Error::InvalidLength { len: input.len() });
        }
        out.reserve(body.len() / 4 * 3 + 2);
        let (chunks, rest) = body.as_chunks::<4>();
        let mut base = 0;
        for c in chunks {
            // Fast path for full groups.
            let d = self.decode;
            let (a, b, c2, e) = (
                d[c[0] as usize],
                d[c[1] as usize],
                d[c[2] as usize],
                d[c[3] as usize],
            );
            if (a | b | c2 | e) & 0xc0 != 0 {
                return decode_group(self.decode, 6, c, base, out);
            }
            let n =
                (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c2) << 6) | u32::from(e);
            out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
            base += 4;
        }
        if !rest.is_empty() {
            decode_group(self.decode, 6, rest, base, out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &[(&str, &str)] = &[
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn rfc4648_vectors() {
        for &(raw, enc) in VECTORS {
            assert_eq!(STANDARD.encode(raw), enc);
            assert_eq!(STANDARD.decode(enc).unwrap(), raw.as_bytes());
            let nopad = enc.trim_end_matches('=');
            assert_eq!(STANDARD_NO_PAD.encode(raw), nopad);
            assert_eq!(STANDARD_NO_PAD.decode(nopad).unwrap(), raw.as_bytes());
            assert_eq!(STANDARD.encoded_len(raw.len()), enc.len());
            assert_eq!(STANDARD_NO_PAD.encoded_len(raw.len()), nopad.len());
        }
    }

    #[test]
    fn url_safe() {
        assert_eq!(URL_SAFE_NO_PAD.encode([0xfb, 0xff]), "-_8");
        assert_eq!(STANDARD_NO_PAD.encode([0xfb, 0xff]), "+/8");
        assert_eq!(URL_SAFE.encode([0xfb, 0xff]), "-_8=");
        assert!(URL_SAFE_NO_PAD.decode("+/8").is_err());
    }

    #[test]
    fn strictness() {
        // Padding policy.
        assert_eq!(STANDARD.decode("Zg"), Err(Error::InvalidLength { len: 2 }));
        assert_eq!(
            STANDARD_NO_PAD.decode("Zg=="),
            Err(Error::InvalidByte {
                index: 2,
                byte: b'='
            })
        );
        // Misplaced padding.
        assert_eq!(
            STANDARD.decode("Zg==Zg=="),
            Err(Error::InvalidByte {
                index: 2,
                byte: b'='
            })
        );
        assert_eq!(
            STANDARD.decode("Z==="),
            Err(Error::InvalidByte {
                index: 1,
                byte: b'='
            })
        );
        // Impossible length.
        assert_eq!(
            STANDARD_NO_PAD.decode("Zm9vY"),
            Err(Error::InvalidLength { len: 5 })
        );
        // Non-canonical trailing bits: "Zh" decodes to 'f' with leftover bits.
        assert_eq!(
            STANDARD_NO_PAD.decode("Zh"),
            Err(Error::TrailingBits { index: 1 })
        );
        // Bad characters.
        assert_eq!(
            STANDARD.decode("Zm9v\nYmFy"),
            Err(Error::InvalidLength { len: 9 })
        );
        assert_eq!(
            STANDARD.decode("Zm*v"),
            Err(Error::InvalidByte {
                index: 2,
                byte: b'*'
            })
        );
        assert_eq!(
            STANDARD.decode("AAAAZm*v"),
            Err(Error::InvalidByte {
                index: 6,
                byte: b'*'
            })
        );
    }

    #[test]
    fn roundtrip_lengths() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        for n in 0..data.len() {
            for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
                let enc = engine.encode(&data[..n]);
                assert_eq!(enc.len(), engine.encoded_len(n));
                assert_eq!(engine.decode(&enc).unwrap(), &data[..n]);
            }
        }
    }
}
