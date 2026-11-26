//! Per-column value transforms (`Transforms/ValueTransforms.cs`, `Transforms/SnakeCase.cs`).
//! Every transform returns a canonical invariant string or `None` (SQL NULL).

use super::{MappingError, RawValue, tcvn3};
use crate::contracts::hashing::canonical_value;
use crate::contracts::time::{DateTime, DateTimeKind, TICKS_PER_HOUR, TimeZone, time_span_text};
use crate::util::{stj, store_text};

fn format_error(column: &str, message: String) -> MappingError {
    MappingError::ValueFormat {
        column: column.to_owned(),
        message,
    }
}

/// TCVN3 to Unicode: bytes are decoded; a string of chars below U+0100 is taken as bytes
/// read through Latin-1; any other string is already Unicode. Trailing spaces are dropped
/// ([`trim_fixed_width`]).
pub fn tcvn3(value: &RawValue, column: &str) -> Result<Option<String>, MappingError> {
    match value {
        RawValue::Null => Ok(None),
        RawValue::Bytes(b) => Ok(Some(trim_fixed_width(tcvn3::decode(b)))),
        RawValue::String(s) => {
            if s.chars().all(|c| u32::from(c) < 0x100) {
                let bytes: Vec<u8> = s.chars().map(|c| u32::from(c) as u8).collect();
                Ok(Some(trim_fixed_width(tcvn3::decode(&bytes))))
            } else {
                Ok(Some(trim_fixed_width(s.clone())))
            }
        }
        other => Err(format_error(
            column,
            format!(
                "Column '{column}': TCVN3 transform expects bytes or text, got {}.",
                other.type_name()
            ),
        )),
    }
}

/// `ValueTransforms.TrimFixedWidth` (ADR 0123): text without its trailing spaces. The store
/// database pads `CHAR(n)` values and ignores trailing spaces when it compares text, so
/// `'100001'` and `'100001      '` are one code there; PostgreSQL `text` sees two. Only
/// U+0020 is dropped, as in that comparison (tabs, newlines, leading and inner spaces stay);
/// an all-space value becomes `""`, never NULL. NUL characters, which PostgreSQL refuses,
/// are removed too (RT-1309): this is [`store_text::clean`], the agent's one read-side
/// normaliser.
pub fn trim_fixed_width(value: String) -> String {
    store_text::clean(value)
}

/// `new decimal(double)`: rounded to 15 significant digits (7 for `float`).
fn decimal_from_float(v: f64, digits: usize) -> Option<dl_decimal::Decimal> {
    if !v.is_finite() {
        return None;
    }
    if v == 0.0 {
        return Some(dl_decimal::Decimal::ZERO);
    }
    let text = format!("{:.*e}", digits - 1, v);
    let d = dl_decimal::Decimal::parse_rounded(&text, dl_decimal::RoundingMode::HalfEven).ok()?;
    stj::fit_dotnet_decimal(d)
}

fn overflow(column: &str) -> MappingError {
    format_error(
        column,
        "Value was either too large or too small for a Decimal.".into(),
    )
}

/// Numeric to invariant decimal string (trailing zeros trimmed).
pub fn money(value: &RawValue, column: &str) -> Result<Option<String>, MappingError> {
    use RawValue::*;
    let d = match value {
        Null => return Ok(None),
        Decimal(d) => *d,
        U8(n) => (*n).into(),
        I8(n) => (*n).into(),
        I16(n) => (*n).into(),
        U16(n) => (*n).into(),
        I32(n) => (*n).into(),
        U32(n) => (*n).into(),
        I64(n) => (*n).into(),
        U64(n) => (*n).into(),
        F64(v) => decimal_from_float(*v, 15).ok_or_else(|| overflow(column))?,
        F32(v) => decimal_from_float(f64::from(*v), 7).ok_or_else(|| overflow(column))?,
        String(s) => {
            return canonical_value::decimal_or_null(Some(s))
                .map(Some)
                .ok_or_else(|| {
                    format_error(
                        column,
                        format!("Column '{column}': '{s}' is not a decimal."),
                    )
                });
        }
        other => {
            return Err(format_error(
                column,
                format!(
                    "Column '{column}': money transform got {}.",
                    other.type_name()
                ),
            ));
        }
    };
    Ok(Some(canonical_value::decimal_string(d)))
}

/// Site-local naive datetime to UTC (`Z`, 7 fraction digits). Ambiguous local times take
/// the standard offset; times in the spring-forward gap move forward by the gap. UTC
/// kind is trusted as UTC.
pub fn local_time(
    value: &RawValue,
    column: &str,
    zone: &TimeZone,
) -> Result<Option<String>, MappingError> {
    let range = || format_error(column, format!("Column '{column}': time is out of range."));
    match value {
        RawValue::Null => Ok(None),
        RawValue::DateTime(dt) => Ok(Some(canonical_value::utc(
            to_utc(*dt, zone).ok_or_else(range)?,
        ))),
        RawValue::DateTimeOffset(t) => Ok(Some(canonical_value::utc(t.utc_date_time()))),
        RawValue::String(s) => match DateTime::try_parse_invariant(s) {
            Some(parsed) => Ok(Some(canonical_value::utc(
                to_utc(parsed, zone).ok_or_else(range)?,
            ))),
            None => Err(format_error(
                column,
                format!(
                    "Column '{column}': time transform got {}.",
                    value.type_name()
                ),
            )),
        },
        other => Err(format_error(
            column,
            format!(
                "Column '{column}': time transform got {}.",
                other.type_name()
            ),
        )),
    }
}

/// `ValueTransforms.ToUtc`: a site wall clock to UTC (`None` out of range).
pub fn to_utc(local: DateTime, zone: &TimeZone) -> Option<DateTime> {
    if local.kind() == DateTimeKind::Utc {
        return Some(local);
    }
    let mut naive = local.ticks();
    if zone.is_invalid_time(naive) {
        let gap = zone
            .adjustment_rules()
            .iter()
            .map(|r| r.daylight_delta)
            .find(|d| *d != 0)
            .unwrap_or(TICKS_PER_HOUR);
        naive = naive.checked_add(gap)?;
    }
    let utc = if zone.is_ambiguous_time(naive) {
        naive.checked_sub(zone.base_utc_offset_ticks())?
    } else {
        zone.convert_time_to_utc(naive)
            .or_else(|| naive.checked_sub(zone.base_utc_offset_ticks()))?
    };
    DateTime::from_ticks(utc, DateTimeKind::Utc)
}

fn float_text(v: f64, single: bool) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let t = if single {
        stj::f32_text(v as f32)
    } else {
        stj::f64_text(v)
    };
    t.unwrap_or_default()
}

/// Type-driven canonical form for columns with no catalog transform. A `DateTime` is
/// still a vendor naive local time and converts like a listed one.
pub fn generic(
    value: &RawValue,
    column: &str,
    zone: &TimeZone,
) -> Result<Option<String>, MappingError> {
    use RawValue::*;
    Ok(Some(match value {
        Null => return Ok(None),
        String(s) => trim_fixed_width(s.clone()),
        DateTime(_) | DateTimeOffset(_) => return local_time(value, column, zone),
        Decimal(d) => canonical_value::decimal_string(*d),
        Bool(b) => canonical_value::bool_string(*b).to_owned(),
        Bytes(b) => canonical_value::base64(b),
        Guid(g) => g.to_string(),
        F64(v) => float_text(*v, false),
        F32(v) => float_text(f64::from(*v), true),
        U8(n) => n.to_string(),
        I8(n) => n.to_string(),
        I16(n) => n.to_string(),
        U16(n) => n.to_string(),
        I32(n) => n.to_string(),
        U32(n) => n.to_string(),
        I64(n) => n.to_string(),
        U64(n) => n.to_string(),
        TimeSpan(t) => time_span_text(*t),
    }))
}

/// Vendor PascalCase/camelCase identifier to snake_case (`PNo` -> `p_no`, `ITotalR` ->
/// `i_total_r`).
pub fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            continue;
        }
        if c.is_uppercase()
            && i > 0
            && needs_break(&chars, i)
            && !out.is_empty()
            && !out.ends_with('_')
        {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out.trim_matches('_').to_owned()
}

fn needs_break(chars: &[char], i: usize) -> bool {
    let prev = chars[i - 1];
    if prev.is_lowercase() || prev.is_numeric() {
        return true;
    }
    prev.is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase())
}
