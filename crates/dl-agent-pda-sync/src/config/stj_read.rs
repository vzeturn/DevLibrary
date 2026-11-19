//! Reading JSON the way `JsonSerializer.Deserialize` does with `JsonSerializerDefaults.Web`
//! for the small agent files: case-insensitive property names (last duplicate wins),
//! strings only from JSON strings, `int` from integer numbers or (Web's
//! `AllowReadingFromString`) from strings holding one, and a typed error otherwise.

use super::time::Timestamp;
use dl_json::{Map, Value};
use std::fmt;

/// What `JsonException` would report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError(pub String);

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JsonError {}

/// Parses `text` (a UTF-8 BOM is skipped like `File.ReadAllText`) keeping number text.
pub fn parse(text: &str) -> Result<Value, JsonError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    dl_json::parse_with(
        text,
        dl_json::ParseOptions {
            keep_raw_numbers: true,
            max_depth: 64,
        },
    )
    .map_err(|e| JsonError(e.to_string()))
}

/// The root as an object; `Ok(None)` for `null` (the serializer returns null).
pub fn root_object(v: &Value) -> Result<Option<&Map>, JsonError> {
    match v {
        Value::Null => Ok(None),
        Value::Object(m) => Ok(Some(m)),
        other => Err(JsonError(format!(
            "The JSON value of kind {} could not be converted to an object.",
            other.type_name()
        ))),
    }
}

/// The member whose name equals `name` ignoring case; the last one when repeated.
pub fn get_ci<'a>(m: &'a Map, name: &str) -> Option<&'a Value> {
    m.iter().rev().find(|(k, _)| eq_ci(k, name)).map(|(_, v)| v)
}

/// A `string?` member: null or a JSON string.
pub fn opt_string(v: Option<&Value>, path: &str) -> Result<Option<String>, JsonError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(JsonError(format!(
            "The JSON value could not be converted to System.String. Path: $.{path}"
        ))),
    }
}

/// An `int` from an integer JSON number, or from a string holding one (`AllowReadingFromString`).
pub fn int32(v: &Value, path: &str) -> Result<i32, JsonError> {
    let bad = || {
        JsonError(format!(
            "The JSON value could not be converted to System.Int32. Path: $.{path}"
        ))
    };
    let text = match v {
        Value::Number(n) => n
            .as_raw_str()
            .map(str::to_owned)
            .unwrap_or_else(|| n.to_string()),
        Value::String(s) => s.clone(),
        _ => return Err(bad()),
    };
    parse_int_strict(&text).ok_or_else(bad)
}

/// `Utf8Parser.TryParse` for `int` over the whole text: optional `-`, digits, nothing else.
fn parse_int_strict(text: &str) -> Option<i32> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i32>().ok()
}

/// `StringComparer.OrdinalIgnoreCase` equality: per UTF-16 unit simple upper-casing (here
/// per char, the same for every name the agent uses).
pub(crate) fn eq_ci(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars().count() == b.chars().count()
        && a.chars().zip(b.chars()).all(|(x, y)| upper(x) == upper(y))
}

fn upper(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// A `Guid` from a JSON string in the `D` format (36 characters, any hex case).
pub fn guid(v: &Value, path: &str) -> Result<dl_uuid::Uuid, JsonError> {
    let bad = || {
        JsonError(format!(
            "The JSON value could not be converted to System.Guid. Path: $.{path}"
        ))
    };
    match v {
        Value::String(s) if s.len() == 36 => dl_uuid::Uuid::parse(s).map_err(|_| bad()),
        _ => Err(bad()),
    }
}

/// A `DateTimeOffset` from a JSON string in ISO 8601 form.
pub fn timestamp(v: &Value, path: &str) -> Result<Timestamp, JsonError> {
    match v {
        Value::String(s) => Timestamp::parse_iso(s).ok_or_else(|| {
            JsonError(format!(
                "The JSON value could not be converted to System.DateTimeOffset. Path: $.{path}"
            ))
        }),
        _ => Err(JsonError(format!(
            "The JSON value could not be converted to System.DateTimeOffset. Path: $.{path}"
        ))),
    }
}

/// `JsonElement.ToString()`: string values as is, `True`/`False`, empty for null, the
/// source text of numbers, objects and arrays.
pub fn element_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n
            .as_raw_str()
            .map(str::to_owned)
            .unwrap_or_else(|| n.to_string()),
        other => {
            let mut w = crate::util::stj::Writer::new(crate::util::stj::Style::Compact);
            w.value(other);
            w.finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_follow_the_web_defaults() {
        let v =
            parse(r#"{"a":5,"b":"7","c":1.0,"d":"1.5","e":-0,"f":99999999999,"g":null}"#).unwrap();
        let m = root_object(&v).unwrap().unwrap();
        assert_eq!(int32(get_ci(m, "A").unwrap(), "a"), Ok(5));
        assert_eq!(int32(get_ci(m, "b").unwrap(), "b"), Ok(7));
        assert!(int32(get_ci(m, "c").unwrap(), "c").is_err());
        assert!(int32(get_ci(m, "d").unwrap(), "d").is_err());
        assert_eq!(int32(get_ci(m, "e").unwrap(), "e"), Ok(0));
        assert!(int32(get_ci(m, "f").unwrap(), "f").is_err());
        assert!(int32(get_ci(m, "g").unwrap(), "g").is_err());
    }
}
