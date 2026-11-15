//! The crate error type.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use core::fmt;

/// What went wrong. Syntax kinds come from the parser; `Type`, `MissingField` and
/// `Custom` come from [`FromJson`](crate::FromJson) conversions.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The input ended in the middle of a value (or was empty).
    UnexpectedEof,
    /// A character that cannot appear at this point of the grammar.
    UnexpectedChar(char),
    /// A malformed number (`01`, `1.`, `-`, `1e`) or a float too large for `f64`.
    InvalidNumber,
    /// A backslash followed by a character that is not a JSON escape.
    InvalidEscape,
    /// A malformed `\uXXXX` escape: bad hex digits or an unpaired surrogate.
    InvalidUnicode,
    /// An unescaped control character (U+0000 to U+001F) inside a string.
    ControlCharInString,
    /// Non-whitespace input after the end of the top-level value.
    TrailingCharacters,
    /// Arrays and objects are nested deeper than
    /// [`ParseOptions::max_depth`](crate::ParseOptions::max_depth).
    DepthLimitExceeded,
    /// The byte input of [`parse_slice`](crate::parse_slice) is not valid UTF-8.
    InvalidUtf8,
    /// A conversion found a value of the wrong JSON type or out of range.
    Type {
        /// What the conversion wanted, e.g. `"u32"` or `"string"`.
        expected: &'static str,
        /// What it found, e.g. `"null"` or `"out-of-range integer"`.
        found: &'static str,
    },
    /// A required object member is missing.
    MissingField(String),
    /// A user-defined conversion error, see [`Error::custom`].
    Custom(String),
}

/// Location of a syntax error in the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Position {
    /// Byte offset from the start of the input (a skipped BOM counts as 3 bytes).
    pub offset: usize,
    /// Line number, starting at 1. Only `\n` starts a new line.
    pub line: usize,
    /// Column number in characters (not bytes), starting at 1.
    pub column: usize,
}

/// Error returned by parsing and by [`FromJson`](crate::FromJson) conversions.
///
/// Syntax errors carry a [`Position`]. Conversion errors carry a JSON Pointer
/// ([RFC 6901](https://www.rfc-editor.org/rfc/rfc6901)) to the offending value instead,
/// which can be passed straight to [`Value::pointer`](crate::Value::pointer).
///
/// The error is a single pointer wide, so `Result<T, Error>` stays small.
#[derive(Clone, PartialEq, Eq)]
pub struct Error {
    inner: Box<Inner>,
}

#[derive(Clone, PartialEq, Eq)]
struct Inner {
    kind: ErrorKind,
    position: Option<Position>,
    path: String,
}

impl Error {
    fn new(kind: ErrorKind, position: Option<Position>) -> Self {
        Self {
            inner: Box::new(Inner {
                kind,
                position,
                path: String::new(),
            }),
        }
    }

    /// Builds a syntax error at byte `offset` of `src`. `base` is added to the reported
    /// offset (used for a skipped BOM).
    pub(crate) fn syntax(kind: ErrorKind, src: &[u8], offset: usize, base: usize) -> Self {
        let offset = offset.min(src.len());
        let before = &src[..offset];
        let line_start = before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let line = 1 + before.iter().filter(|&&b| b == b'\n').count();
        // Count characters, i.e. bytes that are not UTF-8 continuation bytes.
        let column = 1 + before[line_start..]
            .iter()
            .filter(|&&b| b & 0xC0 != 0x80)
            .count();
        Self::new(
            kind,
            Some(Position {
                offset: offset + base,
                line,
                column,
            }),
        )
    }

    /// A type mismatch error for conversions: `expected` and `found` are short
    /// descriptions such as `"string"` and `"null"`.
    pub fn type_mismatch(expected: &'static str, found: &'static str) -> Self {
        Self::new(ErrorKind::Type { expected, found }, None)
    }

    /// A missing object member error for conversions.
    pub fn missing_field(name: &str) -> Self {
        Self::new(ErrorKind::MissingField(name.to_string()), None)
    }

    /// A free-form error for user conversions, e.g. a failed business rule.
    ///
    /// ```
    /// let err = dl_json::Error::custom(format_args!("quantity must be > 0, got {}", 0));
    /// assert_eq!(err.to_string(), "quantity must be > 0, got 0");
    /// ```
    pub fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::new(ErrorKind::Custom(msg.to_string()), None)
    }

    /// The kind of error.
    pub fn kind(&self) -> &ErrorKind {
        &self.inner.kind
    }

    /// Where a syntax error occurred; `None` for conversion errors.
    pub fn position(&self) -> Option<Position> {
        self.inner.position
    }

    /// Byte offset of a syntax error, or 0 when there is no position.
    pub fn offset(&self) -> usize {
        self.inner.position.map_or(0, |p| p.offset)
    }

    /// 1-based line of a syntax error, or 0 when there is no position.
    pub fn line(&self) -> usize {
        self.inner.position.map_or(0, |p| p.line)
    }

    /// 1-based column (in characters) of a syntax error, or 0 when there is no position.
    pub fn column(&self) -> usize {
        self.inner.position.map_or(0, |p| p.column)
    }

    /// JSON Pointer to the value a conversion error refers to, e.g. `/lines/2/qty`.
    /// Empty for syntax errors and for errors at the root.
    pub fn path(&self) -> &str {
        &self.inner.path
    }

    /// Returns `true` if this error came from the parser (it has a position).
    pub fn is_syntax(&self) -> bool {
        self.inner.position.is_some()
    }

    /// Prepends an object member name to the error path (escaped per RFC 6901).
    pub(crate) fn in_field(mut self, name: &str) -> Self {
        let mut seg = String::with_capacity(name.len() + 1 + self.inner.path.len());
        seg.push('/');
        for c in name.chars() {
            match c {
                '~' => seg.push_str("~0"),
                '/' => seg.push_str("~1"),
                c => seg.push(c),
            }
        }
        seg.push_str(&self.inner.path);
        self.inner.path = seg;
        self
    }

    /// Prepends an array index to the error path.
    pub(crate) fn in_index(mut self, index: usize) -> Self {
        let mut seg = alloc::format!("/{index}");
        seg.push_str(&self.inner.path);
        self.inner.path = seg;
        self
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnexpectedEof => f.write_str("unexpected end of input"),
            ErrorKind::UnexpectedChar(c) => {
                write!(f, "unexpected character '{}'", c.escape_debug())
            }
            ErrorKind::InvalidNumber => f.write_str("invalid number"),
            ErrorKind::InvalidEscape => f.write_str("invalid escape sequence"),
            ErrorKind::InvalidUnicode => f.write_str("invalid unicode escape"),
            ErrorKind::ControlCharInString => f.write_str("control character in string"),
            ErrorKind::TrailingCharacters => f.write_str("trailing characters"),
            ErrorKind::DepthLimitExceeded => f.write_str("nesting depth limit exceeded"),
            ErrorKind::InvalidUtf8 => f.write_str("invalid UTF-8"),
            ErrorKind::Type { expected, found } => write!(f, "expected {expected}, found {found}"),
            ErrorKind::MissingField(name) => write!(f, "missing field `{name}`"),
            ErrorKind::Custom(msg) => f.write_str(msg),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner.kind, f)?;
        if let Some(p) = self.inner.position {
            write!(f, " at line {} column {}", p.line, p.column)?;
        }
        if !self.inner.path.is_empty() {
            write!(f, " at {}", self.inner.path)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Error");
        d.field("kind", &self.inner.kind);
        if let Some(p) = self.inner.position {
            d.field("line", &p.line)
                .field("column", &p.column)
                .field("offset", &p.offset);
        }
        if !self.inner.path.is_empty() {
            d.field("path", &self.inner.path);
        }
        d.finish()
    }
}

impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn position_counts_lines_and_chars() {
        let src = "{\n  \"a\": 1,\n  \"é\": x\n}".as_bytes();
        let off = src.iter().position(|&b| b == b'x').unwrap();
        let e = Error::syntax(ErrorKind::UnexpectedChar('x'), src, off, 0);
        assert_eq!((e.line(), e.column(), e.offset()), (3, 8, off));
        assert_eq!(e.to_string(), "unexpected character 'x' at line 3 column 8");
        assert!(e.is_syntax());
    }

    #[test]
    fn conversion_paths() {
        let e = Error::type_mismatch("u32", "string")
            .in_field("qty")
            .in_index(2)
            .in_field("a/b~");
        assert_eq!(e.path(), "/a~1b~0/2/qty");
        assert_eq!(e.to_string(), "expected u32, found string at /a~1b~0/2/qty");
        assert_eq!(e.position(), None);
        assert_eq!((e.line(), e.column(), e.offset()), (0, 0, 0));
        assert_eq!(Error::missing_field("id").to_string(), "missing field `id`");
        assert_eq!(core::mem::size_of::<Error>(), core::mem::size_of::<usize>());
    }

    #[test]
    fn display_escapes_char() {
        let e = Error::syntax(ErrorKind::UnexpectedChar('\n'), b"\n", 0, 0);
        assert_eq!(
            e.to_string(),
            "unexpected character '\\n' at line 1 column 1"
        );
    }
}
