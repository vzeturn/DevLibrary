use core::fmt;

/// Error returned by parsing and by the fallible generators.
///
/// Parse errors carry the byte offset of the problem in the input, so a caller can point
/// at it:
///
/// ```
/// use dl_uuid::{Error, Uuid};
/// assert_eq!(Uuid::parse("67e55044-10b1-426f-9247-bb680e5fe0cg"), Err(Error::InvalidHexDigit { offset: 35 }));
/// assert_eq!(Uuid::parse("67e55044_10b1-426f-9247-bb680e5fe0c8"), Err(Error::ExpectedChar { offset: 8, expected: '-' }));
/// assert_eq!(Uuid::parse("67e55044"), Err(Error::InvalidLength { len: 8 }));
/// assert_eq!(
///     Error::InvalidHexDigit { offset: 35 }.to_string(),
///     "invalid UUID: expected a hex digit at offset 35",
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Error {
    /// The input length (in bytes) matches none of the accepted forms: 32 (simple),
    /// 36 (hyphenated), 38 (braced) or 45 (URN).
    InvalidLength {
        /// Length of the input in bytes.
        len: usize,
    },
    /// A byte that is not `0-9`, `a-f` or `A-F` where a hex digit was expected.
    InvalidHexDigit {
        /// Byte offset in the input.
        offset: usize,
    },
    /// A byte other than the required separator or framing character: `-` in the
    /// hyphenated form, `{` / `}` around the braced form, or a letter of `urn:uuid:`.
    ExpectedChar {
        /// Byte offset in the input.
        offset: usize,
        /// The character required at that offset (lowercase for `urn:uuid:`, which
        /// is matched case-insensitively).
        expected: char,
    },
    /// The operating system's random number generator failed.
    RandomSource {
        /// The OS error code (`errno` on Unix), or `None` if the target has no
        /// entropy source.
        os_code: Option<i32>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::InvalidLength { len } => write!(
                f,
                "invalid UUID length {len}: expected 32, 36, 38 or 45 characters"
            ),
            Error::InvalidHexDigit { offset } => {
                write!(f, "invalid UUID: expected a hex digit at offset {offset}")
            }
            Error::ExpectedChar { offset, expected } => {
                write!(f, "invalid UUID: expected '{expected}' at offset {offset}")
            }
            Error::RandomSource {
                os_code: Some(code),
            } => {
                write!(f, "OS random number generator failed (code {code})")
            }
            Error::RandomSource { os_code: None } => {
                f.write_str("no OS random number generator on this target")
            }
        }
    }
}

impl core::error::Error for Error {}

#[cfg(feature = "std")]
impl Error {
    /// Converts a `dl-rand` failure without exposing `dl_rand::Error` in the public API.
    pub(crate) fn from_rand(e: dl_rand::Error) -> Error {
        match e {
            dl_rand::Error::Os(code) => Error::RandomSource {
                os_code: Some(code),
            },
            dl_rand::Error::Unsupported => Error::RandomSource { os_code: None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn display_messages() {
        let cases = [
            (
                Error::InvalidLength { len: 3 },
                "invalid UUID length 3: expected 32, 36, 38 or 45 characters",
            ),
            (
                Error::InvalidHexDigit { offset: 0 },
                "invalid UUID: expected a hex digit at offset 0",
            ),
            (
                Error::ExpectedChar {
                    offset: 37,
                    expected: '}',
                },
                "invalid UUID: expected '}' at offset 37",
            ),
            (
                Error::RandomSource { os_code: Some(5) },
                "OS random number generator failed (code 5)",
            ),
            (
                Error::RandomSource { os_code: None },
                "no OS random number generator on this target",
            ),
        ];
        for (err, want) in cases {
            assert_eq!(err.to_string(), want);
            let _: &dyn core::error::Error = &err;
        }
    }

    #[cfg(feature = "std")]
    #[test]
    fn from_rand() {
        assert_eq!(
            Error::from_rand(dl_rand::Error::Os(-1)),
            Error::RandomSource { os_code: Some(-1) }
        );
        assert_eq!(
            Error::from_rand(dl_rand::Error::Unsupported),
            Error::RandomSource { os_code: None }
        );
    }
}
