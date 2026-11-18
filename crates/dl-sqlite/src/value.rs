//! SQLite values and the conversions between them and Rust types.

use core::fmt;

use crate::Error;

/// The storage class of a value (`sqlite3_column_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// `NULL`.
    Null,
    /// A signed 64-bit integer.
    Integer,
    /// A 64-bit IEEE float.
    Real,
    /// Text (UTF-8 in this API).
    Text,
    /// Bytes.
    Blob,
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::Null => "NULL",
            Type::Integer => "INTEGER",
            Type::Real => "REAL",
            Type::Text => "TEXT",
            Type::Blob => "BLOB",
        })
    }
}

/// An owned SQLite value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `NULL`.
    Null,
    /// A signed 64-bit integer.
    Integer(i64),
    /// A 64-bit float.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// Bytes.
    Blob(Vec<u8>),
}

impl Value {
    /// The storage class of the value.
    pub fn data_type(&self) -> Type {
        self.as_value_ref().data_type()
    }

    /// A borrowed view of the value.
    pub fn as_value_ref(&self) -> ValueRef<'_> {
        match self {
            Value::Null => ValueRef::Null,
            Value::Integer(v) => ValueRef::Integer(*v),
            Value::Real(v) => ValueRef::Real(*v),
            Value::Text(s) => ValueRef::Text(s.as_bytes()),
            Value::Blob(b) => ValueRef::Blob(b),
        }
    }
}

/// A value borrowed from a row (or from a Rust value being bound).
///
/// `Text` holds the raw bytes as SQLite stores them; they are UTF-8 unless the database
/// holds text written as something else (e.g. `CAST(x'ff' AS TEXT)`). Converting to
/// `String` checks them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ValueRef<'a> {
    /// `NULL`.
    Null,
    /// A signed 64-bit integer.
    Integer(i64),
    /// A 64-bit float.
    Real(f64),
    /// Text bytes (not validated).
    Text(&'a [u8]),
    /// Bytes.
    Blob(&'a [u8]),
}

impl ValueRef<'_> {
    /// The storage class of the value.
    pub fn data_type(&self) -> Type {
        match self {
            ValueRef::Null => Type::Null,
            ValueRef::Integer(_) => Type::Integer,
            ValueRef::Real(_) => Type::Real,
            ValueRef::Text(_) => Type::Text,
            ValueRef::Blob(_) => Type::Blob,
        }
    }
}

/// A Rust value that can be bound to a statement parameter.
///
/// Text and blobs are bound with `SQLITE_TRANSIENT` (SQLite copies them) and an explicit
/// length, so strings need no NUL terminator and may contain NUL characters.
pub trait ToSql {
    /// The value to bind.
    fn to_sql(&self) -> ValueRef<'_>;
}

/// A Rust value that can be read from a column.
pub trait FromSql: Sized {
    /// Converts a column value. Implementations report column 0 in errors;
    /// [`Row::get`](crate::Row::get) fills in the real index.
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error>;
}

fn invalid(expected: Type, value: ValueRef<'_>) -> Error {
    Error::InvalidType {
        column: 0,
        expected,
        found: value.data_type(),
    }
}

impl<T: ToSql + ?Sized> ToSql for &T {
    fn to_sql(&self) -> ValueRef<'_> {
        (**self).to_sql()
    }
}

impl<T: ToSql> ToSql for Option<T> {
    fn to_sql(&self) -> ValueRef<'_> {
        match self {
            Some(v) => v.to_sql(),
            None => ValueRef::Null,
        }
    }
}

macro_rules! int_to_sql {
    ($($t:ty),*) => {$(
        impl ToSql for $t {
            fn to_sql(&self) -> ValueRef<'_> {
                ValueRef::Integer(i64::from(*self))
            }
        }
    )*};
}
int_to_sql!(i8, i16, i32, i64, u8, u16, u32);

impl ToSql for bool {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Integer(i64::from(*self))
    }
}

impl ToSql for f64 {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Real(*self)
    }
}

impl ToSql for f32 {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Real(f64::from(*self))
    }
}

impl ToSql for str {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Text(self.as_bytes())
    }
}

impl ToSql for String {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Text(self.as_bytes())
    }
}

impl ToSql for [u8] {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Blob(self)
    }
}

impl<const N: usize> ToSql for [u8; N] {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Blob(self)
    }
}

impl ToSql for Vec<u8> {
    fn to_sql(&self) -> ValueRef<'_> {
        ValueRef::Blob(self)
    }
}

impl ToSql for Value {
    fn to_sql(&self) -> ValueRef<'_> {
        self.as_value_ref()
    }
}

impl ToSql for ValueRef<'_> {
    fn to_sql(&self) -> ValueRef<'_> {
        *self
    }
}

impl FromSql for i64 {
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        match value {
            ValueRef::Integer(v) => Ok(v),
            other => Err(invalid(Type::Integer, other)),
        }
    }
}

macro_rules! int_from_sql {
    ($($t:ty),*) => {$(
        impl FromSql for $t {
            fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
                let v = i64::from_sql(value)?;
                <$t>::try_from(v).map_err(|_| Error::OutOfRange { column: 0, value: v })
            }
        }
    )*};
}
int_from_sql!(i8, i16, i32, u8, u16, u32, u64, usize);

impl FromSql for bool {
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        i64::from_sql(value).map(|v| v != 0)
    }
}

impl FromSql for f64 {
    /// Reads REAL, and INTEGER converted to the nearest float.
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        match value {
            ValueRef::Real(v) => Ok(v),
            ValueRef::Integer(v) => Ok(v as f64),
            other => Err(invalid(Type::Real, other)),
        }
    }
}

impl FromSql for String {
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        match value {
            ValueRef::Text(b) => text(b).map(str::to_owned),
            other => Err(invalid(Type::Text, other)),
        }
    }
}

impl FromSql for Vec<u8> {
    /// Reads BLOB, and the bytes of TEXT.
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        match value {
            ValueRef::Blob(b) | ValueRef::Text(b) => Ok(b.to_vec()),
            other => Err(invalid(Type::Blob, other)),
        }
    }
}

impl FromSql for Value {
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        Ok(match value {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(v) => Value::Integer(v),
            ValueRef::Real(v) => Value::Real(v),
            ValueRef::Text(b) => Value::Text(text(b)?.to_owned()),
            ValueRef::Blob(b) => Value::Blob(b.to_vec()),
        })
    }
}

impl<T: FromSql> FromSql for Option<T> {
    fn from_sql(value: ValueRef<'_>) -> Result<Self, Error> {
        match value {
            ValueRef::Null => Ok(None),
            other => T::from_sql(other).map(Some),
        }
    }
}

fn text(b: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(b).map_err(|e| Error::Utf8 {
        column: 0,
        valid_up_to: e.valid_up_to(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(i32::from_sql(ValueRef::Integer(-5)), Ok(-5));
        assert_eq!(
            u32::from_sql(ValueRef::Integer(-1)),
            Err(Error::OutOfRange {
                column: 0,
                value: -1
            })
        );
        assert_eq!(bool::from_sql(ValueRef::Integer(2)), Ok(true));
        assert_eq!(f64::from_sql(ValueRef::Integer(3)), Ok(3.0));
        assert_eq!(
            String::from_sql(ValueRef::Text(&[0x66, 0xff])),
            Err(Error::Utf8 {
                column: 0,
                valid_up_to: 1
            })
        );
        assert_eq!(
            i64::from_sql(ValueRef::Text(b"1")),
            Err(Error::InvalidType {
                column: 0,
                expected: Type::Integer,
                found: Type::Text
            })
        );
        assert_eq!(Option::<i64>::from_sql(ValueRef::Null), Ok(None));
        assert_eq!(
            Vec::<u8>::from_sql(ValueRef::Text(b"ab")),
            Ok(b"ab".to_vec())
        );
        assert_eq!(Some("x").to_sql(), ValueRef::Text(b"x"));
        assert_eq!(None::<i64>.to_sql(), ValueRef::Null);
        assert_eq!(true.to_sql(), ValueRef::Integer(1));
        assert_eq!(u32::MAX.to_sql(), ValueRef::Integer(4_294_967_295));
        assert_eq!(Value::Text("é".into()).data_type(), Type::Text);
    }
}
