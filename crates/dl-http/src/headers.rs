//! Header fields: validation and an ordered, case-insensitive multimap.

use crate::Error;
use std::fmt;

/// Header names whose values `Debug` hides.
const SENSITIVE: [&str; 4] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

/// `tchar` from RFC 9110 section 5.6.2.
pub(crate) fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// A byte allowed inside a field value: HTAB, SP, VCHAR or obs-text.
pub(crate) fn is_field_byte(b: u8) -> bool {
    b == b'\t' || b == b' ' || (0x21..=0x7e).contains(&b) || b >= 0x80
}

fn is_ows(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

pub(crate) fn trim_ows(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if !is_ows(*first) {
            break;
        }
        s = rest;
    }
    while let [rest @ .., last] = s {
        if !is_ows(*last) {
            break;
        }
        s = rest;
    }
    s
}

/// Checks that `name` is a non-empty token.
pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || !name.bytes().all(is_tchar) {
        return Err(Error::InvalidHeader {
            reason: "header name must be a non-empty token",
        });
    }
    Ok(())
}

/// Trims surrounding whitespace and checks that the value has no control characters
/// (CR and LF in particular, which would allow header injection).
pub(crate) fn validate_value(value: &str) -> Result<&str, Error> {
    let v = value.trim_matches([' ', '\t']);
    if !v.bytes().all(is_field_byte) {
        return Err(Error::InvalidHeader {
            reason: "header value contains a control character",
        });
    }
    Ok(v)
}

/// Bytes received on the wire as text: UTF-8 when valid, otherwise ISO-8859-1.
pub(crate) fn wire_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// Parses one `name: value` line (without the line terminator) received from a peer.
pub(crate) fn parse_field_line(line: &[u8]) -> Result<(String, String), &'static str> {
    if matches!(line.first(), Some(&(b' ' | b'\t'))) {
        return Err("obsolete line folding is not allowed");
    }
    let colon = line
        .iter()
        .position(|&b| b == b':')
        .ok_or("header line without a colon")?;
    let name = &line[..colon];
    if name.is_empty() || !name.iter().all(|&b| is_tchar(b)) {
        return Err("invalid header name");
    }
    let value = trim_ows(&line[colon + 1..]);
    if !value.iter().all(|&b| is_field_byte(b)) {
        return Err("invalid byte in header value");
    }
    // `name` is ASCII (tchar), so the conversion is lossless.
    Ok((wire_text(name), wire_text(value)))
}

/// An ordered list of header fields.
///
/// Names compare case-insensitively but keep the case they were added with; several
/// fields may share a name and keep their order. Values never contain CR, LF or other
/// control characters.
///
/// ```
/// let mut h = dl_http::Headers::new();
/// h.append("Accept", "application/json")?;
/// h.append("X-Tag", "a")?;
/// h.append("x-tag", "b")?;
/// assert_eq!(h.get("accept"), Some("application/json"));
/// assert_eq!(h.get_all("X-TAG").collect::<Vec<_>>(), ["a", "b"]);
/// assert!(h.append("X-Bad", "a\r\nInjected: 1").is_err());
/// # Ok::<(), dl_http::Error>(())
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// An empty list.
    pub fn new() -> Headers {
        Headers::default()
    }

    /// Number of fields.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no fields.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The first value for `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All values for `name`, in order.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Whether a field named `name` exists.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Whether the comma-separated values of `name` contain `token` (case-insensitive),
    /// e.g. `Connection: keep-alive, close` contains `close`.
    pub fn has_token(&self, name: &str, token: &str) -> bool {
        self.get_all(name)
            .flat_map(|v| v.split(','))
            .any(|t| t.trim().eq_ignore_ascii_case(token))
    }

    /// Adds a field after the existing ones. Surrounding whitespace of the value is
    /// removed.
    ///
    /// # Errors
    /// [`Error::InvalidHeader`] if the name is not a token or the value contains control
    /// characters.
    pub fn append(&mut self, name: &str, value: &str) -> Result<(), Error> {
        validate_name(name)?;
        let value = validate_value(value)?;
        self.entries.push((name.to_owned(), value.to_owned()));
        Ok(())
    }

    /// Replaces all fields named `name` with one field.
    ///
    /// # Errors
    /// Same as [`Headers::append`].
    pub fn insert(&mut self, name: &str, value: &str) -> Result<(), Error> {
        validate_name(name)?;
        let value = validate_value(value)?;
        self.remove(name);
        self.entries.push((name.to_owned(), value.to_owned()));
        Ok(())
    }

    /// Removes all fields named `name`; returns whether any existed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.entries.len() != before
    }

    /// Iterates over `(name, value)` pairs in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// Adds a field that was already validated.
    pub(crate) fn push_validated(&mut self, name: String, value: String) {
        self.entries.push((name, value));
    }
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut m = f.debug_map();
        for (n, v) in &self.entries {
            if SENSITIVE.iter().any(|s| n.eq_ignore_ascii_case(s)) {
                m.entry(n, &"<redacted>");
            } else {
                m.entry(n, v);
            }
        }
        m.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_and_ordered() {
        let mut h = Headers::new();
        h.append("Content-Type", "application/json").unwrap();
        h.append("Set-Cookie", "a=1").unwrap();
        h.append("set-cookie", "b=2").unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h.get("CONTENT-TYPE"), Some("application/json"));
        assert_eq!(h.get_all("Set-Cookie").collect::<Vec<_>>(), ["a=1", "b=2"]);
        let names: Vec<&str> = h.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["Content-Type", "Set-Cookie", "set-cookie"]);
        assert!(h.contains("content-type"));
        assert!(!h.contains("content-length"));

        h.insert("SET-COOKIE", "c=3").unwrap();
        assert_eq!(h.get_all("set-cookie").collect::<Vec<_>>(), ["c=3"]);
        assert!(h.remove("Content-type"));
        assert!(!h.remove("Content-type"));
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn validation() {
        let mut h = Headers::new();
        for bad in ["", "Bad Name", "Bad:Name", "Bä", "a\r\nb", "(x)", "a/b"] {
            assert!(h.append(bad, "v").is_err(), "name {bad:?}");
        }
        for bad in ["a\r\nX: y", "a\nb", "a\rb", "a\0b", "\x7f", "a\x1bb"] {
            assert!(h.append("X", bad).is_err(), "value {bad:?}");
        }
        h.append("X", "  padded\t").unwrap();
        assert_eq!(h.get("x"), Some("padded"));
        h.append("Y", "").unwrap();
        h.append("Z", "tab\tinside, ünïcode").unwrap();
        assert!(h.insert("A B", "v").is_err());
        assert!(h.insert("A", "\n").is_err());
    }

    #[test]
    fn tokens() {
        let mut h = Headers::new();
        h.append("Connection", "keep-alive, Close").unwrap();
        assert!(h.has_token("connection", "close"));
        assert!(h.has_token("connection", "KEEP-ALIVE"));
        assert!(!h.has_token("connection", "upgrade"));
    }

    #[test]
    fn debug_redacts_credentials() {
        let mut h = Headers::new();
        h.append("Authorization", "Bearer secret-token").unwrap();
        h.append("X-Device-ID", "dev-1").unwrap();
        let d = format!("{h:?}");
        assert!(!d.contains("secret-token"), "{d}");
        assert!(d.contains("dev-1"), "{d}");
    }

    #[test]
    fn field_lines() {
        assert_eq!(
            parse_field_line(b"Content-Length:  42 \t").unwrap(),
            ("Content-Length".to_owned(), "42".to_owned())
        );
        assert_eq!(parse_field_line(b"X:").unwrap().1, "");
        assert_eq!(parse_field_line(b"X: caf\xe9").unwrap().1, "café");
        assert_eq!(parse_field_line("X: café".as_bytes()).unwrap().1, "café");
        assert!(parse_field_line(b" folded").is_err());
        assert!(parse_field_line(b"\tfolded").is_err());
        assert!(parse_field_line(b"Content-Length : 5").is_err());
        assert!(parse_field_line(b": empty-name").is_err());
        assert!(parse_field_line(b"no colon").is_err());
        assert!(parse_field_line(b"X: a\0b").is_err());
        assert!(parse_field_line(b"X: a\rb").is_err());
        assert!(parse_field_line(b"X: a\x7fb").is_err());
        assert!(parse_field_line(b"X\x00: a").is_err());
    }
}
