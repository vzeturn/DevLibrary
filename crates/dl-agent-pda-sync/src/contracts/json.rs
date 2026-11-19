//! JSON (de)serialisation of the contracts with the C# contexts' rules: [`WireJson`] for
//! the snake_case source-generated contexts (`SyncJsonContext`, `PrintQueueJsonContext`,
//! `StaffJsonContext`, nulls omitted), helpers for writing members, and
//! [`ContractError`].

use dl_json::Value;

use super::map::StrMap;
use super::time::{DateTime, DateTimeOffset};
use crate::util::stj::{self, Obj, ReadError, ReadOptions, Style, Writer};

/// Why a contract could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Not JSON, or JSON System.Text.Json would refuse for this type (`JsonException`).
    Json(ReadError),
    /// The document is JSON `null` (the C# `Deserialize` returns null).
    Null,
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractError::Json(e) => e.fmt(f),
            ContractError::Null => f.write_str("The JSON document is null."),
        }
    }
}

impl std::error::Error for ContractError {}

impl From<ReadError> for ContractError {
    fn from(e: ReadError) -> Self {
        ContractError::Json(e)
    }
}

/// A DTO of a snake_case source-generated context (`SyncJsonContext` and friends):
/// members in C# declaration order, null members omitted (`WhenWritingNull`), dictionary
/// entries (including null values) in insertion order, the default encoder's escaping.
pub trait WireJson: Sized {
    /// Writes the object.
    fn write_json(&self, w: &mut Writer);

    /// Reads the object from a reader (missing members take their defaults).
    fn read_json(o: Obj<'_>) -> Result<Self, ReadError>;

    /// `JsonSerializer.Serialize(value, context)`: compact JSON.
    fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Compact);
        self.write_json(&mut w);
        w.finish()
    }

    /// `JsonSerializer.Deserialize(json, context)`.
    fn from_json(json: &str) -> Result<Self, ContractError> {
        let v = stj::parse(json)?;
        Self::from_value(&v)
    }

    /// Reads from a parsed value (`JsonElement.Deserialize`); JSON null is
    /// [`ContractError::Null`].
    fn from_value(v: &Value) -> Result<Self, ContractError> {
        match Obj::of(v, stj::SNAKE, "$")? {
            None => Err(ContractError::Null),
            Some(o) => Ok(Self::read_json(o)?),
        }
    }

    /// `JsonSerializer.SerializeToElement(value, context)`: the JSON as a value (numbers
    /// kept as written).
    fn to_element(&self) -> Value {
        // The writer only produces valid JSON, so parsing it back cannot fail.
        stj::parse(&self.to_json()).unwrap_or(Value::Null)
    }

    /// `SiteCommandJson.FromElement`: `None` when the value is not an object or not a
    /// valid instance (never an error).
    fn from_element(v: &Value) -> Option<Self> {
        if !v.is_object() {
            return None;
        }
        Self::from_value(v).ok()
    }
}

/// Reads a DTO with other options (e.g. [`stj::WEB`] for a camelCase API envelope);
/// JSON null is [`ContractError::Null`].
pub fn read_with<T: WireJson>(v: &Value, opts: ReadOptions) -> Result<T, ContractError> {
    match Obj::of(v, opts, "$")? {
        None => Err(ContractError::Null),
        Some(o) => Ok(T::read_json(o)?),
    }
}

/// Reads a list item that is a nested DTO (a null item reads as an error: the Rust port
/// has no null elements).
pub fn item<T: WireJson>(v: &Value, opts: ReadOptions, path: &str) -> Result<T, ReadError> {
    T::read_json(Obj::require(v, opts, path)?)
}

/// Reads a list item that is an integer.
pub fn int_item<T: TryFrom<i128>>(
    v: &Value,
    opts: ReadOptions,
    path: &str,
) -> Result<T, ReadError> {
    stj::read_int(v, opts, path)
}

/// Reads a list item that is a string (`null` items read as `""`).
pub fn string_item(v: &Value, _opts: ReadOptions, path: &str) -> Result<String, ReadError> {
    Ok(stj::read_opt_string(v, path)?.unwrap_or_default())
}

/// A `DateTimeOffset` member: `DateTimeOffset.MinValue` when missing, error when null.
pub fn dto(o: &Obj<'_>, snake: &str) -> Result<DateTimeOffset, ReadError> {
    match o.iso(snake)? {
        None => Ok(DateTimeOffset::MIN),
        Some(iso) => DateTimeOffset::from_iso(iso).ok_or_else(|| {
            ReadError::new(&o.path_of(snake), "The DateTimeOffset is out of range.")
        }),
    }
}

/// A `DateTimeOffset?` member.
pub fn opt_dto(o: &Obj<'_>, snake: &str) -> Result<Option<DateTimeOffset>, ReadError> {
    match o.opt_iso(snake)? {
        None => Ok(None),
        Some(iso) => DateTimeOffset::from_iso(iso).map(Some).ok_or_else(|| {
            ReadError::new(&o.path_of(snake), "The DateTimeOffset is out of range.")
        }),
    }
}

/// A `DateTime` member: `DateTime.MinValue` when missing, error when null.
pub fn date_time(o: &Obj<'_>, snake: &str) -> Result<DateTime, ReadError> {
    match o.iso(snake)? {
        None => Ok(DateTime::default()),
        Some(iso) => DateTime::from_iso(iso)
            .ok_or_else(|| ReadError::new(&o.path_of(snake), "The DateTime is out of range.")),
    }
}

/// A `Dictionary<string, string?>` member as a [`StrMap`].
pub fn str_map(o: &Obj<'_>, snake: &str) -> Result<StrMap, ReadError> {
    Ok(o.str_dict(snake)?.into_iter().collect())
}

/// A `JsonElement` member: JSON null when missing (C#: an `Undefined` element).
pub fn element(o: &Obj<'_>, snake: &str) -> Value {
    o.get(snake).cloned().unwrap_or(Value::Null)
}

/// A `JsonElement?` member: `None` when missing or null.
pub fn opt_element(o: &Obj<'_>, snake: &str) -> Option<Value> {
    o.get_non_null(snake).cloned()
}

/// Writes a dictionary of nullable strings (nulls written, insertion order).
pub fn write_str_map(w: &mut Writer, m: &StrMap) {
    w.begin_object();
    for (k, v) in m.iter() {
        w.key(k);
        match v {
            Some(s) => w.string(s),
            None => w.null(),
        };
    }
    w.end_object();
}

/// Writes a list of strings.
pub fn write_strings(w: &mut Writer, items: &[String]) {
    w.begin_array();
    for s in items {
        w.string(s);
    }
    w.end_array();
}

/// Writes a list of integers.
pub fn write_ints<T: Copy + Into<i64>>(w: &mut Writer, items: &[T]) {
    w.begin_array();
    for n in items {
        w.int((*n).into());
    }
    w.end_array();
}

/// Writes a list of DTOs.
pub fn write_items<T: WireJson>(w: &mut Writer, items: &[T]) {
    w.begin_array();
    for x in items {
        x.write_json(w);
    }
    w.end_array();
}

/// `name: "text"` when `Some` (`WhenWritingNull`).
pub fn opt_str(w: &mut Writer, name: &str, v: &Option<String>) {
    if let Some(s) = v {
        w.key(name).string(s);
    }
}

/// `name: n` when `Some`.
pub fn opt_int(w: &mut Writer, name: &str, v: Option<i64>) {
    if let Some(n) = v {
        w.key(name).int(n);
    }
}

/// `name: true|false` when `Some`.
pub fn opt_bool(w: &mut Writer, name: &str, v: Option<bool>) {
    if let Some(b) = v {
        w.key(name).bool(b);
    }
}

/// `name: "date"`: a `DateTimeOffset` member as System.Text.Json writes it (the `+` of
/// the offset is not escaped, unlike `+` in a string).
pub fn dto_member(w: &mut Writer, name: &str, t: DateTimeOffset) {
    w.key(name).unescaped(&t.to_json_string());
}

/// `name: "date"` when `Some`.
pub fn opt_dto_member(w: &mut Writer, name: &str, v: Option<DateTimeOffset>) {
    if let Some(t) = v {
        dto_member(w, name, t);
    }
}

/// `name: <element>` when `Some`.
pub fn opt_value(w: &mut Writer, name: &str, v: &Option<Value>) {
    if let Some(x) = v {
        w.key(name).value(x);
    }
}

/// Declares a C# enum written with `JsonStringEnumConverter` (C# member names, read
/// case-insensitively, also from numbers).
macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $variant:ident = $value:expr),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vm])* $variant),+
        }

        impl $name {
            /// Every member with its C# name and value.
            pub const MEMBERS: &'static [(&'static str, i64, $name)] =
                &[$((stringify!($variant), $value, $name::$variant)),+];

            /// The C# member name (the JSON text).
            pub fn name(self) -> &'static str {
                match self {
                    $($name::$variant => stringify!($variant)),+
                }
            }

            /// Reads the JSON value with `JsonStringEnumConverter` rules.
            pub fn read(
                v: &dl_json::Value,
                _opts: crate::util::stj::ReadOptions,
                path: &str,
            ) -> Result<Self, crate::util::stj::ReadError> {
                crate::util::stj::read_enum(v, path, Self::MEMBERS)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

pub(crate) use string_enum;

/// Reads an enum member: the first member (value 0) when missing, error when null.
pub fn enum_member<T: Copy>(
    o: &Obj<'_>,
    snake: &str,
    members: &[(&str, i64, T)],
) -> Result<T, ReadError> {
    match o.get(snake) {
        None => members
            .iter()
            .find(|m| m.1 == 0)
            .map(|m| m.2)
            .ok_or_else(|| ReadError::new(&o.path_of(snake), "missing enum value")),
        Some(v) => stj::read_enum(v, &o.path_of(snake), members),
    }
}
