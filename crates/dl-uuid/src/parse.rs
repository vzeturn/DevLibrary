//! Parsing of the four text forms. Every function is `const` and never panics: each
//! index is checked against the input length before use.

use crate::{Error, Uuid};

/// Hex digit value for every byte, `INVALID` for non-hex bytes.
const HEX_VALUE: [u8; 256] = {
    let mut t = [INVALID; 256];
    let mut i = 0;
    while i < 10 {
        t[b'0' as usize + i] = i as u8;
        i += 1;
    }
    let mut i = 0;
    while i < 6 {
        t[b'a' as usize + i] = 10 + i as u8;
        t[b'A' as usize + i] = 10 + i as u8;
        i += 1;
    }
    t
};
const INVALID: u8 = 0xFF;

const URN_PREFIX: &[u8; 9] = b"urn:uuid:";

impl Uuid {
    /// Parses a UUID from text. Accepted forms (hex digits in any case):
    ///
    /// | Form | Example | Length |
    /// |---|---|---|
    /// | hyphenated | `67e55044-10b1-426f-9247-bb680e5fe0c8` | 36 |
    /// | simple | `67e5504410b1426f9247bb680e5fe0c8` | 32 |
    /// | braced | `{67e55044-10b1-426f-9247-bb680e5fe0c8}` | 38 |
    /// | URN | `urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8` | 45 |
    ///
    /// The `urn:uuid:` prefix is case-insensitive (RFC 8141). Nothing else is accepted:
    /// no surrounding whitespace, no hyphens in other places. Trim user input first.
    ///
    /// This is a `const fn`, so it can define constants:
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// const TENANT: Uuid = match Uuid::parse("67e55044-10b1-426f-9247-bb680e5fe0c8") {
    ///     Ok(id) => id,
    ///     Err(_) => panic!("invalid UUID literal"),
    /// };
    /// assert_eq!(TENANT, Uuid::parse("{67E55044-10B1-426F-9247-BB680E5FE0C8}").unwrap());
    /// ```
    ///
    /// # Errors
    /// [`Error::InvalidLength`], [`Error::InvalidHexDigit`] or [`Error::ExpectedChar`],
    /// with the byte offset of the first problem.
    pub const fn parse(s: &str) -> Result<Uuid, Error> {
        Uuid::parse_bytes(s.as_bytes())
    }

    /// Parses a UUID from ASCII bytes, for input that is not known to be UTF-8 (network
    /// buffers, file contents). Same rules and errors as [`parse`](Self::parse).
    ///
    /// ```
    /// use dl_uuid::Uuid;
    /// let id = Uuid::parse_bytes(b"67e5504410b1426f9247bb680e5fe0c8").unwrap();
    /// assert_eq!(id.to_string(), "67e55044-10b1-426f-9247-bb680e5fe0c8");
    /// ```
    ///
    /// # Errors
    /// Same as [`parse`](Self::parse).
    pub const fn parse_bytes(input: &[u8]) -> Result<Uuid, Error> {
        match input.len() {
            32 => parse_simple(input),
            36 => parse_hyphenated(input, 0),
            38 => {
                if input[0] != b'{' {
                    return Err(Error::ExpectedChar {
                        offset: 0,
                        expected: '{',
                    });
                }
                match parse_hyphenated(input, 1) {
                    Ok(_) if input[37] != b'}' => Err(Error::ExpectedChar {
                        offset: 37,
                        expected: '}',
                    }),
                    result => result,
                }
            }
            45 => {
                let mut i = 0;
                while i < URN_PREFIX.len() {
                    let want = URN_PREFIX[i];
                    // ASCII lowercase: setting bit 0x20 maps 'A'..='Z' to 'a'..='z' and
                    // leaves ':' unchanged; it could map other bytes onto a letter, so
                    // only apply it to letters.
                    let got = if input[i].is_ascii_uppercase() {
                        input[i] | 0x20
                    } else {
                        input[i]
                    };
                    if got != want {
                        return Err(Error::ExpectedChar {
                            offset: i,
                            expected: want as char,
                        });
                    }
                    i += 1;
                }
                parse_hyphenated(input, URN_PREFIX.len())
            }
            len => Err(Error::InvalidLength { len }),
        }
    }
}

/// Decodes the hex digits at `input[at]` and `input[at + 1]`.
/// Caller guarantees `at + 1 < input.len()`.
const fn hex_pair(input: &[u8], at: usize) -> Result<u8, Error> {
    let hi = HEX_VALUE[input[at] as usize];
    if hi == INVALID {
        return Err(Error::InvalidHexDigit { offset: at });
    }
    let lo = HEX_VALUE[input[at + 1] as usize];
    if lo == INVALID {
        return Err(Error::InvalidHexDigit { offset: at + 1 });
    }
    Ok(hi << 4 | lo)
}

/// Offsets of the 16 hex digit pairs in the simple form.
const SIMPLE_PAIRS: [usize; 16] = [0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30];
/// Offsets of the 16 hex digit pairs in the hyphenated form.
const HYPHENATED_PAIRS: [usize; 16] = [0, 2, 4, 6, 9, 11, 14, 16, 19, 21, 24, 26, 28, 30, 32, 34];

/// Fast path: decodes all 16 pairs without branching per digit and reports only
/// whether every digit was valid. Every entry of `pairs` is at most `N - 2`, so with a
/// fixed-size input the compiler removes the bounds checks.
const fn decode_pairs<const N: usize>(input: &[u8; N], pairs: &[usize; 16]) -> Option<[u8; 16]> {
    let mut out = [0u8; 16];
    let mut seen = 0u8;
    let mut i = 0;
    while i < 16 {
        let p = pairs[i];
        let hi = HEX_VALUE[input[p] as usize];
        let lo = HEX_VALUE[input[p + 1] as usize];
        // Valid digits are 0..=15; INVALID (0xFF) sets the high nibble.
        seen |= hi | lo;
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    if seen & 0xF0 == 0 { Some(out) } else { None }
}

/// 32 hex digits. Caller guarantees `input.len() == 32`.
const fn parse_simple(input: &[u8]) -> Result<Uuid, Error> {
    if let Some(digits) = input.first_chunk::<32>()
        && let Some(bytes) = decode_pairs(digits, &SIMPLE_PAIRS)
    {
        return Ok(Uuid::from_bytes(bytes));
    }
    simple_slow(input)
}

/// The 36-character `8-4-4-4-12` form starting at `input[start]`.
/// Caller guarantees `start + 36 <= input.len()`.
const fn parse_hyphenated(input: &[u8], start: usize) -> Result<Uuid, Error> {
    if let Some((_, rest)) = input.split_at_checked(start)
        && let Some(text) = rest.first_chunk::<36>()
        && text[8] == b'-'
        && text[13] == b'-'
        && text[18] == b'-'
        && text[23] == b'-'
        && let Some(bytes) = decode_pairs(text, &HYPHENATED_PAIRS)
    {
        return Ok(Uuid::from_bytes(bytes));
    }
    hyphenated_slow(input, start)
}

/// Digit-by-digit decoding that finds the offset of the first invalid byte. Used only
/// when the fast path fails. Caller guarantees `input.len() == 32`.
const fn simple_slow(input: &[u8]) -> Result<Uuid, Error> {
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = match hex_pair(input, 2 * i) {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        i += 1;
    }
    Ok(Uuid::from_bytes(out))
}

/// Slow path of [`parse_hyphenated`], checking left to right so the error points at the
/// first bad byte. Caller guarantees `start + 36 <= input.len()`.
const fn hyphenated_slow(input: &[u8], start: usize) -> Result<Uuid, Error> {
    let mut out = [0u8; 16];
    let mut o = 0;
    let mut i = start;
    let end = start + 36;
    while i < end {
        let rel = i - start;
        if rel == 8 || rel == 13 || rel == 18 || rel == 23 {
            if input[i] != b'-' {
                return Err(Error::ExpectedChar {
                    offset: i,
                    expected: '-',
                });
            }
            i += 1;
            continue;
        }
        // Groups have even lengths, so a pair never straddles a hyphen.
        out[o] = match hex_pair(input, i) {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        o += 1;
        i += 2;
    }
    Ok(Uuid::from_bytes(out))
}

impl core::str::FromStr for Uuid {
    type Err = Error;

    /// Same as [`Uuid::parse`].
    fn from_str(s: &str) -> Result<Uuid, Error> {
        Uuid::parse(s)
    }
}

impl TryFrom<&str> for Uuid {
    type Error = Error;

    /// Same as [`Uuid::parse`].
    fn try_from(s: &str) -> Result<Uuid, Error> {
        Uuid::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::String;

    const ID: u128 = 0x67e55044_10b1_426f_9247_bb680e5fe0c8;

    /// Inputs Python's `uuid.UUID(s)` also accepts (canonical value printed by
    /// `testdata/gen_vectors.py`), plus the uppercase URN prefix.
    #[test]
    fn valid_forms() {
        let valid = [
            "67e55044-10b1-426f-9247-bb680e5fe0c8",
            "67E55044-10B1-426F-9247-BB680E5FE0C8",
            "67e5504410b1426f9247bb680e5fe0c8",
            "67E5504410B1426F9247BB680E5FE0C8",
            "{67e55044-10b1-426f-9247-bb680e5fe0c8}",
            "{67E55044-10b1-426F-9247-bb680E5fe0C8}",
            "urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
            "urn:uuid:67E55044-10B1-426F-9247-BB680E5FE0C8",
            "URN:UUID:67e55044-10b1-426f-9247-bb680e5fe0c8",
            "Urn:Uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
        ];
        for s in valid {
            assert_eq!(Uuid::parse(s), Ok(Uuid::from_u128(ID)), "{s}");
            assert_eq!(s.parse::<Uuid>(), Ok(Uuid::from_u128(ID)), "{s}");
            assert_eq!(Uuid::try_from(s), Ok(Uuid::from_u128(ID)), "{s}");
            assert_eq!(
                Uuid::parse_bytes(s.as_bytes()),
                Ok(Uuid::from_u128(ID)),
                "{s}"
            );
        }
        let edge = [
            ("00000000-0000-0000-0000-000000000000", Uuid::NIL),
            ("ffffffff-ffff-ffff-ffff-ffffffffffff", Uuid::MAX),
            ("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF", Uuid::MAX),
            ("{00000000-0000-0000-0000-000000000000}", Uuid::NIL),
            (
                "0123456789abcdefABCDEF0123456789",
                Uuid::from_u128(0x0123456789abcdefabcdef0123456789),
            ),
        ];
        for (s, want) in edge {
            assert_eq!(Uuid::parse(s), Ok(want), "{s}");
        }
    }

    #[test]
    fn invalid_inputs() {
        use Error::*;
        let h = |offset| ExpectedChar {
            offset,
            expected: '-',
        };
        let x = |offset| InvalidHexDigit { offset };
        let invalid: &[(&str, Error)] = &[
            ("", InvalidLength { len: 0 }),
            ("67e55044", InvalidLength { len: 8 }),
            (
                "67e55044-10b1-426f-9247-bb680e5fe0c",
                InvalidLength { len: 35 },
            ),
            (
                "67e55044-10b1-426f-9247-bb680e5fe0c8a",
                InvalidLength { len: 37 },
            ),
            ("67e5504410b1426f9247bb680e5fe0c", InvalidLength { len: 31 }),
            (
                "67e5504410b1426f9247bb680e5fe0c8a",
                InvalidLength { len: 33 },
            ),
            (
                " 67e55044-10b1-426f-9247-bb680e5fe0c8",
                InvalidLength { len: 37 },
            ),
            (
                "67e55044-10b1-426f-9247-bb680e5fe0c8\n",
                InvalidLength { len: 37 },
            ),
            (
                "{67e5504410b1426f9247bb680e5fe0c8}",
                InvalidLength { len: 34 },
            ),
            (
                "(67e55044-10b1-426f-9247-bb680e5fe0c8)",
                ExpectedChar {
                    offset: 0,
                    expected: '{',
                },
            ),
            (
                "{67e55044-10b1-426f-9247-bb680e5fe0c8)",
                ExpectedChar {
                    offset: 37,
                    expected: '}',
                },
            ),
            (
                "{67e55044-10b1-426f-9247-bb680e5fe0c8{",
                ExpectedChar {
                    offset: 37,
                    expected: '}',
                },
            ),
            ("{67e55044-10b1-426f-9247-bb680e5fe0cx}", x(36)),
            (
                "-67e55044-10b1-426f-9247-bb680e5fe0c8-",
                ExpectedChar {
                    offset: 0,
                    expected: '{',
                },
            ),
            (
                "urn:uuid:67e5504410b1426f9247bb680e5fe0c8",
                InvalidLength { len: 41 },
            ),
            (
                "uri:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
                ExpectedChar {
                    offset: 2,
                    expected: 'n',
                },
            ),
            (
                "urn:guid:67e55044-10b1-426f-9247-bb680e5fe0c8",
                ExpectedChar {
                    offset: 4,
                    expected: 'u',
                },
            ),
            (
                "urn-uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
                ExpectedChar {
                    offset: 3,
                    expected: ':',
                },
            ),
            (
                "urn:uuid 67e55044-10b1-426f-9247-bb680e5fe0c8",
                ExpectedChar {
                    offset: 8,
                    expected: ':',
                },
            ),
            ("urn:uuid:67e55044-10b1-426f-9247_bb680e5fe0c8", h(32)),
            ("urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0cZ", x(44)),
            ("g7e55044-10b1-426f-9247-bb680e5fe0c8", x(0)),
            ("67e55044-10b1-426f-9247-bb680e5fe0cg", x(35)),
            ("67e55044-10b1-426f-9247-bb680e5fe0 8", x(34)),
            ("67e55044 10b1-426f-9247-bb680e5fe0c8", h(8)),
            ("67e55044-10b1_426f-9247-bb680e5fe0c8", h(13)),
            ("67e55044-10b1-426f+9247-bb680e5fe0c8", h(18)),
            ("67e55044-10b1-426f-9247:bb680e5fe0c8", h(23)),
            ("67e5504-410b1-426f-9247-bb680e5fe0c8", x(7)),
            ("67e550441-0b1-426f-9247-bb680e5fe0c8", h(8)),
            ("67e55044-10b1-426f-92470bb680e5fe0c8", h(23)),
            ("67e55044-10b1-426f-9247-bb68-e5fe0c8", x(28)),
            ("------------------------------------", x(0)),
            ("67e5504410b1426f9247bb680e5fe0c-", x(31)),
            (
                "0x67e5504410b1426f9247bb680e5fe0c8",
                InvalidLength { len: 34 },
            ),
            ("67e5504410b1426f9247bb680e5fe0cG", x(31)),
            // Non-ASCII: 'é' is two bytes, so this is 36 bytes long; the offset is a byte offset.
            ("67e55044-10b1-426f-9247-bb680e5fe0é", x(34)),
            ("６７e55044-10b1-426f-9247-bb680e5f", x(0)),
        ];
        assert!(invalid.len() >= 30);
        for (s, want) in invalid {
            assert_eq!(Uuid::parse(s), Err(*want), "{s:?}");
            assert_eq!(s.parse::<Uuid>(), Err(*want), "{s:?}");
        }
    }

    #[test]
    fn invalid_bytes_never_panic() {
        // Every byte value in every position of each accepted length.
        let base: [&[u8]; 4] = [
            b"67e5504410b1426f9247bb680e5fe0c8",
            b"67e55044-10b1-426f-9247-bb680e5fe0c8",
            b"{67e55044-10b1-426f-9247-bb680e5fe0c8}",
            b"urn:uuid:67e55044-10b1-426f-9247-bb680e5fe0c8",
        ];
        for s in base {
            let mut buf = alloc::vec::Vec::from(s);
            for pos in 0..buf.len() {
                let orig = buf[pos];
                for b in 0..=255u8 {
                    buf[pos] = b;
                    match Uuid::parse_bytes(&buf) {
                        Ok(id) if b.eq_ignore_ascii_case(&orig) => {
                            assert_eq!(id, Uuid::from_u128(ID));
                        }
                        Ok(_) => {
                            // Otherwise only a hex digit replaced by another one parses.
                            assert!(b.is_ascii_hexdigit() && orig.is_ascii_hexdigit());
                        }
                        Err(
                            Error::InvalidHexDigit { offset } | Error::ExpectedChar { offset, .. },
                        ) => {
                            assert_eq!(offset, pos)
                        }
                        Err(e) => panic!("unexpected {e:?}"),
                    }
                }
                buf[pos] = orig;
            }
        }
        for len in 0..64 {
            let junk: String = core::iter::repeat_n('z', len).collect();
            assert!(Uuid::parse(&junk).is_err());
        }
    }

    #[test]
    fn round_trip_every_form() {
        let id = Uuid::from_u128(ID);
        for s in [
            format!("{}", id.hyphenated()),
            format!("{}", id.simple()),
            format!("{}", id.braced()),
            format!("{}", id.urn()),
            format!("{:X}", id.hyphenated()),
            format!("{:X}", id.simple()),
            format!("{:X}", id.braced()),
            format!("{:X}", id.urn()),
        ] {
            assert_eq!(Uuid::parse(&s), Ok(id), "{s}");
        }
    }
}
