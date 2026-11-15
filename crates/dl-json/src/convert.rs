//! `ToJson` / `FromJson` conversion traits.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::{Error, Map, Number, Value};

/// Converts a Rust value to a JSON [`Value`].
///
/// Implement it for your own types, usually with [`json!`](crate::json):
///
/// ```
/// use dl_json::{ToJson, Value, json};
///
/// struct Room { number: u16, rate: f64 }
///
/// impl ToJson for Room {
///     fn to_json(&self) -> Value {
///         json!({"number": self.number, "rate": self.rate})
///     }
/// }
///
/// let rooms = vec![Room { number: 101, rate: 85.5 }];
/// assert_eq!(dl_json::to_string_of(&rooms), r#"[{"number":101,"rate":85.5}]"#);
/// ```
pub trait ToJson {
    /// Builds the JSON representation of `self`.
    fn to_json(&self) -> Value;
}

/// Builds a Rust value from a JSON [`Value`].
///
/// Built-in implementations are strict: integers must be JSON integers within the
/// target range (a float such as `3.0` is rejected), strings must be strings, and so on.
/// Use [`Value::field`] to read object members with the member name in errors.
///
/// ```
/// use dl_json::{Error, FromJson, Value};
///
/// struct Guest { name: String, vip: bool }
///
/// impl FromJson for Guest {
///     fn from_json(v: &Value) -> Result<Self, Error> {
///         Ok(Guest { name: v.field("name")?, vip: v.field::<Option<bool>>("vip")?.unwrap_or(false) })
///     }
/// }
///
/// let g: Guest = dl_json::from_str(r#"{"name": "Trần Thị B"}"#).unwrap();
/// assert_eq!((g.name.as_str(), g.vip), ("Trần Thị B", false));
/// ```
pub trait FromJson: Sized {
    /// Converts `value`, or explains why it does not fit.
    fn from_json(value: &Value) -> Result<Self, Error>;
}

/// Parses `input` and converts it with [`FromJson`].
///
/// ```
/// let totals: Vec<u64> = dl_json::from_str("[1200, 3400]").unwrap();
/// assert_eq!(totals, [1200, 3400]);
/// ```
pub fn from_str<T: FromJson>(input: &str) -> Result<T, Error> {
    T::from_json(&crate::parse(input)?)
}

/// Converts `value` with [`ToJson`] and serializes it compactly.
pub fn to_string_of<T: ToJson + ?Sized>(value: &T) -> String {
    crate::to_string(&value.to_json())
}

// ---- ToJson ----

impl<T: ToJson + ?Sized> ToJson for &T {
    fn to_json(&self) -> Value {
        (**self).to_json()
    }
}

impl<T: ToJson + ?Sized> ToJson for &mut T {
    fn to_json(&self) -> Value {
        (**self).to_json()
    }
}

impl<T: ToJson + ?Sized> ToJson for Box<T> {
    fn to_json(&self) -> Value {
        (**self).to_json()
    }
}

impl ToJson for Value {
    fn to_json(&self) -> Value {
        self.clone()
    }
}

impl ToJson for Map {
    fn to_json(&self) -> Value {
        Value::Object(self.clone())
    }
}

impl ToJson for Number {
    fn to_json(&self) -> Value {
        Value::Number(self.clone())
    }
}

impl ToJson for bool {
    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }
}

impl ToJson for str {
    fn to_json(&self) -> Value {
        Value::String(self.into())
    }
}

impl ToJson for String {
    fn to_json(&self) -> Value {
        Value::String(self.clone())
    }
}

macro_rules! to_json_via_from {
    ($($t:ty)*) => {$(
        impl ToJson for $t {
            fn to_json(&self) -> Value {
                Value::from(*self)
            }
        }
    )*};
}
// Non-finite floats become `null`, see `From<f64> for Value`.
to_json_via_from!(u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64);

impl<T: ToJson> ToJson for Option<T> {
    fn to_json(&self) -> Value {
        self.as_ref().map_or(Value::Null, ToJson::to_json)
    }
}

impl<T: ToJson> ToJson for [T] {
    fn to_json(&self) -> Value {
        Value::Array(self.iter().map(ToJson::to_json).collect())
    }
}

impl<T: ToJson, const LEN: usize> ToJson for [T; LEN] {
    fn to_json(&self) -> Value {
        self.as_slice().to_json()
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self) -> Value {
        self.as_slice().to_json()
    }
}

impl<K: AsRef<str>, T: ToJson> ToJson for BTreeMap<K, T> {
    fn to_json(&self) -> Value {
        Value::Object(
            self.iter()
                .map(|(k, v)| (k.as_ref(), v.to_json()))
                .collect(),
        )
    }
}

// ---- FromJson ----

impl FromJson for Value {
    fn from_json(value: &Value) -> Result<Self, Error> {
        Ok(value.clone())
    }
}

impl FromJson for Map {
    fn from_json(value: &Value) -> Result<Self, Error> {
        value
            .as_object()
            .cloned()
            .ok_or_else(|| Error::type_mismatch("object", value.type_name()))
    }
}

impl FromJson for Number {
    fn from_json(value: &Value) -> Result<Self, Error> {
        value
            .as_number()
            .cloned()
            .ok_or_else(|| Error::type_mismatch("number", value.type_name()))
    }
}

impl FromJson for bool {
    fn from_json(value: &Value) -> Result<Self, Error> {
        value
            .as_bool()
            .ok_or_else(|| Error::type_mismatch("boolean", value.type_name()))
    }
}

impl FromJson for String {
    fn from_json(value: &Value) -> Result<Self, Error> {
        match value {
            Value::String(s) => Ok(s.clone()),
            v => Err(Error::type_mismatch("string", v.type_name())),
        }
    }
}

impl FromJson for f64 {
    /// Accepts any number; integers above 2^53 are rounded.
    fn from_json(value: &Value) -> Result<Self, Error> {
        match value {
            Value::Number(n) => n
                .as_f64()
                .ok_or(Error::type_mismatch("f64", "out-of-range number")),
            v => Err(Error::type_mismatch("f64", v.type_name())),
        }
    }
}

impl FromJson for f32 {
    /// Accepts any number whose magnitude fits in `f32` (precision is rounded).
    fn from_json(value: &Value) -> Result<Self, Error> {
        match value {
            Value::Number(n) => n
                .as_f64()
                .map(|f| f as f32)
                .filter(|f| f.is_finite())
                .ok_or(Error::type_mismatch("f32", "out-of-range number")),
            v => Err(Error::type_mismatch("f32", v.type_name())),
        }
    }
}

/// Describes why a number is not an integer of the wanted range.
fn int_mismatch(n: &Number, expected: &'static str) -> Error {
    let integral = match n.as_raw_str() {
        Some(s) => !s.bytes().any(|b| matches!(b, b'.' | b'e' | b'E')),
        None => !n.is_f64(),
    };
    let found = if integral {
        "out-of-range integer"
    } else {
        "float"
    };
    Error::type_mismatch(expected, found)
}

macro_rules! from_json_int {
    ($($t:ty, $wide:ty, $as:ident;)*) => {$(
        impl FromJson for $t {
            /// Accepts JSON integers within range; rejects floats and other types.
            fn from_json(value: &Value) -> Result<Self, Error> {
                let Value::Number(n) = value else {
                    return Err(Error::type_mismatch(stringify!($t), value.type_name()));
                };
                n.$as()
                    .and_then(|w: $wide| <$t>::try_from(w).ok())
                    .ok_or_else(|| int_mismatch(n, stringify!($t)))
            }
        }
    )*};
}
from_json_int! {
    u8, u64, as_u64; u16, u64, as_u64; u32, u64, as_u64; u64, u64, as_u64; usize, u64, as_u64;
    i8, i64, as_i64; i16, i64, as_i64; i32, i64, as_i64; i64, i64, as_i64; isize, i64, as_i64;
}

impl FromJson for u128 {
    /// Accepts JSON integers within range, including raw numbers beyond 64 bits.
    fn from_json(value: &Value) -> Result<Self, Error> {
        let Value::Number(n) = value else {
            return Err(Error::type_mismatch("u128", value.type_name()));
        };
        n.as_u64()
            .map(u128::from)
            .or_else(|| n.as_raw_str()?.parse().ok())
            .ok_or_else(|| int_mismatch(n, "u128"))
    }
}

impl FromJson for i128 {
    /// Accepts JSON integers within range, including raw numbers beyond 64 bits.
    fn from_json(value: &Value) -> Result<Self, Error> {
        let Value::Number(n) = value else {
            return Err(Error::type_mismatch("i128", value.type_name()));
        };
        n.as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .or_else(|| n.as_raw_str()?.parse().ok())
            .ok_or_else(|| int_mismatch(n, "i128"))
    }
}

impl<T: FromJson> FromJson for Option<T> {
    /// `null` becomes `None`; anything else is converted with `T`.
    fn from_json(value: &Value) -> Result<Self, Error> {
        match value {
            Value::Null => Ok(None),
            v => T::from_json(v).map(Some),
        }
    }
}

impl<T: FromJson> FromJson for Box<T> {
    fn from_json(value: &Value) -> Result<Self, Error> {
        T::from_json(value).map(Box::new)
    }
}

impl<T: FromJson> FromJson for Vec<T> {
    /// Converts every element; errors carry the element index in their path.
    fn from_json(value: &Value) -> Result<Self, Error> {
        let Value::Array(a) = value else {
            return Err(Error::type_mismatch("array", value.type_name()));
        };
        a.iter()
            .enumerate()
            .map(|(i, v)| T::from_json(v).map_err(|e| e.in_index(i)))
            .collect()
    }
}

impl<T: FromJson> FromJson for BTreeMap<String, T> {
    /// Converts every member; errors carry the member name in their path.
    fn from_json(value: &Value) -> Result<Self, Error> {
        let Value::Object(m) = value else {
            return Err(Error::type_mismatch("object", value.type_name()));
        };
        m.iter()
            .map(|(k, v)| Ok((k.clone(), T::from_json(v).map_err(|e| e.in_field(k))?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorKind, ParseOptions, json, parse, parse_with};
    use alloc::string::ToString;
    use alloc::vec;

    #[derive(Debug, PartialEq)]
    struct Line {
        sku: String,
        qty: u32,
        unit_price: f64,
    }

    #[derive(Debug, PartialEq)]
    struct Invoice {
        id: String,
        customer: Option<String>,
        lines: Vec<Line>,
    }

    impl ToJson for Line {
        fn to_json(&self) -> Value {
            json!({"sku": self.sku, "qty": self.qty, "unit_price": self.unit_price})
        }
    }

    impl FromJson for Line {
        fn from_json(v: &Value) -> Result<Self, Error> {
            Ok(Line {
                sku: v.field("sku")?,
                qty: v.field("qty")?,
                unit_price: v.field("unit_price")?,
            })
        }
    }

    impl ToJson for Invoice {
        fn to_json(&self) -> Value {
            json!({"id": self.id, "customer": self.customer, "lines": self.lines})
        }
    }

    impl FromJson for Invoice {
        fn from_json(v: &Value) -> Result<Self, Error> {
            Ok(Invoice {
                id: v.field("id")?,
                customer: v.field("customer")?,
                lines: v.field("lines")?,
            })
        }
    }

    #[test]
    fn user_struct_round_trip() {
        let inv = Invoice {
            id: "INV-7".into(),
            customer: None,
            lines: vec![
                Line {
                    sku: "COFFEE".into(),
                    qty: 2,
                    unit_price: 3.5,
                },
                Line {
                    sku: "TEA \"green\"".into(),
                    qty: 1,
                    unit_price: 2.25,
                },
            ],
        };
        let s = to_string_of(&inv);
        assert_eq!(
            s,
            r#"{"id":"INV-7","customer":null,"lines":[{"sku":"COFFEE","qty":2,"unit_price":3.5},{"sku":"TEA \"green\"","qty":1,"unit_price":2.25}]}"#
        );
        assert_eq!(from_str::<Invoice>(&s).unwrap(), inv);

        let bad = r#"{"id": "X", "lines": [{"sku": "A", "qty": 1, "unit_price": 1}, {"sku": "B", "qty": -1, "unit_price": 1}]}"#;
        let err = from_str::<Invoice>(bad).unwrap_err();
        assert_eq!(err.path(), "/lines/1/qty");
        assert_eq!(
            err.to_string(),
            "expected u32, found out-of-range integer at /lines/1/qty"
        );
        assert_eq!(parse(bad).unwrap().pointer(err.path()), Some(&json!(-1)));
        let err = from_str::<Invoice>(r#"{"lines": []}"#).unwrap_err();
        assert_eq!(err.kind(), &ErrorKind::MissingField("id".into()));
        assert!(from_str::<Invoice>("{").unwrap_err().is_syntax());
    }

    #[test]
    fn integer_ranges() {
        assert_eq!(u8::from_json(&json!(255)).unwrap(), 255);
        assert!(u8::from_json(&json!(256)).is_err());
        assert!(u8::from_json(&json!(-1)).is_err());
        assert_eq!(i8::from_json(&json!(-128)).unwrap(), -128);
        assert!(i8::from_json(&json!(128)).is_err());
        assert_eq!(u64::from_json(&json!(u64::MAX)).unwrap(), u64::MAX);
        assert_eq!(i64::from_json(&json!(i64::MIN)).unwrap(), i64::MIN);
        assert!(i64::from_json(&json!(u64::MAX)).is_err());
        assert_eq!(
            u32::from_json(&json!(1.5)).unwrap_err().kind(),
            &ErrorKind::Type {
                expected: "u32",
                found: "float"
            }
        );
        assert!(u32::from_json(&json!(3.0)).is_err());
        assert_eq!(
            u32::from_json(&json!("3")).unwrap_err().kind(),
            &ErrorKind::Type {
                expected: "u32",
                found: "string"
            }
        );
        let raw = ParseOptions {
            keep_raw_numbers: true,
            ..Default::default()
        };
        let v = parse_with(
            "[300, 340282366920938463463374607431768211455, -5, 2.5]",
            raw,
        )
        .unwrap();
        assert_eq!(u16::from_json(&v[0]).unwrap(), 300);
        assert_eq!(u128::from_json(&v[1]).unwrap(), u128::MAX);
        assert!(u64::from_json(&v[1]).is_err());
        assert_eq!(i128::from_json(&v[2]).unwrap(), -5);
        assert_eq!(i128::from_json(&json!(u64::MAX)).unwrap(), u64::MAX as i128);
        assert!(i8::from_json(&v[3]).is_err());
        assert_eq!(f64::from_json(&v[3]).unwrap(), 2.5);
    }

    #[test]
    fn floats_strings_containers() {
        assert_eq!(f64::from_json(&json!(7)).unwrap(), 7.0);
        assert_eq!(f32::from_json(&json!(0.1)).unwrap(), 0.1f32);
        assert!(f32::from_json(&json!(1e300)).is_err());
        assert!(f64::from_json(&json!(null)).is_err());
        assert!(bool::from_json(&json!(true)).unwrap());
        assert!(bool::from_json(&json!(1)).is_err());
        assert_eq!(String::from_json(&json!("x")).unwrap(), "x");
        assert_eq!(Option::<u8>::from_json(&json!(null)).unwrap(), None);
        assert_eq!(Option::<u8>::from_json(&json!(4)).unwrap(), Some(4));
        assert_eq!(*Box::<u8>::from_json(&json!(4)).unwrap(), 4);
        let m: BTreeMap<String, u32> = from_str(r#"{"b": 2, "a": 1}"#).unwrap();
        assert_eq!(m.to_json(), json!({"a": 1, "b": 2}));
        let err = BTreeMap::<String, u32>::from_json(&json!({"a/b": "x"})).unwrap_err();
        assert_eq!(err.path(), "/a~1b");
        assert!(Vec::<u8>::from_json(&json!({})).is_err());
        assert_eq!(Map::from_json(&json!({"k": 1})).unwrap().len(), 1);
        assert!(Map::from_json(&json!([])).is_err());
        assert_eq!(Number::from_json(&json!(1)).unwrap(), Number::from(1u8));
        assert!(Number::from_json(&json!("1")).is_err());
        assert_eq!(Value::from_json(&json!([1])).unwrap(), json!([1]));
    }

    #[test]
    fn to_json_impls() {
        assert_eq!(true.to_json(), json!(true));
        assert_eq!("s".to_json(), json!("s"));
        assert_eq!(String::from("s").to_json(), json!("s"));
        assert_eq!(f64::NAN.to_json(), Value::Null);
        assert_eq!(0.1f32.to_json().to_string(), "0.1");
        assert_eq!([1u8, 2].to_json(), json!([1, 2]));
        assert_eq!(vec![Some(1), None].to_json(), json!([1, null]));
        let boxed = Box::new(5i16);
        assert_eq!(boxed.to_json(), json!(5));
        let mut five = 5u8;
        let five_ref = &mut five;
        assert_eq!(ToJson::to_json(&five_ref), json!(5));
        assert_eq!(Number::from(9u8).to_json(), json!(9));
        assert_eq!(Map::new().to_json(), json!({}));
        assert_eq!(to_string_of("a\"b"), r#""a\"b""#);
        assert_eq!(to_string_of(&[1.5f64, 2.0]), "[1.5,2]");
    }
}
