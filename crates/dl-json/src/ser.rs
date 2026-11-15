//! Serializer: compact and pretty output, string escaping, number formatting.

use alloc::string::String;
use core::fmt::{self, Write};

use crate::number::N;
use crate::{Number, Value};

/// Serializes `value` as compact JSON (no whitespace).
///
/// ```
/// let v = dl_json::json!({"sku": "A-1", "qty": 2, "price": 19.99, "tags": []});
/// assert_eq!(dl_json::to_string(&v), r#"{"sku":"A-1","qty":2,"price":19.99,"tags":[]}"#);
/// ```
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write_to(value, &mut out);
    out
}

/// Serializes `value` as pretty JSON: 2-space indent, `": "` after keys, one element per
/// line, empty containers as `[]` and `{}`. Same layout as `JSON.stringify(v, null, 2)`.
///
/// ```
/// let v = dl_json::json!({"id": 7, "rooms": [101, 102], "extras": {}});
/// assert_eq!(
///     dl_json::to_string_pretty(&v),
///     "{\n  \"id\": 7,\n  \"rooms\": [\n    101,\n    102\n  ],\n  \"extras\": {}\n}"
/// );
/// ```
pub fn to_string_pretty(value: &Value) -> String {
    to_string_pretty_with(value, "  ")
}

/// Like [`to_string_pretty`] with a custom indent unit, e.g. `"\t"` or `"    "`.
///
/// `indent` is copied as-is once per nesting level; it should contain only JSON
/// whitespace (space or tab), otherwise the output is not valid JSON.
pub fn to_string_pretty_with(value: &Value, indent: &str) -> String {
    let mut out = String::new();
    write_pretty_to(value, &mut out, indent);
    out
}

/// Appends compact JSON for `value` to `out`. Reusing one buffer avoids allocations when
/// serializing many values.
pub fn write_to(value: &Value, out: &mut String) {
    // Writing to a `String` never fails.
    let _ = write_value(out, value, None, 0);
}

/// Appends pretty JSON for `value` to `out`, using `indent` per level
/// (see [`to_string_pretty_with`]).
pub fn write_pretty_to(value: &Value, out: &mut String, indent: &str) {
    // Writing to a `String` never fails.
    let _ = write_value(out, value, Some(indent), 0);
}

pub(crate) fn write_value<W: Write>(
    w: &mut W,
    v: &Value,
    indent: Option<&str>,
    level: usize,
) -> fmt::Result {
    match v {
        Value::Null => w.write_str("null"),
        Value::Bool(true) => w.write_str("true"),
        Value::Bool(false) => w.write_str("false"),
        Value::Number(n) => write_number(w, n),
        Value::String(s) => write_escaped(w, s),
        Value::Array(a) => {
            if a.is_empty() {
                return w.write_str("[]");
            }
            w.write_char('[')?;
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    w.write_char(',')?;
                }
                newline(w, indent, level + 1)?;
                write_value(w, x, indent, level + 1)?;
            }
            newline(w, indent, level)?;
            w.write_char(']')
        }
        Value::Object(m) => {
            if m.is_empty() {
                return w.write_str("{}");
            }
            w.write_char('{')?;
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    w.write_char(',')?;
                }
                newline(w, indent, level + 1)?;
                write_escaped(w, k)?;
                w.write_str(if indent.is_some() { ": " } else { ":" })?;
                write_value(w, x, indent, level + 1)?;
            }
            newline(w, indent, level)?;
            w.write_char('}')
        }
    }
}

fn newline<W: Write>(w: &mut W, indent: Option<&str>, level: usize) -> fmt::Result {
    if let Some(unit) = indent {
        w.write_char('\n')?;
        for _ in 0..level {
            w.write_str(unit)?;
        }
    }
    Ok(())
}

// ---- strings ----

/// Escape action per byte: 0 = copy, `u` = `\u00XX`, 1 = possible U+2028/U+2029 lead
/// byte, anything else = the letter after the backslash.
static ESCAPE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = b'u';
        i += 1;
    }
    t[0x08] = b'b';
    t[0x0C] = b'f';
    t[b'\n' as usize] = b'n';
    t[b'\r' as usize] = b'r';
    t[b'\t' as usize] = b't';
    t[b'"' as usize] = b'"';
    t[b'\\' as usize] = b'\\';
    t[0xE2] = 1;
    t
};

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

#[inline]
fn has_byte(w: u64, b: u8) -> u64 {
    let x = w ^ (LO * b as u64);
    x.wrapping_sub(LO) & !x
}

/// Index of the first byte at or after `i` that may need escaping, or `b.len()`.
#[inline]
fn next_escape(b: &[u8], mut i: usize) -> usize {
    while let Some(chunk) = b.get(i..i + 8) {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let w = u64::from_le_bytes(word);
        let hit = has_byte(w, b'"')
            | has_byte(w, b'\\')
            | has_byte(w, 0xE2)
            | (w.wrapping_sub(LO * 0x20) & !w);
        if hit & HI != 0 {
            break;
        }
        i += 8;
    }
    while let Some(&c) = b.get(i) {
        if ESCAPE[c as usize] != 0 {
            return i;
        }
        i += 1;
    }
    i
}

/// Writes `s` as a JSON string literal. Escapes `"`, `\`, all control characters and
/// U+2028/U+2029; other characters are written as UTF-8.
pub(crate) fn write_escaped<W: Write>(w: &mut W, s: &str) -> fmt::Result {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    w.write_char('"')?;
    let b = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    loop {
        i = next_escape(b, i);
        let Some(&c) = b.get(i) else { break };
        let mut buf = [b'\\', 0, 0, 0, 0, 0];
        let (len, consumed) = match ESCAPE[c as usize] {
            1 => match (b.get(i + 1), b.get(i + 2)) {
                (Some(0x80), Some(&last @ (0xA8 | 0xA9))) => {
                    buf[1..].copy_from_slice(if last == 0xA8 { b"u2028" } else { b"u2029" });
                    (6, 3)
                }
                _ => {
                    i += 1;
                    continue;
                }
            },
            b'u' => {
                buf[1..4].copy_from_slice(b"u00");
                buf[4] = HEX[(c >> 4) as usize];
                buf[5] = HEX[(c & 0xF) as usize];
                (6, 1)
            }
            letter => {
                buf[1] = letter;
                (2, 1)
            }
        };
        // `start` and `i` are at character boundaries: `i` is at an ASCII byte or a
        // UTF-8 lead byte, and `start` follows a complete character.
        if let Some(run) = s.get(start..i) {
            w.write_str(run)?;
        }
        if let Ok(esc) = core::str::from_utf8(&buf[..len]) {
            w.write_str(esc)?;
        }
        i += consumed;
        start = i;
    }
    if let Some(run) = s.get(start..) {
        w.write_str(run)?;
    }
    w.write_char('"')
}

// ---- numbers ----

/// Fixed-size buffer for formatting without allocation.
pub(crate) struct StackBuf {
    buf: [u8; 64],
    len: usize,
}

impl StackBuf {
    pub(crate) fn new() -> Self {
        Self {
            buf: [0; 64],
            len: 0,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // Only `&str` data is ever written, so this is valid UTF-8.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl Write for StackBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        let dst = self.buf.get_mut(self.len..end).ok_or(fmt::Error)?;
        dst.copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Two ASCII digits for every value 0..100.
static DIGITS2: [u8; 200] = {
    let mut t = [0u8; 200];
    let mut i = 0;
    while i < 100 {
        t[2 * i] = b'0' + (i / 10) as u8;
        t[2 * i + 1] = b'0' + (i % 10) as u8;
        i += 1;
    }
    t
};

fn write_u64<W: Write>(w: &mut W, mut v: u64) -> fmt::Result {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while v >= 100 {
        let r = (v % 100) as usize * 2;
        v /= 100;
        i -= 2;
        buf[i..i + 2].copy_from_slice(&DIGITS2[r..r + 2]);
    }
    if v >= 10 {
        let r = v as usize * 2;
        i -= 2;
        buf[i..i + 2].copy_from_slice(&DIGITS2[r..r + 2]);
    } else {
        i -= 1;
        buf[i] = b'0' + v as u8;
    }
    // ASCII digits only.
    w.write_str(core::str::from_utf8(&buf[i..]).unwrap_or("0"))
}

/// Writes a finite float like ECMAScript `Number::toString` (used by `JSON.stringify`):
/// shortest round-trip digits, plain decimal for `1e-6 <= |f| < 1e21`, otherwise
/// exponent form. Differences from JS: the exponent has no `+` sign (`1e21`, JS writes
/// `1e+21`), `-0` keeps its sign (JS writes `0`), and see [`plain_int_is_lossy`].
/// All forms are valid JSON.
pub(crate) fn write_f64<W: Write>(w: &mut W, f: f64) -> fmt::Result {
    // Rust's `{:e}` prints the shortest digits that round-trip: `d.ddde[-]x`.
    let neg = f.is_sign_negative();
    // Clear the sign bit (`f64::abs` is not in `core` on every supported toolchain).
    let abs = f64::from_bits(f.to_bits() & !(1 << 63));
    let mut sci = StackBuf::new();
    write!(sci, "{abs:e}")?;
    let s = sci.as_str();
    let (mantissa, exp) = s.split_once('e').unwrap_or((s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (first, rest) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = StackBuf::new();
    digits.write_str(first)?;
    digits.write_str(rest)?;
    let d = digits.as_str();
    let k = d.len() as i32;
    // The value is 0.d * 10^n.
    let n = exp + 1;
    if neg {
        w.write_char('-')?;
    }
    if k <= n && n <= 21 && !plain_int_is_lossy(neg, abs, d, k, n) {
        w.write_str(d)?;
        for _ in 0..n - k {
            w.write_char('0')?;
        }
        Ok(())
    } else if 0 < n && n < k && n <= 21 {
        // 0 < n < k here, so the split point is inside `d`.
        let (int, frac) = d.split_at_checked(n as usize).unwrap_or((d, ""));
        w.write_str(int)?;
        w.write_char('.')?;
        w.write_str(frac)
    } else if -6 < n && n <= 0 {
        w.write_str("0.")?;
        for _ in 0..-n {
            w.write_char('0')?;
        }
        w.write_str(d)
    } else {
        // `d` has at least one ASCII digit.
        let (lead, tail) = d.split_at_checked(1).unwrap_or((d, ""));
        w.write_str(lead)?;
        if !tail.is_empty() {
            w.write_char('.')?;
            w.write_str(tail)?;
        }
        write!(w, "e{}", n - 1)
    }
}

/// `true` if the zero-padded plain form of an integral float (`d` followed by `n - k`
/// zeros) would be read back by the parser as a 64-bit integer different from the
/// float's exact value. This happens for some floats between 2^53 and 2^64, e.g.
/// `12345678901234567168.0`, which JS prints as `12345678901234567000`. Such floats are
/// written in exponent form instead, so they read back as the same float.
fn plain_int_is_lossy(neg: bool, abs: f64, d: &str, k: i32, n: i32) -> bool {
    if abs < 9_007_199_254_740_992.0 {
        // Below 2^53 the shortest digits of an integral float are its exact value.
        return false;
    }
    let Ok(digits) = d.parse::<u128>() else {
        return false;
    };
    // n <= 21, so the padded value stays below 10^21 and fits in u128.
    let padded = digits * 10u128.pow((n - k) as u32);
    let parsed_as_int = if neg {
        padded <= 1 << 63
    } else {
        padded <= u64::MAX as u128
    };
    parsed_as_int && padded != abs as u128
}

pub(crate) fn write_number<W: Write>(w: &mut W, n: &Number) -> fmt::Result {
    match &n.n {
        N::PosInt(u) => write_u64(w, *u),
        N::NegInt(i) => {
            w.write_char('-')?;
            write_u64(w, i.unsigned_abs())
        }
        N::Float(f) => write_f64(w, *f),
        N::Raw(s) => w.write_str(s),
    }
}

#[cfg(test)]
mod tests;
