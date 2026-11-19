//! JSON output byte-identical to Go's `encoding/json` `Marshal` for the shapes the agent
//! sends: struct fields in declaration order, `omitempty`, HTML-safe string escaping and
//! ES6-style float formatting.
//!
//! The hub and the agent both hash JSON (`ContentHash`, `MappingHash`), so a single
//! differing byte would make every job look changed. Copied from `dl-agent-einvoice`
//! (without the `time.Time` helpers this module does not need).

/// Builds one JSON object, tracking the comma between members.
pub struct Obj<'a> {
    out: &'a mut String,
    first: bool,
}

impl<'a> Obj<'a> {
    /// Starts an object (`{`).
    pub fn new(out: &'a mut String) -> Obj<'a> {
        out.push('{');
        Obj { out, first: true }
    }

    fn key(&mut self, k: &str) -> &mut String {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
        string(self.out, k);
        self.out.push(':');
        self.out
    }

    /// A string field.
    pub fn str(&mut self, k: &str, v: &str) -> &mut Self {
        let out = self.key(k);
        string(out, v);
        self
    }

    /// A string field with `omitempty`.
    pub fn str_omit(&mut self, k: &str, v: &str) -> &mut Self {
        if !v.is_empty() {
            self.str(k, v);
        }
        self
    }

    /// An integer field.
    pub fn int(&mut self, k: &str, v: i64) -> &mut Self {
        let out = self.key(k);
        out.push_str(&v.to_string());
        self
    }

    /// An integer field with `omitempty`.
    pub fn int_omit(&mut self, k: &str, v: i64) -> &mut Self {
        if v != 0 {
            self.int(k, v);
        }
        self
    }

    /// A float64 field.
    pub fn float(&mut self, k: &str, v: f64) -> &mut Self {
        let out = self.key(k);
        float(out, v);
        self
    }

    /// A float64 field with `omitempty`.
    pub fn float_omit(&mut self, k: &str, v: f64) -> &mut Self {
        if v != 0.0 {
            self.float(k, v);
        }
        self
    }

    /// A bool field.
    pub fn bool(&mut self, k: &str, v: bool) -> &mut Self {
        let out = self.key(k);
        out.push_str(if v { "true" } else { "false" });
        self
    }

    /// A bool field with `omitempty`.
    pub fn bool_omit(&mut self, k: &str, v: bool) -> &mut Self {
        if v {
            self.bool(k, v);
        }
        self
    }

    /// A field whose value is written by `f` (objects, arrays, raw JSON).
    pub fn raw(&mut self, k: &str, f: impl FnOnce(&mut String)) -> &mut Self {
        let out = self.key(k);
        f(out);
        self
    }

    /// Closes the object (`}`).
    pub fn end(&mut self) {
        self.out.push('}');
    }
}

/// Writes `items` as an array; `None` writes `null` (a nil Go slice).
pub fn array<T>(out: &mut String, items: Option<&[T]>, mut each: impl FnMut(&mut String, &T)) {
    let Some(items) = items else {
        out.push_str("null");
        return;
    };
    out.push('[');
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        each(out, it);
    }
    out.push(']');
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Go's `appendString` with `escapeHTML`: `<`, `>`, `&`, U+2028 and U+2029 are escaped as
/// `\u00XX`/`\u20XX`; `\b \f \n \r \t` use short escapes; other control bytes `\u00XX`.
/// (Rust strings are valid UTF-8, so Go's `\ufffd` replacement never applies.)
pub fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\u{0}'..='\u{1f}' => {
                let b = c as u8;
                out.push_str("\\u00");
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 0xF)] as char);
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Go's float64 encoder: shortest round-trip digits, plain notation unless
/// `|v| < 1e-6` or `|v| >= 1e21`, where it uses `e` notation with the exponent sign and
/// no zero padding (`1e-07` → `1e-7`, `1e+21`).
pub fn float(out: &mut String, v: f64) {
    let abs = v.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        // Rust's `{:e}` gives the shortest digits as "1.5e-7" / "1e21".
        let s = format!("{v:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        out.push_str(mant);
        out.push('e');
        if let Some(neg) = exp.strip_prefix('-') {
            out.push('-');
            out.push_str(neg);
        } else {
            out.push('+');
            out.push_str(exp);
        }
        return;
    }
    if v == 0.0 {
        // Go prints -0 as "-0".
        out.push_str(if v.is_sign_negative() { "-0" } else { "0" });
        return;
    }
    // Rust's Display for f64 is the shortest round-trip form in plain notation.
    out.push_str(&format!("{v}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(v: f64) -> String {
        let mut s = String::new();
        float(&mut s, v);
        s
    }

    #[test]
    fn floats_match_go() {
        // Expected values from Go: json.Marshal(v).
        for (v, want) in [
            (1.0, "1"),
            (2.5, "2.5"),
            (0.1, "0.1"),
            (1e-7, "1e-7"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (3.3333333333333335, "3.3333333333333335"),
            (0.000001, "0.000001"),
            (1.5e-7, "1.5e-7"),
            (-2.0, "-2"),
            (123456789.125, "123456789.125"),
        ] {
            assert_eq!(f(v), want, "{v}");
        }
    }

    #[test]
    fn strings_match_go() {
        let mut s = String::new();
        string(
            &mut s,
            "A&B <x> \"q\" \\ \n\r\t\u{1}\u{8}\u{c}\u{2028}\u{2029}é日",
        );
        assert_eq!(
            s,
            r#""A\u0026B \u003cx\u003e \"q\" \\ \n\r\t\u0001\b\f\u2028\u2029é日""#
        );
    }
}
