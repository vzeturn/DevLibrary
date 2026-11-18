//! Decoding with the rules of `System.Text.Json` and `DeviceJson.Options` / `Envelope`
//! (web defaults): case-insensitive member names, numbers also read from strings, missing
//! members keep their default, `null` for a reference type is `None`, the last duplicate
//! wins, and in strict mode an unknown member is an error. Errors carry the System.Text.Json
//! message, including `Path` and `BytePositionInLine` computed on the compact form of the
//! payload (what the API sends).

use std::fmt::{self, Write as _};

use dl_json::{Map, Value};
use dl_time::{Date, DateTime, Time};

use crate::config::{DotnetError, eq_ignore_case};

/// A payload System.Text.Json would refuse (the C# `JsonException`); the processor answers
/// it as DEVICE_BAD_PAYLOAD with [`PayloadError::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadError {
    message: String,
    inner: Option<DotnetError>,
}

impl PayloadError {
    /// An error with this exact message.
    pub fn new(message: impl Into<String>) -> PayloadError {
        PayloadError {
            message: message.into(),
            inner: None,
        }
    }

    /// This error with the reader exception System.Text.Json wraps (`InnerException`).
    pub fn with_inner(mut self, inner: DotnetError) -> PayloadError {
        self.inner = Some(inner);
        self
    }

    /// `JsonException.Message`.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// `JsonException.InnerException`: the `FormatException` or `InvalidOperationException`
    /// of the reader when a value has the wrong token type or format.
    pub fn inner(&self) -> Option<&DotnetError> {
        self.inner.as_ref()
    }

    /// The `System.Text.Json.JsonException` as the .NET agent logs it, inner exception
    /// included.
    pub fn to_dotnet(&self) -> DotnetError {
        let e = DotnetError::new("System.Text.Json.JsonException", self.message.clone());
        match &self.inner {
            Some(inner) => e.with_inner(inner.clone()),
            None => e,
        }
    }
}

/// The `Utf8JsonReader` token type name of `v` (`Number`, `StartObject`, ...).
fn token_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(true) => "True",
        Value::Bool(false) => "False",
        Value::Number(_) => "Number",
        Value::String(_) => "String",
        Value::Array(_) => "StartArray",
        Value::Object(_) => "StartObject",
    }
}

/// The reader's `InvalidOperationException` for a token of the wrong type; `what` is
/// `a number`, `a string` or `a boolean`.
pub(crate) fn wrong_token(v: &Value, what: &str) -> DotnetError {
    DotnetError::new(
        "System.InvalidOperationException",
        format!(
            "Cannot get the value of a token type '{}' as {what}.",
            token_type(v)
        ),
    )
}

/// The reader's `FormatException` for a number that is not an integer of `clr_type` (or a
/// string that does not hold one).
fn bad_integer(clr_type: &str) -> DotnetError {
    DotnetError::new(
        "System.FormatException",
        format!(
            "Either the JSON value is not in a supported format, or is out of bounds for an {clr_type}."
        ),
    )
}

/// Compact JSON as System.Text.Json writes it with its default encoder
/// (`JavaScriptEncoder.Default`): printable ASCII is kept except `"`, `&`, `'`, `+`, `<`,
/// `>` and `` ` ``, which are `\u00XX`; `\` is `\\`; `\b \f \n \r \t` are short escapes;
/// every other control and every non-ASCII character is `\uXXXX` (UTF-16 units, upper-case
/// hex). Numbers keep their text. Use it for everything the C# agent serializes (result
/// bodies, outbox files, logged results) so the bytes are the same.
pub fn to_dotnet_json(value: &Value) -> String {
    let mut out = String::new();
    write_dotnet_json(&mut out, value);
    out
}

fn write_dotnet_json(out: &mut String, value: &Value) {
    match value {
        Value::String(s) => write_dotnet_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_dotnet_json(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut first = true;
            for (key, item) in map {
                if !first {
                    out.push(',');
                }
                first = false;
                write_dotnet_string(out, key);
                out.push(':');
                write_dotnet_json(out, item);
            }
            out.push('}');
        }
        other => out.push_str(&dl_json::to_string(other)),
    }
}

fn write_dotnet_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '"' | '&' | '\'' | '+' | '<' | '>' | '`' => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04X}");
                }
            }
        }
    }
    out.push('"');
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PayloadError {}

/// A command payload type (`cmd.Payload.Deserialize<T>(DeviceJson.Options)`).
pub trait Payload: Sized {
    /// Decodes a non-null payload strictly (unknown members refused).
    fn decode(reader: &mut Reader<'_>, value: &Value) -> Result<Self, PayloadError>;
}

/// Parses an API body or outbox file. Numbers keep their source text, so a payload that is
/// passed on or re-serialized is unchanged and error positions match System.Text.Json
/// (`2.0` stays three bytes). Use this, not `dl_json::parse`, for anything from the wire.
pub fn parse_json(bytes: &[u8]) -> Result<Value, dl_json::Error> {
    let opts = dl_json::ParseOptions {
        keep_raw_numbers: true,
        ..dl_json::ParseOptions::default()
    };
    dl_json::parse_slice_with(bytes, opts)
}

/// `CommandProcessor.Payload<T>`: decodes `payload` strictly; a JSON `null` payload is the
/// C# `JsonException("payload is null")`. Error positions assume the payload was parsed
/// with [`parse_json`] from the compact JSON the API sends.
pub fn decode_payload<T: Payload>(payload: &Value) -> Result<T, PayloadError> {
    if payload.is_null() {
        return Err(PayloadError::new("payload is null"));
    }
    let mut reader = Reader::new(payload, true);
    T::decode(&mut reader, payload)
}

/// One step of a System.Text.Json path.
#[derive(Debug, Clone)]
enum Seg {
    Key(String),
    Index(usize),
}

/// Reads one JSON document while tracking the System.Text.Json path for error messages.
pub struct Reader<'a> {
    root: &'a Value,
    strict: bool,
    path: Vec<Seg>,
}

impl<'a> Reader<'a> {
    /// A reader over `root`; `strict` = `DeviceJson.Options` (unknown members refused),
    /// otherwise `DeviceJson.Envelope` (unknown members skipped).
    pub fn new(root: &'a Value, strict: bool) -> Reader<'a> {
        Reader {
            root,
            strict,
            path: Vec::new(),
        }
    }

    /// `"The JSON value could not be converted to {type}. Path: ... | LineNumber: 0 |
    /// BytePositionInLine: n."` for the value at the current path.
    pub(crate) fn convert_error(&self, type_name: &str) -> PayloadError {
        PayloadError::new(format!(
            "The JSON value could not be converted to {type_name}. Path: {} | LineNumber: 0 | BytePositionInLine: {}.",
            self.path_string(),
            self.position()
        ))
    }

    fn path_string(&self) -> String {
        const SPECIAL: &[char] = &[
            '.', ' ', '\'', '/', '"', '[', ']', '(', ')', '\t', '\n', '\r', '\u{c}', '\u{8}', '\\',
            '\u{85}', '\u{2028}', '\u{2029}',
        ];
        let mut s = String::from("$");
        for seg in &self.path {
            match seg {
                Seg::Key(k) if k.contains(SPECIAL) => s.push_str(&format!("['{k}']")),
                Seg::Key(k) => {
                    s.push('.');
                    s.push_str(k);
                }
                Seg::Index(i) => s.push_str(&format!("[{i}]")),
            }
        }
        s
    }

    /// The byte offset just after the current token in the compact serialization of the
    /// root (System.Text.Json reports the reader position after the failing token; for an
    /// object or array that is one past its opening bracket).
    fn position(&self) -> usize {
        let len = |v: &Value| dl_json::to_string(v).len();
        let mut offset = 0;
        let mut cur = self.root;
        for seg in &self.path {
            match (seg, cur) {
                (Seg::Key(k), Value::Object(map)) => {
                    offset += 1;
                    let mut found = None;
                    for (mk, mv) in map {
                        let key_len = len(&Value::from(mk.as_str()));
                        if mk == k {
                            offset += key_len + 1;
                            found = Some(mv);
                            break;
                        }
                        offset += key_len + 1 + len(mv) + 1;
                    }
                    match found {
                        Some(v) => cur = v,
                        None => return offset,
                    }
                }
                (Seg::Index(i), Value::Array(items)) => {
                    offset += 1 + items
                        .iter()
                        .take(*i)
                        .map(|item| len(item) + 1)
                        .sum::<usize>();
                    match items.get(*i) {
                        Some(v) => cur = v,
                        None => return offset,
                    }
                }
                _ => return offset,
            }
        }
        offset
            + if matches!(cur, Value::Object(_) | Value::Array(_)) {
                1
            } else {
                len(cur)
            }
    }

    /// Visits the members of an object of .NET type `type_name` whose properties are
    /// `names` (camelCase, matched case-insensitively). `f` gets the property index.
    pub(crate) fn members<'v>(
        &mut self,
        map: &'v Map,
        type_name: &str,
        names: &[&str],
        mut f: impl FnMut(&mut Self, usize, &'v Value) -> Result<(), PayloadError>,
    ) -> Result<(), PayloadError> {
        for (key, v) in map {
            match names.iter().position(|n| eq_ignore_case(n, key)) {
                Some(i) => {
                    self.path.push(Seg::Key(key.clone()));
                    f(self, i, v)?;
                    self.path.pop();
                }
                None if self.strict => {
                    return Err(PayloadError::new(format!(
                        "The JSON property '{key}' could not be mapped to any .NET member contained in type '{type_name}'."
                    )));
                }
                None => {}
            }
        }
        Ok(())
    }

    /// The object at the current path, or the conversion error naming `type_name`.
    pub(crate) fn object<'v>(
        &self,
        v: &'v Value,
        type_name: &str,
    ) -> Result<&'v Map, PayloadError> {
        v.as_object().ok_or_else(|| self.convert_error(type_name))
    }

    /// A nested record: `null` is `None`, a non-object fails naming `type_name`.
    pub(crate) fn nested<'v, T>(
        &mut self,
        v: &'v Value,
        type_name: &str,
        decode: impl FnOnce(&mut Self, &'v Map) -> Result<T, PayloadError>,
    ) -> Result<Option<T>, PayloadError> {
        if v.is_null() {
            return Ok(None);
        }
        let map = self.object(v, type_name)?;
        decode(self, map).map(Some)
    }

    /// A `string` member: string or `null`; anything else fails naming the declaring type,
    /// with the reader's "as a string" exception inside.
    pub(crate) fn string(
        &self,
        v: &Value,
        declaring: &str,
    ) -> Result<Option<String>, PayloadError> {
        match v {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s.clone())),
            _ => Err(self
                .convert_error(declaring)
                .with_inner(wrong_token(v, "a string"))),
        }
    }

    /// An integer member of .NET type `clr_type` in `[min, max]`: a JSON integer or (web
    /// defaults) a string holding one; `null` is `None`. A number or string that is not
    /// such an integer fails with the reader's `FormatException` inside, another token
    /// with its `InvalidOperationException`.
    fn integer(
        &self,
        v: &Value,
        declaring: &str,
        clr_type: &str,
        min: i64,
        max: i64,
    ) -> Result<Option<i64>, PayloadError> {
        let n = match v {
            Value::Null => return Ok(None),
            Value::Number(n) => n.as_i64(),
            Value::String(s) => parse_integer_text(s),
            _ => {
                return Err(self
                    .convert_error(declaring)
                    .with_inner(wrong_token(v, "a number")));
            }
        };
        match n {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(self
                .convert_error(declaring)
                .with_inner(bad_integer(clr_type))),
        }
    }

    /// An `int` member (`null` fails like any other non-number token).
    pub(crate) fn int32(&self, v: &Value, declaring: &str) -> Result<i32, PayloadError> {
        self.int32_opt(v, declaring)?.ok_or_else(|| {
            self.convert_error(declaring)
                .with_inner(wrong_token(v, "a number"))
        })
    }

    /// An `int?` member.
    pub(crate) fn int32_opt(
        &self,
        v: &Value,
        declaring: &str,
    ) -> Result<Option<i32>, PayloadError> {
        // The range check keeps the value inside i32.
        Ok(self
            .integer(v, declaring, "Int32", i32::MIN.into(), i32::MAX.into())?
            .map(|n| n as i32))
    }

    /// A `long?` member.
    pub(crate) fn int64_opt(
        &self,
        v: &Value,
        declaring: &str,
    ) -> Result<Option<i64>, PayloadError> {
        self.integer(v, declaring, "Int64", i64::MIN, i64::MAX)
    }

    /// A `bool` member: only `true` / `false` (strings are refused, `null` fails).
    pub(crate) fn boolean(&self, v: &Value, declaring: &str) -> Result<bool, PayloadError> {
        self.boolean_opt(v, declaring)?.ok_or_else(|| {
            self.convert_error(declaring)
                .with_inner(wrong_token(v, "a boolean"))
        })
    }

    /// A `bool?` member.
    pub(crate) fn boolean_opt(
        &self,
        v: &Value,
        declaring: &str,
    ) -> Result<Option<bool>, PayloadError> {
        match v {
            Value::Null => Ok(None),
            Value::Bool(b) => Ok(Some(*b)),
            _ => Err(self
                .convert_error(declaring)
                .with_inner(wrong_token(v, "a boolean"))),
        }
    }

    /// A `DateTime` member through `WallClockDateTimeConverter` (`yyyy-MM-ddTHH:mm:ss`).
    pub(crate) fn wall_clock(&self, v: &Value, declaring: &str) -> Result<DateTime, PayloadError> {
        let text = match v {
            Value::String(s) => s.as_str(),
            Value::Null => "",
            _ => {
                return Err(self
                    .convert_error(declaring)
                    .with_inner(wrong_token(v, "a string")));
            }
        };
        parse_wall_clock(text).ok_or_else(|| {
            PayloadError::new(format!(
                "Expected a yyyy-MM-dd'T'HH:mm:ss date, got '{text}'"
            ))
        })
    }

    /// A `Guid` member: a string in the 36-character `D` form (any hex case).
    pub(crate) fn guid(&self, v: &Value, declaring: &str) -> Result<dl_uuid::Uuid, PayloadError> {
        let bad_format = || {
            self.convert_error(declaring).with_inner(DotnetError::new(
                "System.FormatException",
                "The JSON value is not in a supported Guid format.",
            ))
        };
        match v {
            Value::String(s) if s.len() == 36 => dl_uuid::Uuid::parse(s).map_err(|_| bad_format()),
            Value::String(_) => Err(bad_format()),
            _ => Err(self
                .convert_error(declaring)
                .with_inner(wrong_token(v, "a string"))),
        }
    }

    /// An `IReadOnlyList<string>` member: `null` is `None`, elements are strings or `null`.
    pub(crate) fn string_list(
        &mut self,
        v: &Value,
    ) -> Result<Option<Vec<Option<String>>>, PayloadError> {
        let items = match v {
            Value::Null => return Ok(None),
            Value::Array(items) => items,
            _ => {
                return Err(
                    self.convert_error("System.Collections.Generic.IReadOnlyList`1[System.String]")
                );
            }
        };
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            self.path.push(Seg::Index(i));
            out.push(self.string(item, "System.String")?);
            self.path.pop();
        }
        Ok(Some(out))
    }
}

/// `Utf8Parser.TryParse` of an integer held in a JSON string: optional sign, ASCII digits.
fn parse_integer_text(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.strip_prefix('+').unwrap_or(s).parse().ok()
}

/// `default(DateTime)`: `0001-01-01T00:00:00`, what a missing `from` / `to` decodes to.
pub const MIN_WALL_CLOCK: DateTime = DateTime::new(
    match Date::new(1, 1, 1) {
        Ok(d) => d,
        Err(_) => panic!("0001-01-01 is a valid date"),
    },
    Time::MIDNIGHT,
);

/// `DateTime.TryParseExact(text, "yyyy-MM-dd'T'HH:mm:ss", InvariantCulture)`: exactly that
/// shape, a real calendar date, years 0001-9999, no offset and no fraction.
pub fn parse_wall_clock(text: &str) -> Option<DateTime> {
    let b = text.as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<u32> {
        let part = &b[from..to];
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let year = num(0, 4)?;
    if year == 0 {
        return None;
    }
    let date = Date::new(
        i32::try_from(year).ok()?,
        u8::try_from(num(5, 7)?).ok()?,
        u8::try_from(num(8, 10)?).ok()?,
    )
    .ok()?;
    let time = Time::from_hms(
        u8::try_from(num(11, 13)?).ok()?,
        u8::try_from(num(14, 16)?).ok()?,
        u8::try_from(num(17, 19)?).ok()?,
    )
    .ok()?;
    Some(DateTime::new(date, time))
}

/// The converter's output format, `yyyy-MM-ddTHH:mm:ss`.
pub fn format_wall_clock(t: DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year(),
        t.month().number(),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// `Value::String` of [`format_wall_clock`].
pub(crate) fn wall_clock_value(t: DateTime) -> Value {
    Value::from(format_wall_clock(t))
}

/// An optional record as JSON (`null` for `None`).
pub(crate) fn opt_json<T: dl_json::ToJson>(v: &Option<T>) -> Value {
    v.as_ref().map_or(Value::Null, dl_json::ToJson::to_json)
}

/// `Option<String>` as JSON (`null` for `None`), the System.Text.Json output of a `string`.
pub(crate) fn opt_str(s: &Option<String>) -> Value {
    s.as_deref().map_or(Value::Null, Value::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_matches_parse_exact() {
        let ok = parse_wall_clock("2026-10-14T14:00:00").unwrap();
        assert_eq!(format_wall_clock(ok), "2026-10-14T14:00:00");
        for bad in [
            "2026-10-14 14:00:00",
            "2026-10-14T14:00:00Z",
            "2026-02-30T00:00:00",
            "2026-1-14T14:00:00",
            "2026-10-14T24:00:00",
            "0000-10-14T14:00:00",
            "2026-10-14T14:00:00.5",
            " 2026-10-14T14:00:00",
            "",
            "２026-10-14T14:00:00",
        ] {
            assert_eq!(parse_wall_clock(bad), None, "{bad:?}");
        }
        assert_eq!(format_wall_clock(MIN_WALL_CLOCK), "0001-01-01T00:00:00");
    }

    #[test]
    fn writes_like_the_system_text_json_default_encoder() {
        // Expected text from JsonSerializer.Serialize(new { s }, DeviceJson.Options) (.NET 10).
        let s = "q\" b\\ s/ lt< gt> amp& ap' plus+ bt` del\u{7f} nl\n cr\r tab\t bs\u{8} ff\u{c} c1\u{1} nbsp\u{a0} e\u{e9} viet\u{1b0}\u{1ebf} ls\u{2028} emoji\u{1F600} ~";
        assert_eq!(
            to_dotnet_json(&dl_json::json!({"s": s})),
            r#"{"s":"q\u0022 b\\ s/ lt\u003C gt\u003E amp\u0026 ap\u0027 plus\u002B bt\u0060 del\u007F nl\n cr\r tab\t bs\b ff\f c1\u0001 nbsp\u00A0 e\u00E9 viet\u01B0\u1EBF ls\u2028 emoji\uD83D\uDE00 ~"}"#
        );
        // The e2e comparison: the NotConnectedAdapter message and a quote in a message.
        let body = parse_json(br#"{"a":[1,2.50,null,true],"m":"Ch\u01b0a k\u1ebft n\u1ed1i thi\u1ebft b\u1ecb kho\u00e1","q":"room's"}"#).unwrap();
        assert_eq!(
            to_dotnet_json(&body),
            r#"{"a":[1,2.50,null,true],"m":"Ch\u01B0a k\u1EBFt n\u1ED1i thi\u1EBFt b\u1ECB kho\u00E1","q":"room\u0027s"}"#
        );
        assert_eq!(
            to_dotnet_json(&dl_json::json!({"k\"": {}})),
            r#"{"k\u0022":{}}"#
        );
    }

    #[test]
    fn conversion_errors_carry_the_reader_exception() {
        // Inner exceptions as System.Text.Json 10 reports them (probe of DeviceJson.Options).
        let inner_of = |json: &str, f: &dyn Fn(&Reader<'_>, &Value) -> Result<(), PayloadError>| {
            let root = parse_json(json.as_bytes()).unwrap();
            let mut r = Reader::new(&root, true);
            r.path.push(Seg::Key("x".into()));
            let e = f(&r, root.as_object().unwrap().get("x").unwrap()).unwrap_err();
            e.inner().map(|i| format!("{}: {}", i.type_name, i.message))
        };
        let int = |r: &Reader<'_>, v: &Value| r.int32(v, "T").map(drop);
        let long = |r: &Reader<'_>, v: &Value| r.int64_opt(v, "T").map(drop);
        let string = |r: &Reader<'_>, v: &Value| r.string(v, "T").map(drop);
        let boolean = |r: &Reader<'_>, v: &Value| r.boolean_opt(v, "T").map(drop);
        let guid = |r: &Reader<'_>, v: &Value| r.guid(v, "T").map(drop);
        let fmt32 = "System.FormatException: Either the JSON value is not in a supported format, or is out of bounds for an Int32.";
        let op = |t: &str, what: &str| {
            format!(
                "System.InvalidOperationException: Cannot get the value of a token type '{t}' as {what}."
            )
        };
        assert_eq!(inner_of(r#"{"x":"two"}"#, &int).as_deref(), Some(fmt32));
        assert_eq!(inner_of(r#"{"x":2.5}"#, &int).as_deref(), Some(fmt32));
        assert_eq!(
            inner_of(r#"{"x":3000000000}"#, &int).as_deref(),
            Some(fmt32)
        );
        assert_eq!(
            inner_of(r#"{"x":true}"#, &int),
            Some(op("True", "a number"))
        );
        assert_eq!(
            inner_of(r#"{"x":{}}"#, &int),
            Some(op("StartObject", "a number"))
        );
        assert_eq!(
            inner_of(r#"{"x":[1]}"#, &int),
            Some(op("StartArray", "a number"))
        );
        assert_eq!(
            inner_of(r#"{"x":null}"#, &int),
            Some(op("Null", "a number"))
        );
        assert_eq!(
            inner_of(r#"{"x":"x"}"#, &long).as_deref(),
            Some(
                "System.FormatException: Either the JSON value is not in a supported format, or is out of bounds for an Int64."
            )
        );
        assert_eq!(
            inner_of(r#"{"x":5}"#, &string),
            Some(op("Number", "a string"))
        );
        assert_eq!(
            inner_of(r#"{"x":[]}"#, &string),
            Some(op("StartArray", "a string"))
        );
        assert_eq!(
            inner_of(r#"{"x":"true"}"#, &boolean),
            Some(op("String", "a boolean"))
        );
        assert_eq!(
            inner_of(r#"{"x":1}"#, &boolean),
            Some(op("Number", "a boolean"))
        );
        assert_eq!(
            inner_of(r#"{"x":"nope"}"#, &guid).as_deref(),
            Some("System.FormatException: The JSON value is not in a supported Guid format.")
        );
        assert_eq!(
            inner_of(r#"{"x":5}"#, &guid),
            Some(op("Number", "a string"))
        );
        assert_eq!(
            inner_of(r#"{"x":null}"#, &guid),
            Some(op("Null", "a string"))
        );
        let required_bool = |r: &Reader<'_>, v: &Value| r.boolean(v, "T").map(drop);
        assert_eq!(
            inner_of(r#"{"x":null}"#, &required_bool),
            Some(op("Null", "a boolean"))
        );
        // A record of the wrong shape has no inner exception.
        let root = parse_json(br#"{"x":5}"#).unwrap();
        let r = Reader::new(&root, true);
        let x = root.as_object().unwrap().get("x").unwrap();
        assert!(r.object(x, "T").unwrap_err().inner().is_none());
        let e = PayloadError::new("m").with_inner(DotnetError::new("System.FormatException", "f"));
        assert_eq!(
            e.to_dotnet().to_string(),
            "System.Text.Json.JsonException: m\n ---> System.FormatException: f\n   --- End of inner exception stack trace ---"
        );
    }

    #[test]
    fn integer_strings_follow_utf8_parser() {
        assert_eq!(parse_integer_text("2"), Some(2));
        assert_eq!(parse_integer_text("-3"), Some(-3));
        assert_eq!(parse_integer_text("+3"), Some(3));
        assert_eq!(parse_integer_text("007"), Some(7));
        for bad in ["", " 2", "2.0", "1e2", "+", "-", "2 "] {
            assert_eq!(parse_integer_text(bad), None, "{bad:?}");
        }
    }
}
