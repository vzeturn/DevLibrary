//! Strict RFC 8259 parser.

use alloc::string::String;
use alloc::vec::Vec;

use crate::number::{self, N};
use crate::{Error, ErrorKind, Map, Number, Value};

/// Parser settings for [`parse_with`] and [`parse_slice_with`].
///
/// ```
/// use dl_json::{ParseOptions, parse_with};
///
/// let opts = ParseOptions { keep_raw_numbers: true, ..ParseOptions::default() };
/// let v = parse_with(r#"{"total": 1234567.10}"#, opts).unwrap();
/// assert_eq!(v["total"].as_number().unwrap().as_raw_str(), Some("1234567.10"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParseOptions {
    /// Maximum nesting of arrays and objects. `128` by default; `0` allows only scalars.
    /// The parser, serializer and `Drop` recurse once per level, so very large limits can
    /// overflow the stack on hostile input.
    pub max_depth: usize,
    /// Keep every number as its exact source text (see [`Number::as_raw_str`]) instead
    /// of converting it to `u64`/`i64`/`f64`. Raw numbers are validated against the JSON
    /// grammar but have no range limit, so `1e400` is accepted. `false` by default.
    pub keep_raw_numbers: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            max_depth: 128,
            keep_raw_numbers: false,
        }
    }
}

/// Parses a JSON text with the default [`ParseOptions`].
///
/// The grammar is RFC 8259 exactly: no comments, trailing commas, single quotes,
/// `NaN`, leading zeros or `+` signs, and only space, tab, LF and CR as whitespace.
/// Any value may be at the top level. A leading byte order mark (U+FEFF) is skipped.
/// When an object repeats a key, the last value wins and keeps the first position.
///
/// ```
/// let v = dl_json::parse(r#"{"room": 1205, "guests": ["An", "Bình"]}"#).unwrap();
/// assert_eq!(v["guests"][1], "Bình");
///
/// let err = dl_json::parse("[1, 2,]").unwrap_err();
/// assert_eq!(err.to_string(), "unexpected character ']' at line 1 column 7");
/// ```
pub fn parse(input: &str) -> Result<Value, Error> {
    parse_with(input, ParseOptions::default())
}

/// Parses a JSON text with custom options. See [`parse`].
pub fn parse_with(input: &str, options: ParseOptions) -> Result<Value, Error> {
    let (src, base) = match input.strip_prefix('\u{feff}') {
        Some(rest) => (rest, 3),
        None => (input, 0),
    };
    let mut p = Parser {
        src,
        b: src.as_bytes(),
        pos: 0,
        base,
        options,
        values: Vec::new(),
        entries: Vec::new(),
    };
    let v = p.value(0)?;
    p.skip_ws();
    if p.pos < p.b.len() {
        return Err(p.err(ErrorKind::TrailingCharacters, p.pos));
    }
    Ok(v)
}

/// Parses JSON from bytes with the default options: validates UTF-8 first
/// ([`ErrorKind::InvalidUtf8`] at the first bad byte), skips a leading UTF-8 byte order
/// mark (`EF BB BF`, allowed by RFC 8259 section 8.1), then behaves like [`parse`].
///
/// ```
/// let v = dl_json::parse_slice(b"\xEF\xBB\xBF{\"ok\": true}").unwrap();
/// assert_eq!(v["ok"], true);
/// assert!(dl_json::parse_slice(b"\"\xFF\"").is_err());
/// ```
pub fn parse_slice(input: &[u8]) -> Result<Value, Error> {
    parse_slice_with(input, ParseOptions::default())
}

/// Byte-input variant of [`parse_with`]. See [`parse_slice`].
pub fn parse_slice_with(input: &[u8], options: ParseOptions) -> Result<Value, Error> {
    match core::str::from_utf8(input) {
        Ok(s) => parse_with(s, options),
        Err(e) => Err(Error::syntax(
            ErrorKind::InvalidUtf8,
            input,
            e.valid_up_to(),
            0,
        )),
    }
}

struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    /// Bytes skipped before `src` (BOM), added to reported offsets.
    base: usize,
    options: ParseOptions,
    /// Scratch stacks shared by all nesting levels: elements are collected here and
    /// moved into an exactly sized `Vec` when the container closes.
    values: Vec<Value>,
    entries: Vec<(String, Value)>,
}

/// `true` for bytes that end a plain run inside a string: `"`, `\` and controls.
static STRING_SPECIAL: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = true;
        i += 1;
    }
    t[b'"' as usize] = true;
    t[b'\\' as usize] = true;
    t
};

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

impl<'a> Parser<'a> {
    #[cold]
    fn err(&self, kind: ErrorKind, at: usize) -> Error {
        Error::syntax(kind, self.b, at, self.base)
    }

    /// Error for the byte at `at`: end of input or the unexpected character there.
    #[cold]
    fn unexpected(&self, at: usize) -> Error {
        match self.src.get(at..).and_then(|s| s.chars().next()) {
            Some(c) => self.err(ErrorKind::UnexpectedChar(c), at),
            None => self.err(ErrorKind::UnexpectedEof, at),
        }
    }

    #[inline]
    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\n' | b'\r' | b'\t') = self.b.get(self.pos) {
            self.pos += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.skip_ws();
        match self.b.get(self.pos) {
            Some(b'"') => {
                self.pos += 1;
                self.string().map(Value::String)
            }
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number().map(Value::Number),
            _ => Err(self.unexpected(self.pos)),
        }
    }

    fn literal(&mut self, word: &[u8], v: Value) -> Result<Value, Error> {
        for (i, &c) in word.iter().enumerate() {
            if self.b.get(self.pos + i) != Some(&c) {
                return Err(self.unexpected(self.pos + i));
            }
        }
        self.pos += word.len();
        Ok(v)
    }

    fn enter(&self, depth: usize) -> Result<(), Error> {
        if depth >= self.options.max_depth {
            return Err(self.err(ErrorKind::DepthLimitExceeded, self.pos));
        }
        Ok(())
    }

    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        self.enter(depth)?;
        self.pos += 1;
        self.skip_ws();
        if self.b.get(self.pos) == Some(&b']') {
            self.pos += 1;
            return Ok(Value::Array(Vec::new()));
        }
        let base = self.values.len();
        loop {
            let v = self.value(depth + 1)?;
            self.values.push(v);
            self.skip_ws();
            match self.b.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.unexpected(self.pos)),
            }
        }
        Ok(Value::Array(self.values.drain(base..).collect()))
    }

    fn object(&mut self, depth: usize) -> Result<Value, Error> {
        self.enter(depth)?;
        self.pos += 1;
        self.skip_ws();
        if self.b.get(self.pos) == Some(&b'}') {
            self.pos += 1;
            return Ok(Value::Object(Map::new()));
        }
        let base = self.entries.len();
        loop {
            if self.b.get(self.pos) != Some(&b'"') {
                return Err(self.unexpected(self.pos));
            }
            self.pos += 1;
            let key = self.string()?;
            self.skip_ws();
            if self.b.get(self.pos) != Some(&b':') {
                return Err(self.unexpected(self.pos));
            }
            self.pos += 1;
            let v = self.value(depth + 1)?;
            self.entries.push((key, v));
            self.skip_ws();
            match self.b.get(self.pos) {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(self.unexpected(self.pos)),
            }
        }
        Ok(Value::Object(Map::from_entries(
            self.entries.drain(base..).collect(),
        )))
    }

    /// Index of the first `"`, `\` or control byte at or after `i`, or the input length.
    #[inline]
    fn scan_plain(&self, mut i: usize) -> usize {
        let b = self.b;
        while let Some(chunk) = b.get(i..i + 8) {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            let w = u64::from_le_bytes(word);
            let q = w ^ (LO * b'"' as u64);
            let s = w ^ (LO * b'\\' as u64);
            let hit = (q.wrapping_sub(LO) & !q)
                | (s.wrapping_sub(LO) & !s)
                | (w.wrapping_sub(LO * 0x20) & !w);
            if hit & HI != 0 {
                break;
            }
            i += 8;
        }
        while let Some(&c) = b.get(i) {
            if STRING_SPECIAL[c as usize] {
                return i;
            }
            i += 1;
        }
        i
    }

    /// Slice of the source between two positions that sit on ASCII bytes (or the end),
    /// which are always character boundaries.
    fn run(&self, start: usize, end: usize) -> Result<&'a str, Error> {
        self.src
            .get(start..end)
            .ok_or_else(|| self.err(ErrorKind::InvalidUtf8, start))
    }

    /// Parses a string body; `self.pos` is just past the opening quote.
    fn string(&mut self) -> Result<String, Error> {
        let start = self.pos;
        let mut i = self.scan_plain(start);
        if self.b.get(i) == Some(&b'"') {
            // Fast path: no escapes, a single exactly sized allocation.
            self.pos = i + 1;
            return Ok(String::from(self.run(start, i)?));
        }
        let mut out = String::with_capacity(i - start + 16);
        let mut run_start = start;
        loop {
            match self.b.get(i) {
                Some(b'"') => {
                    out.push_str(self.run(run_start, i)?);
                    self.pos = i + 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    out.push_str(self.run(run_start, i)?);
                    i = self.escape(i, &mut out)?;
                    run_start = i;
                }
                Some(_) => return Err(self.err(ErrorKind::ControlCharInString, i)),
                None => return Err(self.err(ErrorKind::UnexpectedEof, i)),
            }
            i = self.scan_plain(i);
        }
    }

    /// Decodes the escape whose backslash is at `i`; returns the index after it.
    fn escape(&self, i: usize, out: &mut String) -> Result<usize, Error> {
        let c = match self.b.get(i + 1) {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => return self.unicode_escape(i, out),
            Some(_) => return Err(self.err(ErrorKind::InvalidEscape, i)),
            None => return Err(self.err(ErrorKind::UnexpectedEof, i + 1)),
        };
        out.push(c);
        Ok(i + 2)
    }

    /// Reads 4 hex digits at `at`.
    fn hex4(&self, at: usize, escape_start: usize) -> Result<u32, Error> {
        let mut v = 0u32;
        for k in 0..4 {
            let d = match self.b.get(at + k) {
                Some(&c @ b'0'..=b'9') => c - b'0',
                Some(&c @ b'a'..=b'f') => c - b'a' + 10,
                Some(&c @ b'A'..=b'F') => c - b'A' + 10,
                Some(_) => return Err(self.err(ErrorKind::InvalidUnicode, escape_start)),
                None => return Err(self.err(ErrorKind::UnexpectedEof, at + k)),
            };
            v = v << 4 | d as u32;
        }
        Ok(v)
    }

    /// Decodes `\uXXXX` (and a following low surrogate escape) starting at `i`.
    fn unicode_escape(&self, i: usize, out: &mut String) -> Result<usize, Error> {
        let hi = self.hex4(i + 2, i)?;
        let (cp, end) = match hi {
            0xD800..=0xDBFF => {
                if self.b.get(i + 6..i + 8) != Some(b"\\u") {
                    return Err(self.err(ErrorKind::InvalidUnicode, i));
                }
                let lo = self.hex4(i + 8, i)?;
                if !(0xDC00..=0xDFFF).contains(&lo) {
                    return Err(self.err(ErrorKind::InvalidUnicode, i));
                }
                (0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), i + 12)
            }
            0xDC00..=0xDFFF => return Err(self.err(ErrorKind::InvalidUnicode, i)),
            _ => (hi, i + 6),
        };
        let c = char::from_u32(cp).ok_or_else(|| self.err(ErrorKind::InvalidUnicode, i))?;
        out.push(c);
        Ok(end)
    }

    fn number(&mut self) -> Result<Number, Error> {
        let start = self.pos;
        let b = self.b;
        if !self.options.keep_raw_numbers
            && let Some((n, end)) = fast_number(b, start)
        {
            self.pos = end;
            return Ok(Number { n });
        }
        // Slow path: big integers, long mantissas, large exponents and all errors.
        let sc = number::scan(b, start).map_err(|at| match b.get(at) {
            None => self.err(ErrorKind::UnexpectedEof, at),
            Some(_) => self.err(ErrorKind::InvalidNumber, at),
        })?;
        self.pos = sc.end;
        let lexeme = self.run(start, sc.end)?;
        if self.options.keep_raw_numbers {
            return Ok(Number {
                n: N::Raw(lexeme.into()),
            });
        }
        number::from_lexeme(lexeme, sc.is_int)
            .map(|n| Number { n })
            .ok_or_else(|| self.err(ErrorKind::InvalidNumber, start))
    }
}

/// Powers of ten that are exactly representable as `f64`.
const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// Converts the common number shapes in one pass, without building a lexeme.
///
/// Integers of up to 19 digits always fit in `u64`. Other numbers use Clinger's fast
/// path: when the decimal significand is at most 2^53 and the power of ten at most 22,
/// both are exact `f64` values and one multiplication or division gives the correctly
/// rounded result. Returns `None` for everything else (including syntax errors), which
/// the caller handles with the general path.
#[inline]
fn fast_number(b: &[u8], start: usize) -> Option<(N, usize)> {
    #[inline]
    fn digits(b: &[u8], mut i: usize, m: &mut u64) -> usize {
        while let Some(&c @ b'0'..=b'9') = b.get(i) {
            *m = m.wrapping_mul(10).wrapping_add((c - b'0') as u64);
            i += 1;
        }
        i
    }
    let neg = b.get(start) == Some(&b'-');
    let int_start = start + neg as usize;
    let mut m = 0u64;
    let mut i = digits(b, int_start, &mut m);
    let int_len = i - int_start;
    if int_len == 0 || (int_len > 1 && b.get(int_start) == Some(&b'0')) {
        return None;
    }
    let mut count = int_len;
    let mut e10: i64 = 0;
    let mut is_int = true;
    if b.get(i) == Some(&b'.') {
        is_int = false;
        let frac_start = i + 1;
        i = digits(b, frac_start, &mut m);
        let frac_len = i - frac_start;
        if frac_len == 0 {
            return None;
        }
        count += frac_len;
        e10 = -(frac_len as i64);
    }
    if let Some(b'e' | b'E') = b.get(i) {
        is_int = false;
        i += 1;
        let exp_neg = match b.get(i) {
            Some(b'-') => {
                i += 1;
                true
            }
            Some(b'+') => {
                i += 1;
                false
            }
            _ => false,
        };
        let exp_start = i;
        let mut e = 0i64;
        while let Some(&c @ b'0'..=b'9') = b.get(i) {
            if e < 1_000_000 {
                e = e * 10 + (c - b'0') as i64;
            }
            i += 1;
        }
        if i == exp_start {
            return None;
        }
        e10 += if exp_neg { -e } else { e };
    }
    if count > 19 {
        // `m` may have wrapped.
        return None;
    }
    let n = if is_int {
        if !neg {
            N::PosInt(m)
        } else if m == 0 {
            // Keep the sign of `-0`; an integer cannot.
            N::Float(-0.0)
        } else if m <= 1 << 63 {
            N::NegInt(0i64.wrapping_sub_unsigned(m))
        } else {
            // Below i64::MIN: u64 -> f64 conversion is correctly rounded.
            N::Float(-(m as f64))
        }
    } else {
        if m > 1 << 53 || !(-22..=22).contains(&e10) {
            return None;
        }
        let f = m as f64;
        let f = if e10 >= 0 {
            f * POW10[e10 as usize]
        } else {
            f / POW10[(-e10) as usize]
        };
        N::Float(if neg { -f } else { f })
    };
    Some((n, i))
}

#[cfg(test)]
mod tests;
