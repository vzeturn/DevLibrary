//! The crate error type.

use alloc::string::String;
use core::fmt;

/// One of the three dot-separated parts of a compact JWS token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// The JOSE header (first part).
    Header,
    /// The claims set (second part).
    Payload,
    /// The signature (third part).
    Signature,
}

impl Part {
    /// Lower-case name of the part: `"header"`, `"payload"` or `"signature"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Part::Header => "header",
            Part::Payload => "payload",
            Part::Signature => "signature",
        }
    }
}

impl fmt::Display for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Everything that can go wrong when creating keys, encoding or decoding tokens.
///
/// The variants are grouped by the decoding step that produces them, in the order the
/// steps run: size and structure, encoding, header, signature, then claims. A token that
/// fails the signature check never reaches the claim checks, so `Expired` or
/// `InvalidIssuer` always describe a token that was signed with your key.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The token is longer than [`Validation::max_token_len`](crate::Validation::max_token_len).
    TokenTooLarge,
    /// The token is not `header.payload.signature`. `part` is the first part that is
    /// missing or empty; [`Part::Signature`] also reports extra `.`-separated parts.
    Malformed {
        /// The offending part.
        part: Part,
    },
    /// A part is not unpadded, canonical base64url (RFC 7515 section 2).
    InvalidBase64 {
        /// The offending part.
        part: Part,
    },
    /// A part is not valid UTF-8 JSON, is not a JSON object, or a member has the wrong
    /// type (for example a string `exp`, or a field your claims type rejects).
    InvalidJson {
        /// The offending part.
        part: Part,
        /// The underlying parse or conversion error; its
        /// [`path`](dl_json::Error::path) points at the bad member.
        error: dl_json::Error,
    },
    /// The header names an algorithm this crate does not implement, including `none`
    /// and the asymmetric algorithms (`RS256`, `ES256`, `EdDSA`, ...).
    UnsupportedAlgorithm(String),
    /// The header lists a critical extension (`crit`, RFC 7515 section 4.1.11) that
    /// this crate does not understand. The value is the first such name.
    UnsupportedCriticalHeader(String),
    /// The header algorithm is supported but not in
    /// [`Validation::algorithms`](crate::Validation::algorithms).
    AlgorithmNotAllowed,
    /// No key matches the token: the key selector returned `None`, or the token has no
    /// `kid` or an unknown one for a [`KeySet`](crate::KeySet).
    UnknownKey,
    /// The signature does not match the header and payload under the given key.
    InvalidSignature,
    /// A claim listed in [`Validation::required_claims`](crate::Validation::required_claims),
    /// or needed by another check (`iss`, `aud`, `sub`, `iat`), is missing.
    MissingClaim(String),
    /// `exp` is in the past (beyond the leeway).
    Expired,
    /// `nbf` is in the future (beyond the leeway), or `iat` is when `max_age` is set.
    NotYetValid,
    /// `iat` is older than [`Validation::max_age`](crate::Validation::max_age) allows.
    TooOld,
    /// `iss` is not one of the accepted issuers.
    InvalidIssuer,
    /// No `aud` value is one of the accepted audiences, or the token has an `aud` and
    /// no audience was configured.
    InvalidAudience,
    /// `sub` is not the expected subject.
    InvalidSubject,
    /// The HMAC key is shorter than the hash output of the algorithm (RFC 7518 section
    /// 3.2): 32 bytes for HS256, 48 for HS384, 64 for HS512.
    KeyTooShort {
        /// Length of the key in bytes.
        len: usize,
        /// Minimum length required in bytes.
        min: usize,
    },
    /// [`Key::from_base64url`](crate::Key::from_base64url) got text that is not
    /// unpadded base64url.
    InvalidKeyEncoding,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TokenTooLarge => f.write_str("token exceeds the maximum length"),
            Error::Malformed { part } => {
                write!(
                    f,
                    "malformed token: expected header.payload.signature, bad {part}"
                )
            }
            Error::InvalidBase64 { part } => write!(f, "{part} is not valid base64url"),
            Error::InvalidJson { part, error } => write!(f, "invalid {part} JSON: {error}"),
            Error::UnsupportedAlgorithm(alg) => write!(f, "unsupported algorithm {alg:?}"),
            Error::UnsupportedCriticalHeader(name) => {
                write!(f, "unsupported critical header parameter {name:?}")
            }
            Error::AlgorithmNotAllowed => f.write_str("algorithm not allowed"),
            Error::UnknownKey => f.write_str("no key found for this token"),
            Error::InvalidSignature => f.write_str("invalid signature"),
            Error::MissingClaim(name) => write!(f, "missing required claim {name:?}"),
            Error::Expired => f.write_str("token has expired"),
            Error::NotYetValid => f.write_str("token is not valid yet"),
            Error::TooOld => f.write_str("token is too old"),
            Error::InvalidIssuer => f.write_str("invalid issuer"),
            Error::InvalidAudience => f.write_str("invalid audience"),
            Error::InvalidSubject => f.write_str("invalid subject"),
            Error::KeyTooShort { len, min } => {
                write!(f, "HMAC key is {len} bytes, at least {min} are required")
            }
            Error::InvalidKeyEncoding => f.write_str("key is not valid base64url"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::InvalidJson { error, .. } => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn display() {
        assert_eq!(Error::Expired.to_string(), "token has expired");
        assert_eq!(
            Error::Malformed {
                part: Part::Signature
            }
            .to_string(),
            "malformed token: expected header.payload.signature, bad signature"
        );
        assert_eq!(
            Error::UnsupportedAlgorithm("none".into()).to_string(),
            "unsupported algorithm \"none\""
        );
        assert_eq!(
            Error::KeyTooShort { len: 20, min: 32 }.to_string(),
            "HMAC key is 20 bytes, at least 32 are required"
        );
        let json = dl_json::parse("{").unwrap_err();
        let err = Error::InvalidJson {
            part: Part::Header,
            error: json,
        };
        assert!(err.to_string().starts_with("invalid header JSON: "));
        assert!(core::error::Error::source(&err).is_some());
        assert!(core::error::Error::source(&Error::Expired).is_none());
    }
}
