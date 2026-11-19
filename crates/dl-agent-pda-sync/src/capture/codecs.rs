//! Cursor and key codecs (`Strategies/Codecs.cs`): typed key values to and from the
//! strings kept in `capture.db` cursors, and the stable string identity of a wire key.
//! The text is byte-identical to the C# agent's, so cursors survive an exe swap.

use super::error::{CaptureError, ErrorKind, Result};
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::map::StrMap;
use crate::contracts::mapping::RawValue;
use crate::contracts::time::{DateTime, DateTimeKind};
use crate::util::stj::{self, Style, Writer};

/// `double.ToString("R", InvariantCulture)` (.NET Core 3.0+): shortest round-trip digits;
/// scientific (`1E+15`, `1.5E-05`) when the scale exceeds `max(digits, 15)` or the
/// exponent is below -4.
pub fn double_r(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_owned();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let n = digits.len() as i32;
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    if exp + 1 > n.max(15) || exp < -4 {
        out.push_str(&digits[..1]);
        if n > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    } else if exp < 0 {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let int_len = (exp + 1) as usize;
        if digits.len() <= int_len {
            out.push_str(&digits);
            for _ in digits.len()..int_len {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..int_len]);
            out.push('.');
            out.push_str(&digits[int_len..]);
        }
    }
    out
}

fn hex_lower(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(H[usize::from(x >> 4)] as char);
        s.push(H[usize::from(x & 15)] as char);
    }
    s
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let nib = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    b.chunks(2)
        .map(|p| Some(nib(p[0])? << 4 | nib(p[1])?))
        .collect()
}

/// `ValueCodec`: typed key values to and from cursor text.
pub mod value_codec {
    use super::*;

    /// `ValueCodec.Encode`.
    pub fn encode(value: &RawValue) -> Result<String> {
        Ok(match value {
            // A later logical-key column of a raw table can be NULL (SY4).
            RawValue::Null => "n:".to_owned(),
            RawValue::U8(n) => format!("i:{n}"),
            RawValue::I8(n) => format!("i:{n}"),
            RawValue::I16(n) => format!("i:{n}"),
            RawValue::U16(n) => format!("i:{n}"),
            RawValue::I32(n) => format!("i:{n}"),
            RawValue::U32(n) => format!("i:{n}"),
            RawValue::I64(n) => format!("i:{n}"),
            RawValue::U64(n) => format!("u:{n}"),
            RawValue::Decimal(d) => format!("m:{d}"),
            RawValue::F64(f) => format!("f:{}", double_r(*f)),
            RawValue::F32(f) => format!("f:{}", double_r(f64::from(*f))),
            RawValue::DateTime(t) => format!("t:{}", t.ticks()),
            RawValue::Guid(g) => format!("g:{}", g.to_string().to_lowercase()),
            RawValue::String(s) => format!("s:{s}"),
            RawValue::Bytes(b) => format!("b:{}", hex_lower(b)),
            RawValue::Bool(o) => format!("o:{}", if *o { "1" } else { "0" }),
            RawValue::DateTimeOffset(_) | RawValue::TimeSpan(_) => {
                return Err(CaptureError::new(
                    ErrorKind::NotSupported,
                    format!("Unsupported key type {}.", value.type_name()),
                ));
            }
        })
    }

    fn bad(text: &str) -> CaptureError {
        CaptureError::format(format!("Unknown cursor value '{text}'."))
    }

    /// `ValueCodec.Decode`.
    pub fn decode(text: &str) -> Result<RawValue> {
        let Some(body) = text.get(2..) else {
            return Err(bad(text));
        };
        let num_err = || CaptureError::format("The input string was not in a correct format.");
        Ok(match text.as_bytes()[0] {
            b'i' => RawValue::I64(body.trim().parse().map_err(|_| num_err())?),
            b'u' => RawValue::U64(body.trim().parse().map_err(|_| num_err())?),
            b'm' => RawValue::Decimal(stj::parse_dotnet_decimal(body.trim()).ok_or_else(num_err)?),
            b'f' => RawValue::F64(match body.trim() {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                b => b.parse().map_err(|_| num_err())?,
            }),
            b't' => {
                let ticks: i64 = body.trim().parse().map_err(|_| num_err())?;
                RawValue::DateTime(
                    DateTime::from_ticks(ticks, DateTimeKind::Unspecified).ok_or_else(num_err)?,
                )
            }
            b'g' => RawValue::Guid(
                body.trim()
                    .parse::<dl_uuid::Uuid>()
                    .map_err(|_| CaptureError::format("Unrecognized Guid format."))?,
            ),
            b's' => RawValue::String(body.to_owned()),
            b'b' => RawValue::Bytes(
                from_hex(body)
                    .ok_or_else(|| CaptureError::format("The input is not a valid hex string."))?,
            ),
            b'o' => RawValue::Bool(body == "1"),
            b'n' => RawValue::Null,
            _ => return Err(bad(text)),
        })
    }

    /// `ValueCodec.EncodeKey`: a JSON array of encoded values, as `JsonSerializer`
    /// writes it with default options.
    pub fn encode_key(values: &[RawValue]) -> Result<String> {
        let mut w = Writer::new(Style::Compact);
        w.begin_array();
        for v in values {
            w.string(&encode(v)?);
        }
        w.end_array();
        Ok(w.finish())
    }

    /// `ValueCodec.DecodeKey`: `None` for `None`.
    pub fn decode_key(json: Option<&str>) -> Result<Option<Vec<RawValue>>> {
        let Some(json) = json else { return Ok(None) };
        let v = stj::parse(json).map_err(|e| CaptureError::new(ErrorKind::Other, e.to_string()))?;
        let dl_json::Value::Array(items) = v else {
            return Err(CaptureError::new(
                ErrorKind::Other,
                format!("The JSON value could not be converted to System.String[]. {json}"),
            ));
        };
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            match item {
                dl_json::Value::String(s) => out.push(decode(s)?),
                _ => {
                    return Err(CaptureError::new(
                        ErrorKind::NullReference,
                        "Object reference not set to an instance of an object.",
                    ));
                }
            }
        }
        Ok(Some(out))
    }
}

/// `KeyCodec`: stable string identity of a wire key (injective: names and values are
/// length-prefixed, lengths in UTF-16 units as .NET counts them).
pub mod key_codec {
    use super::*;
    use crate::contracts::utf16_len;

    /// `KeyCodec.Encode`: pairs in ordinal key order.
    pub fn encode(key: &StrMap) -> String {
        let mut pairs: Vec<(&str, &Option<String>)> = key.iter().collect();
        pairs.sort_by(|a, b| ordinal_cmp(a.0, b.0));
        let mut s = String::new();
        for (k, v) in pairs {
            s.push_str(&format!("{}:{k}=", utf16_len(k)));
            match v {
                None => s.push('~'),
                Some(v) => s.push_str(&format!("{}:{v}", utf16_len(v))),
            }
            s.push(';');
        }
        s
    }

    fn malformed(s: &str) -> CaptureError {
        CaptureError::format(format!("Malformed key '{s}'."))
    }

    /// `KeyCodec.Decode`: inverse of [`encode`]; an occurrence suffix (`#n`) after the
    /// last pair is ignored.
    pub fn decode(encoded: &str) -> Result<StrMap> {
        let u: Vec<u16> = encoded.encode_utf16().collect();
        let text = |a: usize, b: usize| -> Result<String> {
            u.get(a..b)
                .and_then(|r| String::from_utf16(r).ok())
                .ok_or_else(|| {
                    CaptureError::new(
                        ErrorKind::Argument,
                        "Index and length must refer to a location within the string. (Parameter 'length')",
                    )
                })
        };
        let read_len = |pos: &mut usize| -> Result<usize> {
            let end = u[*pos..]
                .iter()
                .position(|&c| c == u16::from(b':'))
                .map(|i| i + *pos)
                .ok_or_else(|| malformed(encoded))?;
            let n: i64 = text(*pos, end)?.trim().parse().map_err(|_| {
                CaptureError::format("The input string was not in a correct format.")
            })?;
            *pos = end + 1;
            usize::try_from(n).map_err(|_| {
                CaptureError::new(
                    ErrorKind::Argument,
                    "Length cannot be less than zero. (Parameter 'length')",
                )
            })
        };
        let expect = |pos: &mut usize, c: u8| -> Result<()> {
            if u.get(*pos) != Some(&u16::from(c)) {
                return Err(malformed(encoded));
            }
            *pos += 1;
            Ok(())
        };
        let mut result = StrMap::new();
        let mut pos = 0;
        while pos < u.len() && u[pos] != u16::from(b'#') {
            let name_len = read_len(&mut pos)?;
            let name = text(pos, pos + name_len)?;
            pos += name_len;
            expect(&mut pos, b'=')?;
            match u.get(pos) {
                Some(&c) if c == u16::from(b'~') => {
                    pos += 1;
                    result.insert(name, None);
                }
                Some(_) => {
                    let value_len = read_len(&mut pos)?;
                    let value = text(pos, pos + value_len)?;
                    pos += value_len;
                    result.insert(name, Some(value));
                }
                None => {
                    return Err(CaptureError::new(
                        ErrorKind::Other,
                        "Index was outside the bounds of the array.",
                    ));
                }
            }
            expect(&mut pos, b';')?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    //! `CodecTests.cs` (the `SrcVerClock` case is in `src_ver.rs`).
    use super::*;
    use crate::contracts::map::str_map;

    #[test]
    fn typed_key_values_round_trip_through_cursor_text() {
        let values = [
            RawValue::I32(5),
            RawValue::I64(5),
            RawValue::I16(3),
            RawValue::I64(12_345_678_901),
            RawValue::Decimal("12.5".parse().unwrap()),
            RawValue::F64(1.5),
            RawValue::DateTime(
                DateTime::from_ticks(
                    DateTime::new(2026, 10, 24, 12, 30, 1).unwrap().ticks() + 123 * 10_000,
                    DateTimeKind::Unspecified,
                )
                .unwrap(),
            ),
            RawValue::Guid("2f1b5d1c-0b0c-4a58-9c3f-0d6c4c1d8e11".parse().unwrap()),
            RawValue::from("tcvn3 À text"),
            RawValue::Bytes(vec![0, 255, 16]),
            RawValue::Bool(true),
        ];
        for v in values {
            let back = value_codec::decode(&value_codec::encode(&v).unwrap()).unwrap();
            let expected = match v {
                RawValue::I16(n) => RawValue::I64(n.into()),
                RawValue::I32(n) => RawValue::I64(n.into()),
                other => other,
            };
            assert_eq!(back, expected);
        }
    }

    #[test]
    fn keyset_position_round_trips_including_null() {
        let d = DateTime::new(2026, 1, 2, 0, 0, 0).unwrap();
        let json = value_codec::encode_key(&[
            RawValue::I32(1),
            RawValue::from("x"),
            RawValue::DateTime(d),
            RawValue::Null,
        ])
        .unwrap();
        assert_eq!(
            value_codec::decode_key(Some(&json)).unwrap().unwrap(),
            [
                RawValue::I64(1),
                RawValue::from("x"),
                RawValue::DateTime(d),
                RawValue::Null
            ]
        );
        assert_eq!(value_codec::decode_key(None).unwrap(), None);
    }

    #[test]
    fn cursor_text_matches_the_csharp_bytes() {
        // JsonSerializer's default encoder escapes non-ASCII and HTML-sensitive characters.
        let json = value_codec::encode_key(&[RawValue::I32(7), RawValue::from("À+")]).unwrap();
        assert_eq!(json, r#"["i:7","s:\u00C0\u002B"]"#);
        assert_eq!(
            value_codec::encode(&RawValue::F64(1e15)).unwrap(),
            "f:1E+15"
        );
        assert_eq!(
            value_codec::encode(&RawValue::F64(1e14)).unwrap(),
            "f:100000000000000"
        );
        assert_eq!(
            value_codec::encode(&RawValue::F64(0.00001)).unwrap(),
            "f:1E-05"
        );
        assert_eq!(
            value_codec::encode(&RawValue::F64(0.0001)).unwrap(),
            "f:0.0001"
        );
        assert_eq!(
            value_codec::encode(&RawValue::F32(1.1)).unwrap(),
            "f:1.100000023841858"
        );
        assert_eq!(
            value_codec::encode(&RawValue::Decimal("12.50".parse().unwrap())).unwrap(),
            "m:12.50"
        );
        assert_eq!(
            value_codec::encode(&RawValue::Bytes(vec![0xAB])).unwrap(),
            "b:ab"
        );
        assert!(value_codec::decode("x:1").unwrap_err().kind == ErrorKind::Format);
        assert!(value_codec::decode("").is_err());
    }

    #[test]
    fn wire_key_encoding_is_injective_and_reversible() {
        let tricky1 = str_map([("a", Some("1;1:b=2")), ("c", None)]);
        let tricky2 = str_map([("a", Some("1")), ("b", Some("2")), ("c", None)]);
        assert_ne!(key_codec::encode(&tricky1), key_codec::encode(&tricky2));
        assert_eq!(
            key_codec::decode(&key_codec::encode(&tricky1)).unwrap(),
            tricky1
        );
        assert_eq!(
            key_codec::decode(&(key_codec::encode(&tricky2) + "#2")).unwrap(),
            tricky2,
            "an occurrence suffix is not part of the key"
        );
        assert_eq!(key_codec::encode(&tricky2), "1:a=1:1;1:b=1:2;1:c=~;");
        assert!(key_codec::decode("1:a=").is_err());
    }

    #[test]
    fn key_order_does_not_matter() {
        assert_eq!(
            key_codec::encode(&str_map([("b", Some("2")), ("a", Some("1"))])),
            key_codec::encode(&str_map([("a", Some("1")), ("b", Some("2"))]))
        );
    }

    #[test]
    fn lengths_count_utf16_units() {
        let k = str_map([("k", Some("😀é"))]);
        let e = key_codec::encode(&k);
        assert_eq!(e, "1:k=3:😀é;");
        assert_eq!(key_codec::decode(&e).unwrap(), k);
    }
}
