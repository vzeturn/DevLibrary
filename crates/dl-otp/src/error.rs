use core::fmt;

/// Errors returned by this crate.
///
/// Byte offsets (`index`) refer to the string passed to [`OtpAuthUri::parse`].
/// The enum is `#[non_exhaustive]` because the `std` feature adds `Error::Random`.
///
/// [`OtpAuthUri::parse`]: crate::OtpAuthUri::parse
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The number of code digits is outside `6..=10`.
    InvalidDigits(u8),
    /// The TOTP period is zero.
    InvalidPeriod,
    /// The secret is shorter than the minimum: 1 byte for an existing secret, 16 bytes
    /// (128 bits, RFC 4226 section 4) for a newly generated one.
    SecretTooShort {
        /// Length that was supplied or requested, in bytes.
        len: usize,
        /// Minimum accepted length, in bytes.
        min: usize,
    },
    /// The Base32 secret is malformed. Offsets inside refer to the secret with spaces and
    /// dashes removed.
    InvalidSecret(dl_encoding::Error),
    /// An algorithm name other than `SHA1`, `SHA256` or `SHA512`.
    UnknownAlgorithm,
    /// The issuer or account name cannot be put in an otpauth label: the account name is
    /// empty or starts with a space (parsers drop leading spaces), or one of them contains
    /// `:` (the label separator).
    InvalidLabel,
    /// The TOTP uses `t0 != 0`, which the Key Uri Format cannot express.
    NonZeroT0,
    /// The URI does not start with `otpauth://`.
    InvalidScheme,
    /// The OTP type is not `totp` or `hotp`.
    InvalidType,
    /// The label has no account name.
    MissingAccount,
    /// The `secret` parameter is missing.
    MissingSecret,
    /// The `counter` parameter is missing from an `hotp` URI.
    MissingCounter,
    /// A `%` that is not followed by two hexadecimal digits.
    InvalidPercentEncoding {
        /// Offset of the `%`.
        index: usize,
    },
    /// A label or parameter that is not valid UTF-8 after percent-decoding.
    InvalidUtf8 {
        /// Offset of the start of the label or parameter value.
        index: usize,
    },
    /// A parameter that occurs more than once.
    DuplicateParameter {
        /// Parameter name.
        name: &'static str,
        /// Offset of the second occurrence.
        index: usize,
    },
    /// A parameter with an invalid value (unknown algorithm, digits outside `6..=10`,
    /// zero period, malformed number).
    InvalidParameter {
        /// Parameter name.
        name: &'static str,
        /// Offset of the value.
        index: usize,
    },
    /// The operation needs the other OTP type (for example `to_totp` on an `hotp` URI).
    WrongKind,
    /// Recovery code shape out of range: `groups` and `group_len` must be at least 1, and
    /// a code must have between 8 and 128 characters.
    InvalidRecoveryFormat,
    /// The operating system's random number generator failed.
    #[cfg(feature = "std")]
    Random(dl_rand::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::InvalidDigits(d) => write!(f, "invalid number of digits {d} (allowed 6..=10)"),
            Error::InvalidPeriod => f.write_str("TOTP period must be at least 1 second"),
            Error::SecretTooShort { len, min } => {
                write!(f, "secret of {len} bytes is too short (minimum {min})")
            }
            Error::InvalidSecret(e) => write!(f, "invalid Base32 secret: {e}"),
            Error::UnknownAlgorithm => {
                f.write_str("unknown algorithm (expected SHA1, SHA256 or SHA512)")
            }
            Error::InvalidLabel => f.write_str(
                "invalid otpauth label: account name must be non-empty and not start with \
                 a space, and neither issuer nor account may contain ':'",
            ),
            Error::NonZeroT0 => f.write_str("otpauth URIs cannot express a TOTP with t0 != 0"),
            Error::InvalidScheme => f.write_str("URI does not start with otpauth://"),
            Error::InvalidType => f.write_str("OTP type must be totp or hotp"),
            Error::MissingAccount => f.write_str("otpauth label has no account name"),
            Error::MissingSecret => f.write_str("otpauth URI has no secret parameter"),
            Error::MissingCounter => f.write_str("hotp URI has no counter parameter"),
            Error::InvalidPercentEncoding { index } => {
                write!(f, "invalid percent-encoding at offset {index}")
            }
            Error::InvalidUtf8 { index } => {
                write!(f, "component at offset {index} is not valid UTF-8")
            }
            Error::DuplicateParameter { name, index } => {
                write!(f, "duplicate parameter {name} at offset {index}")
            }
            Error::InvalidParameter { name, index } => {
                write!(f, "invalid value for parameter {name} at offset {index}")
            }
            Error::WrongKind => f.write_str("wrong OTP type for this operation"),
            Error::InvalidRecoveryFormat => f.write_str(
                "invalid recovery code format: need groups >= 1, group_len >= 1 and \
                 8..=128 characters per code",
            ),
            #[cfg(feature = "std")]
            Error::Random(e) => write!(f, "random number generator failed: {e}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::InvalidSecret(e) => Some(e),
            #[cfg(feature = "std")]
            Error::Random(e) => Some(e),
            _ => None,
        }
    }
}

impl From<dl_encoding::Error> for Error {
    fn from(e: dl_encoding::Error) -> Self {
        Error::InvalidSecret(e)
    }
}

#[cfg(feature = "std")]
impl From<dl_rand::Error> for Error {
    fn from(e: dl_rand::Error) -> Self {
        Error::Random(e)
    }
}
