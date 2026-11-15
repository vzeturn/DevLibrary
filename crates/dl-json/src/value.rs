//! The JSON value tree, accessors and conversions.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;
use core::{fmt, mem, ops, str::FromStr};

use crate::{Error, FromJson, Map, Number};

/// Shared `null` returned by indexing when a key or index is missing.
pub(crate) static NULL: Value = Value::Null;

/// Any JSON value.
///
/// ```
/// use dl_json::{Value, json};
///
/// let booking = json!({"guest": "Nguyen Van A", "nights": 3, "paid": false});
/// assert_eq!(booking["guest"], "Nguyen Van A");
/// assert_eq!(booking["nights"].as_u64(), Some(3));
/// assert!(booking["missing"].is_null());
/// assert_eq!(booking.to_string(), r#"{"guest":"Nguyen Van A","nights":3,"paid":false}"#);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    /// `null`.
    #[default]
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(Number),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object.
    Object(Map),
}

impl Value {
    /// The JSON type name: `"null"`, `"boolean"`, `"number"`, `"string"`, `"array"` or
    /// `"object"`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// Looks up an object member. `None` if missing or if `self` is not an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    /// Mutable variant of [`get`](Self::get).
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.as_object_mut()?.get_mut(key)
    }

    /// Looks up an array element. `None` if out of bounds or if `self` is not an array.
    pub fn get_index(&self, index: usize) -> Option<&Value> {
        self.as_array()?.get(index)
    }

    /// Mutable variant of [`get_index`](Self::get_index).
    pub fn get_index_mut(&mut self, index: usize) -> Option<&mut Value> {
        self.as_array_mut()?.get_mut(index)
    }

    /// Looks up a value by JSON Pointer ([RFC 6901](https://www.rfc-editor.org/rfc/rfc6901)).
    ///
    /// `""` is the whole document, `/a/0/b` walks members and array indexes, `~1` stands
    /// for `/` and `~0` for `~`. Returns `None` for missing values and malformed pointers
    /// (no leading `/`, a bad `~` escape, or an array index with leading zeros or `-`).
    ///
    /// ```
    /// let invoice = dl_json::json!({"lines": [{"sku": "A-1"}, {"sku": "B/2"}]});
    /// assert_eq!(invoice.pointer("/lines/1/sku").and_then(|v| v.as_str()), Some("B/2"));
    /// assert!(invoice.pointer("/lines/9").is_none());
    /// ```
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        let mut cur = self;
        for token in tokens(pointer)? {
            cur = match cur {
                Value::Object(m) => m.get(&unescape(token)?)?,
                Value::Array(a) => a.get(array_index(token)?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// Mutable variant of [`pointer`](Self::pointer).
    ///
    /// ```
    /// let mut booking = dl_json::json!({"rooms": [{"nights": 2}]});
    /// *booking.pointer_mut("/rooms/0/nights").unwrap() = 3.into();
    /// assert_eq!(booking["rooms"][0]["nights"], 3);
    /// ```
    pub fn pointer_mut(&mut self, pointer: &str) -> Option<&mut Value> {
        let mut cur = self;
        for token in tokens(pointer)? {
            cur = match cur {
                Value::Object(m) => m.get_mut(&unescape(token)?)?,
                Value::Array(a) => a.get_mut(array_index(token)?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// Replaces `self` with `null` and returns the previous value.
    pub fn take(&mut self) -> Value {
        mem::take(self)
    }

    /// `true` for `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// `true` for booleans.
    pub fn is_boolean(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    /// `true` for numbers.
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }

    /// `true` for strings.
    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// `true` for arrays.
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// `true` for objects.
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// The boolean, if `self` is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The number, if `self` is one.
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// See [`Number::as_i64`]; `None` for non-numbers.
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number()?.as_i64()
    }

    /// See [`Number::as_u64`]; `None` for non-numbers.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number()?.as_u64()
    }

    /// See [`Number::as_f64`]; `None` for non-numbers.
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number()?.as_f64()
    }

    /// The string slice, if `self` is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The elements, if `self` is an array.
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The mutable elements, if `self` is an array.
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The members, if `self` is an object.
    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    /// The mutable members, if `self` is an object.
    pub fn as_object_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    /// Converts the object member `name` with [`FromJson`].
    ///
    /// If the member is missing, `T` is converted from `null` when it accepts it
    /// (`Option<T>` gives `None`, `Value` gives `Null`); otherwise the error is
    /// [`ErrorKind::MissingField`](crate::ErrorKind::MissingField) with the name.
    /// Conversion errors get `/name` prepended to their [`path`](Error::path).
    ///
    /// ```
    /// use dl_json::{ErrorKind, json};
    ///
    /// let room = json!({"number": 1205, "note": null});
    /// assert_eq!(room.field::<u16>("number").unwrap(), 1205);
    /// assert_eq!(room.field::<Option<String>>("note").unwrap(), None);
    /// assert_eq!(room.field::<Option<String>>("absent").unwrap(), None);
    /// let err = room.field::<String>("guest").unwrap_err();
    /// assert_eq!(err.kind(), &ErrorKind::MissingField("guest".into()));
    /// ```
    pub fn field<T: FromJson>(&self, name: &str) -> Result<T, Error> {
        let Value::Object(m) = self else {
            return Err(Error::type_mismatch("object", self.type_name()));
        };
        match m.get(name) {
            Some(v) => T::from_json(v).map_err(|e| e.in_field(name)),
            None => T::from_json(&NULL).map_err(|_| Error::missing_field(name)),
        }
    }
}

/// Splits a JSON Pointer into raw reference tokens. `None` if it does not start with `/`.
fn tokens(pointer: &str) -> Option<core::iter::Skip<core::str::Split<'_, char>>> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return None;
    }
    // "" yields no tokens; "/a/b" yields "a", "b" after skipping the empty first piece.
    Some(pointer.split('/').skip(1))
}

/// Decodes `~1` and `~0` in a reference token. `None` for any other `~` sequence.
fn unescape(token: &str) -> Option<Cow<'_, str>> {
    if !token.contains('~') {
        return Some(Cow::Borrowed(token));
    }
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(Cow::Owned(out))
}

/// Parses an array index token: `0` or a digit string without leading zeros.
fn array_index(token: &str) -> Option<usize> {
    let b = token.as_bytes();
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) || (b.len() > 1 && b[0] == b'0') {
        return None;
    }
    token.parse().ok()
}

impl ops::Index<&str> for Value {
    type Output = Value;
    /// Object member, or `null` if missing or `self` is not an object. Never panics.
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}

impl ops::Index<usize> for Value {
    type Output = Value;
    /// Array element, or `null` if out of bounds or `self` is not an array. Never panics.
    fn index(&self, index: usize) -> &Value {
        self.get_index(index).unwrap_or(&NULL)
    }
}

impl fmt::Display for Value {
    /// Compact JSON. The alternate flag (`{:#}`) gives pretty output with 2-space indent.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            crate::ser::write_value(f, self, Some("  "), 0)
        } else {
            crate::ser::write_value(f, self, None, 0)
        }
    }
}

impl FromStr for Value {
    type Err = Error;
    /// Same as [`parse`](crate::parse).
    fn from_str(s: &str) -> Result<Value, Error> {
        crate::parse(s)
    }
}

// ---- From conversions ----

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

macro_rules! from_int {
    ($($t:ty)*) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Value::Number(Number::from(v))
            }
        }
    )*};
}
from_int!(u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize);

impl From<f64> for Value {
    /// Finite floats become numbers; NaN and infinities become `null` (like
    /// `JSON.stringify`).
    fn from(f: f64) -> Self {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

impl From<f32> for Value {
    /// Uses the shortest decimal that round-trips the `f32`, so `0.1f32` becomes `0.1`
    /// rather than `0.10000000149011612`. NaN and infinities become `null`.
    fn from(f: f32) -> Self {
        Value::from(f32_to_f64(f))
    }
}

/// Converts through the shortest round-trip decimal representation of `f`.
pub(crate) fn f32_to_f64(f: f32) -> f64 {
    if !f.is_finite() {
        return f64::NAN;
    }
    let mut buf = crate::ser::StackBuf::new();
    // Writing a float into a 64-byte buffer cannot fail: `{:e}` of an f32 is < 20 bytes.
    let _ = fmt::Write::write_fmt(&mut buf, format_args!("{f:e}"));
    buf.as_str().parse().unwrap_or(f as f64)
}

impl From<Number> for Value {
    fn from(n: Number) -> Self {
        Value::Number(n)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.into())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&String> for Value {
    fn from(s: &String) -> Self {
        Value::String(s.clone())
    }
}

impl From<Cow<'_, str>> for Value {
    fn from(s: Cow<'_, str>) -> Self {
        Value::String(s.into_owned())
    }
}

impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::Object(m)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}

impl<T: Clone + Into<Value>> From<&[T]> for Value {
    fn from(v: &[T]) -> Self {
        Value::Array(v.iter().cloned().map(Into::into).collect())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    /// `None` becomes `null`.
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

// ---- Equality with Rust primitives ----

impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<String> for Value {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}

impl PartialEq<Value> for str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for &str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for String {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for bool {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

macro_rules! eq_num {
    ($($t:ty)*) => {$(
        impl PartialEq<$t> for Value {
            /// Numeric comparison, see [`Number`] equality.
            fn eq(&self, other: &$t) -> bool {
                match self {
                    Value::Number(n) => *n == Number::from(*other),
                    _ => false,
                }
            }
        }
        impl PartialEq<Value> for $t {
            fn eq(&self, other: &Value) -> bool {
                other == self
            }
        }
    )*};
}
eq_num!(u8 u16 u32 u64 usize i8 i16 i32 i64 isize);

impl PartialEq<f64> for Value {
    /// Numeric comparison; never equal to NaN.
    fn eq(&self, other: &f64) -> bool {
        match (self, Number::from_f64(*other)) {
            (Value::Number(n), Some(o)) => *n == o,
            _ => false,
        }
    }
}

impl PartialEq<f32> for Value {
    /// Compares with the `f32` converted as by `From<f32>`.
    fn eq(&self, other: &f32) -> bool {
        *self == f32_to_f64(*other)
    }
}

impl PartialEq<Value> for f64 {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for f32 {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn rfc6901_examples() {
        // RFC 6901 section 5.
        let doc = crate::parse(
            r#"{
              "foo": ["bar", "baz"],
              "": 0,
              "a/b": 1,
              "c%d": 2,
              "e^f": 3,
              "g|h": 4,
              "i\\j": 5,
              "k\"l": 6,
              " ": 7,
              "m~n": 8
            }"#,
        )
        .unwrap();
        assert_eq!(doc.pointer(""), Some(&doc));
        assert_eq!(doc.pointer("/foo"), Some(&json!(["bar", "baz"])));
        assert_eq!(doc.pointer("/foo/0").unwrap(), "bar");
        let cases = [
            ("/", 0),
            ("/a~1b", 1),
            ("/c%d", 2),
            ("/e^f", 3),
            ("/g|h", 4),
            ("/i\\j", 5),
        ];
        for (p, n) in cases {
            assert_eq!(*doc.pointer(p).unwrap(), n, "{p}");
        }
        for (p, n) in [("/k\"l", 6), ("/ ", 7), ("/m~0n", 8)] {
            assert_eq!(*doc.pointer(p).unwrap(), n, "{p}");
        }
    }

    #[test]
    fn pointer_rejects_malformed() {
        let doc = json!({"a": [10, 20], "~": 1});
        for p in [
            "a", "/a/01", "/a/-", "/a/+1", "/a/2", "/~2", "/~", "/a/0/x", "/b",
        ] {
            assert!(doc.pointer(p).is_none(), "{p}");
        }
        assert_eq!(*doc.pointer("/~0").unwrap(), 1);
        let mut doc = doc;
        *doc.pointer_mut("/a/1").unwrap() = json!("x");
        assert_eq!(doc["a"][1], "x");
        assert!(doc.pointer_mut("/a/5").is_none());
    }

    #[test]
    fn accessors() {
        let mut v = json!({"s": "x", "b": true, "n": -5, "f": 1.5, "a": [1], "o": {}, "z": null});
        assert_eq!(v["s"].as_str(), Some("x"));
        assert_eq!(v["b"].as_bool(), Some(true));
        assert_eq!((v["n"].as_i64(), v["n"].as_u64()), (Some(-5), None));
        assert_eq!(v["f"].as_f64(), Some(1.5));
        assert_eq!(v["a"].as_array().map(Vec::len), Some(1));
        assert!(v["o"].as_object().unwrap().is_empty());
        assert!(v["z"].is_null() && v["nope"].is_null() && v[3].is_null() && v["a"][7].is_null());
        let names: Vec<_> = ["z", "b", "n", "s", "a", "o"]
            .iter()
            .map(|k| v[*k].type_name())
            .collect();
        assert_eq!(
            names,
            ["null", "boolean", "number", "string", "array", "object"]
        );
        assert!(v["b"].is_boolean() && v["n"].is_number() && v["s"].is_string());
        assert!(v["a"].is_array() && v["o"].is_object());
        assert_eq!(v.get_index(0), None);
        assert!(v.as_array_mut().is_none() && v.get_index_mut(0).is_none());
        v.get_mut("a")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!(2));
        *v.get_mut("a").unwrap().get_index_mut(1).unwrap() = json!(2);
        assert_eq!(v["a"].get_index(1), Some(&json!(2)));
        v.as_object_mut().unwrap().insert("new", 1);
        assert_eq!(v["new"], 1);
        let taken = v.get_mut("s").unwrap().take();
        assert_eq!(taken, "x");
        assert!(v["s"].is_null());
        assert_eq!(v["a"], json!([1, 2]));
        assert_eq!("7".parse::<Value>().unwrap(), 7);
    }

    #[test]
    fn conversions_and_equality() {
        assert_eq!(Value::from(f64::NAN), Value::Null);
        assert_eq!(Value::from(f32::INFINITY), Value::Null);
        assert_eq!(Value::from(0.1f32).as_f64(), Some(0.1));
        assert_eq!(Value::from(None::<i32>), Value::Null);
        assert_eq!(Value::from(Some("a")), "a");
        assert_eq!(Value::from(vec![1, 2]), json!([1, 2]));
        assert_eq!(Value::from(&[true, false][..]), json!([true, false]));
        assert_eq!(Value::from(String::from("s")), *"s");
        assert_eq!(Value::from(&String::from("s")), String::from("s"));
        assert_eq!(Value::from(Cow::Borrowed("c")), "c");
        assert_eq!(Value::from(u128::MAX).to_string(), u128::MAX.to_string());
        assert!(Value::from(1u8) == 1 && 1 == Value::from(1u8));
        assert!(Value::from(-1) == -1i64 && Value::from(3u64) == 3.0);
        let (half, yes, no, a, one_s, one_f) = (
            Value::from(0.5f32),
            Value::from(true),
            Value::from(false),
            Value::from("a"),
            Value::from("1"),
            Value::from(1.0),
        );
        assert!(half == 0.5f32 && 0.5f32 == half && 0.5 == half);
        assert!(yes == true && false == no);
        let owned = String::from("a");
        assert!("a" == a && owned == a && a == owned);
        assert!(one_s != 1 && one_f != "1" && one_f != f64::from_bits(0x7ff8_0000_0000_0000));
        assert_eq!(
            format!("{:#}", json!({"a": [1]})),
            "{\n  \"a\": [\n    1\n  ]\n}"
        );
    }

    #[test]
    fn field_conversion() {
        let v = json!({"qty": "3"});
        let err = v.field::<u32>("qty").unwrap_err();
        assert_eq!(err.path(), "/qty");
        assert_eq!(err.to_string(), "expected u32, found string at /qty");
        assert_eq!(
            json!(1).field::<u32>("x").unwrap_err().to_string(),
            "expected object, found number"
        );
        assert_eq!(v.field::<Value>("none").unwrap(), Value::Null);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn value_is_compact() {
        assert!(
            core::mem::size_of::<Value>() <= 40,
            "{}",
            core::mem::size_of::<Value>()
        );
    }
}
