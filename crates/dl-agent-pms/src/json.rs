//! JSON the way Go's `encoding/json` reads and writes it for the agent's shapes:
//! integers only from integer literals that fit, `null` into a scalar leaving it unset,
//! invalid UTF-8 replaced with U+FFFD, numbers kept as their exact source text, and
//! HTML-safe string escaping on output (`<`, `>`, `&`, U+2028, U+2029).

use std::collections::BTreeMap;

use dl_json::{Map, Number, ParseOptions, Value};

/// Parses JSON keeping every number's source text. Invalid UTF-8 becomes U+FFFD, as
/// `json.Unmarshal` does inside strings.
pub fn parse(data: &[u8]) -> Result<Value, dl_json::Error> {
    let text = String::from_utf8_lossy(data);
    dl_json::parse_with(
        &text,
        ParseOptions {
            keep_raw_numbers: true,
            ..ParseOptions::default()
        },
    )
}

/// Parses a JSON object (`None` for anything else or invalid JSON).
pub fn parse_object(data: &[u8]) -> Option<Map> {
    match parse(data) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// A field that does not hold the Go type it is decoded into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    /// The field path, e.g. `limits.inflight_cap`.
    pub field: String,
}

fn type_err(field: &str) -> TypeError {
    TypeError {
        field: field.to_owned(),
    }
}

/// The source text of a number.
pub fn number_text(n: &Number) -> String {
    n.as_raw_str()
        .map(str::to_owned)
        .unwrap_or_else(|| n.to_string())
}

/// A JSON number written as an integer literal that fits `i64` (Go's int64 decoding).
pub fn as_int(v: &Value) -> Option<i64> {
    let text = number_text(v.as_number()?);
    if text.contains(['.', 'e', 'E']) {
        return None;
    }
    text.parse().ok()
}

/// A Go `int64` field: missing or `null` is 0 (the zero value stays).
pub fn int(obj: &Map, key: &str, path: &str) -> Result<i64, TypeError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(v) => as_int(v).ok_or_else(|| type_err(path)),
    }
}

/// A Go `*int64` field: `None` for missing or `null`.
pub fn opt_int(obj: &Map, key: &str, path: &str) -> Result<Option<i64>, TypeError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => as_int(v).map(Some).ok_or_else(|| type_err(path)),
    }
}

/// A Go `*int32` field.
pub fn opt_i32(obj: &Map, key: &str, path: &str) -> Result<Option<i32>, TypeError> {
    match opt_int(obj, key, path)? {
        None => Ok(None),
        Some(n) => i32::try_from(n).map(Some).map_err(|_| type_err(path)),
    }
}

/// A Go `string` field: missing or `null` is "".
pub fn string(obj: &Map, key: &str, path: &str) -> Result<String, TypeError> {
    Ok(opt_string(obj, key, path)?.unwrap_or_default())
}

/// A Go `*string` field.
pub fn opt_string(obj: &Map, key: &str, path: &str) -> Result<Option<String>, TypeError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(type_err(path)),
    }
}

/// A Go `bool` field.
pub fn boolean(obj: &Map, key: &str, path: &str) -> Result<bool, TypeError> {
    Ok(opt_bool(obj, key, path)?.unwrap_or(false))
}

/// A Go `*bool` field.
pub fn opt_bool(obj: &Map, key: &str, path: &str) -> Result<Option<bool>, TypeError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(type_err(path)),
    }
}

/// The member a Go struct field takes when keys are matched like `encoding/json` does:
/// the last key equal to `name` ignoring ASCII case.
pub fn field_ci<'a>(obj: &'a Map, name: &str) -> Option<&'a Value> {
    obj.iter()
        .rfind(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Go's string encoder with HTML escaping.
pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
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

/// A JSON string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    write_str(&mut out, s);
    out
}

/// Writes a value compactly, keeping object member order and number text (what
/// `json.Marshal` does with a `json.RawMessage`).
pub fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&number_text(n)),
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(out, k);
                out.push(':');
                write_value(out, item);
            }
            out.push('}');
        }
    }
}

/// The compact text of a value.
pub fn to_string(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

/// Writes a Go `map[string]json.RawMessage`: keys sorted bytewise.
pub fn write_map(out: &mut String, m: &BTreeMap<String, Value>) {
    out.push('{');
    for (i, (k, v)) in m.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_str(out, k);
        out.push(':');
        write_value(out, v);
    }
    out.push('}');
}

/// `json.MarshalIndent(v, "", indent)` of an already decoded value: one member per line,
/// `"key": value`, empty objects and arrays as `{}` and `[]`.
pub fn to_pretty(v: &Value, indent: &str) -> String {
    let mut out = String::new();
    pretty(&mut out, v, indent, 0);
    out
}

fn newline(out: &mut String, indent: &str, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str(indent);
    }
}

fn pretty(out: &mut String, v: &Value, indent: &str, depth: usize) {
    match v {
        Value::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                pretty(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() => {
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent, depth + 1);
                write_str(out, k);
                out.push_str(": ");
                pretty(out, item, indent, depth + 1);
            }
            newline(out, indent, depth);
            out.push('}');
        }
        other => write_value(out, other),
    }
}

/// Builds a JSON object member by member, in the order of a Go struct's fields.
#[derive(Debug, Default)]
pub struct Obj {
    out: String,
}

impl Obj {
    /// An empty object.
    pub fn new() -> Obj {
        Obj {
            out: String::from("{"),
        }
    }

    fn key(&mut self, k: &str) -> &mut String {
        if self.out.len() > 1 {
            self.out.push(',');
        }
        write_str(&mut self.out, k);
        self.out.push(':');
        &mut self.out
    }

    /// A string member.
    pub fn str(mut self, k: &str, v: &str) -> Obj {
        write_str(self.key(k), v);
        self
    }

    /// A `*string` member (`null` for `None`).
    pub fn opt_str(mut self, k: &str, v: Option<&str>) -> Obj {
        match v {
            Some(s) => write_str(self.key(k), s),
            None => self.key(k).push_str("null"),
        }
        self
    }

    /// An integer member.
    pub fn int(mut self, k: &str, v: i64) -> Obj {
        self.key(k).push_str(&v.to_string());
        self
    }

    /// A `*int` member (`null` for `None`).
    pub fn opt_int(mut self, k: &str, v: Option<i64>) -> Obj {
        match v {
            Some(n) => self.key(k).push_str(&n.to_string()),
            None => self.key(k).push_str("null"),
        }
        self
    }

    /// A bool member.
    pub fn bool(mut self, k: &str, v: bool) -> Obj {
        self.key(k).push_str(if v { "true" } else { "false" });
        self
    }

    /// A member whose JSON text is already encoded.
    pub fn raw(mut self, k: &str, json: &str) -> Obj {
        self.key(k).push_str(json);
        self
    }

    /// A value member.
    pub fn value(mut self, k: &str, v: &Value) -> Obj {
        write_value(self.key(k), v);
        self
    }

    /// The finished object.
    pub fn finish(mut self) -> String {
        self.out.push('}');
        self.out
    }
}

/// The number of Unicode scalar values (Go's `utf8.RuneCountInString`).
pub fn rune_count(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(s: &str) -> Map {
        parse_object(s.as_bytes()).unwrap()
    }

    #[test]
    fn integers_like_go() {
        let o = obj(r#"{"a":1,"b":1.0,"c":1e3,"d":99999999999999999999,"e":null,"f":"1","g":-0}"#);
        assert_eq!(int(&o, "a", "a"), Ok(1));
        assert!(int(&o, "b", "b").is_err());
        assert!(int(&o, "c", "c").is_err());
        assert!(int(&o, "d", "d").is_err());
        assert_eq!(int(&o, "e", "e"), Ok(0));
        assert!(int(&o, "f", "f").is_err());
        assert_eq!(int(&o, "g", "g"), Ok(0));
        assert_eq!(int(&o, "missing", "m"), Ok(0));
        let o = obj(r#"{"n":2147483648}"#);
        assert!(opt_i32(&o, "n", "n").is_err());
    }

    #[test]
    fn writes_like_go() {
        let v = parse(br#"{"b":"<a&b>","a":[1.50,true,null],"z":"\u2028"}"#).unwrap();
        assert_eq!(
            to_string(&v),
            r#"{"b":"\u003ca\u0026b\u003e","a":[1.50,true,null],"z":"\u2028"}"#
        );
        let mut m = BTreeMap::new();
        m.insert("b".to_owned(), Value::Null);
        m.insert("a".to_owned(), Value::Bool(true));
        let mut s = String::new();
        write_map(&mut s, &m);
        assert_eq!(s, r#"{"a":true,"b":null}"#);
        assert_eq!(
            Obj::new()
                .str("t", "x")
                .opt_int("n", None)
                .int("i", -3)
                .finish(),
            r#"{"t":"x","n":null,"i":-3}"#
        );
    }

    #[test]
    fn pretty_like_marshal_indent() {
        let v = parse(br#"{"a":{"b":[1,{}],"c":[]},"d":"x"}"#).unwrap();
        assert_eq!(
            to_pretty(&v, "  "),
            "{\n  \"a\": {\n    \"b\": [\n      1,\n      {}\n    ],\n    \"c\": []\n  },\n  \"d\": \"x\"\n}"
        );
    }

    #[test]
    fn invalid_utf8_becomes_replacement() {
        let v = parse(b"\"a\xffb\"").unwrap();
        assert_eq!(v.as_str(), Some("a\u{fffd}b"));
    }

    #[test]
    fn case_insensitive_last_match() {
        let o = obj(r#"{"Kind":"a","kind":"b"}"#);
        assert_eq!(field_ci(&o, "KIND").and_then(Value::as_str), Some("b"));
    }
}
