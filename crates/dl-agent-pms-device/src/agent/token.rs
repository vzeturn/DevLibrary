//! `Agent/TokenFile.cs`: the per-agent secret, read once from a file protected by an ACL
//! (docs/runbooks/device-agent-install.md). Error messages name the path, never the content.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use crate::config::DotnetError;

/// Why the token could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenFileError {
    /// The file or its directory does not exist.
    NotFound {
        /// The configured path.
        path: String,
    },
    /// The trimmed token is shorter than [`TokenFile::MIN_LENGTH`].
    TooShort {
        /// The configured path.
        path: String,
    },
    /// Any other I/O failure (access denied, sharing violation).
    Io {
        /// The configured path.
        path: String,
        /// The OS error text.
        message: String,
        /// `UnauthorizedAccessException` instead of `IOException`.
        denied: bool,
    },
}

impl TokenFileError {
    /// The exception `TokenFile.ReadAsync` throws.
    pub fn to_dotnet(&self) -> DotnetError {
        match self {
            TokenFileError::NotFound { .. } | TokenFileError::TooShort { .. } => {
                DotnetError::new("System.InvalidOperationException", self.to_string())
            }
            TokenFileError::Io { denied: true, .. } => {
                DotnetError::new("System.UnauthorizedAccessException", self.to_string())
            }
            TokenFileError::Io { .. } => {
                DotnetError::new("System.IO.IOException", self.to_string())
            }
        }
    }
}

impl fmt::Display for TokenFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenFileError::NotFound { path } => {
                write!(f, "Device agent token file '{path}' was not found")
            }
            TokenFileError::TooShort { path } => write!(
                f,
                "Device agent token in '{path}' is shorter than {} characters; issue a new one",
                TokenFile::MIN_LENGTH
            ),
            TokenFileError::Io { path, message, .. } => {
                write!(f, "Device agent token file '{path}': {message}")
            }
        }
    }
}

impl std::error::Error for TokenFileError {}

/// The agent token, read on first use and then cached.
#[derive(Debug)]
pub struct TokenFile {
    path: String,
    token: Mutex<Option<String>>,
}

impl TokenFile {
    /// `TokenFile.MinLength`, in UTF-16 code units like C# `string.Length`.
    pub const MIN_LENGTH: usize = 32;

    /// The token at `path` (not read yet).
    pub fn new(path: impl Into<String>) -> TokenFile {
        TokenFile {
            path: path.into(),
            token: Mutex::new(None),
        }
    }

    /// The configured path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The trimmed token. The file is read once; a failure is retried on the next call.
    pub fn read(&self) -> Result<String, TokenFileError> {
        let mut cached = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = cached.as_ref() {
            return Ok(t.clone());
        }
        let bytes = std::fs::read(&self.path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => TokenFileError::NotFound {
                path: self.path.clone(),
            },
            kind => TokenFileError::Io {
                path: self.path.clone(),
                message: e.to_string(),
                denied: kind == std::io::ErrorKind::PermissionDenied,
            },
        })?;
        let text = decode_text(&bytes);
        let token = text.trim_matches(char::is_whitespace);
        if token.encode_utf16().count() < Self::MIN_LENGTH {
            return Err(TokenFileError::TooShort {
                path: self.path.clone(),
            });
        }
        *cached = Some(token.to_owned());
        Ok(token.to_owned())
    }
}

/// `File.ReadAllText`: UTF-8 unless a BOM says UTF-16; invalid sequences become U+FFFD.
fn decode_text(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], le: bool| {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| match (c, le) {
                ([a, b], true) => u16::from_le_bytes([*a, *b]),
                ([a, b], false) => u16::from_be_bytes([*a, *b]),
                _ => 0xFFFD,
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boms_are_honoured() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(decode_text(&[0xFF, 0xFE, b'a', 0, b'b', 0]), "ab");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0, b'a']), "a");
        assert_eq!(decode_text(b"a\xFFb"), "a\u{FFFD}b");
    }
}
