//! Binding op args to procedure parameters, as the Go agent converts them.

use super::*;

pub(super) struct Output {
    pub(super) name: String,
    pub(super) typ: String,
}

pub(super) struct BoundCall {
    pub(super) proc_name: String,
    pub(super) params: Vec<Param>,
    pub(super) outputs: Vec<Output>,
}

pub(super) fn json_str(v: &J) -> Option<&str> {
    match v {
        J::String(s) => Some(s),
        _ => None,
    }
}

pub(super) fn int_in(v: &J, typ: &str, lo: i64, hi: i64) -> Result<i64, String> {
    let Some(n) = v.as_number() else {
        return Err(format!("{typ} expects an integer"));
    };
    let text = json::number_text(n);
    match text.parse::<i64>() {
        Ok(x) if (lo..=hi).contains(&x) => Ok(x),
        Ok(_) => Err(format!("{typ} out of range")),
        // Go's ParseInt reports a range error for an integer literal that does not fit.
        Err(_) if !text.contains(['.', 'e', 'E']) => Err(format!("{typ} out of range")),
        Err(_) => Err(format!("{typ} expects an integer")),
    }
}

pub(super) fn is_decimal_text(s: &str) -> bool {
    let body = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let digits = |x: &str| (1..=38).contains(&x.len()) && x.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

/// `YYYY-MM-DD` exactly (Go's `civil.ParseDate`).
pub(super) fn parse_date(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let p = &s[r];
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let y = i32::try_from(num(0..4)?).ok()?;
    let m = u8::try_from(num(5..7)?).ok()?;
    let d = u8::try_from(num(8..10)?).ok()?;
    Date::new(y, m, d).ok()
}

pub(super) fn typed_null(typ: &str) -> ParamValue {
    match typ {
        "bit" => ParamValue::Bool(None),
        "tinyint" | "smallint" | "int" | "bigint" => ParamValue::I64(None),
        "varchar" => ParamValue::VarChar(None),
        "date" => ParamValue::Date(None),
        "datetime2" => ParamValue::DateTime(None),
        "datetimeoffset" => ParamValue::DateTimeOffset(None),
        "varbinary" => ParamValue::Bytes(None),
        _ => ParamValue::String(None),
    }
}

/// Converts one JSON value to the parameter go-mssqldb would send for `p.typ`.
pub(super) fn bind_value(p: &ops::Param, raw: Option<&J>) -> Result<ParamValue, String> {
    let raw = match raw {
        None | Some(J::Null) => {
            if p.nullable || (p.has_value() && p.arg.is_empty()) {
                return Ok(typed_null(&p.typ));
            }
            return Err(format!("{} may not be null", p.typ));
        }
        Some(v) => v,
    };
    let text_of = |raw: &J| -> Result<String, String> {
        let s = json_str(raw).ok_or_else(|| format!("{} expects a string", p.typ))?;
        let n = p.max_size();
        if n >= 0 && json::rune_count(s) as i64 > n {
            return Err(format!("{} longer than {n} characters", p.typ));
        }
        Ok(s.to_owned())
    };
    Ok(match p.typ.as_str() {
        "bit" => match raw {
            J::Bool(b) => ParamValue::Bool(Some(*b)),
            _ => return Err("bit expects a boolean".into()),
        },
        "tinyint" => ParamValue::I64(Some(int_in(raw, "tinyint", 0, 255)?)),
        "smallint" => ParamValue::I64(Some(int_in(raw, "smallint", -32768, 32767)?)),
        "int" => ParamValue::I64(Some(int_in(
            raw,
            "int",
            i64::from(i32::MIN),
            i64::from(i32::MAX),
        )?)),
        "bigint" => ParamValue::I64(Some(int_in(raw, "bigint", i64::MIN, i64::MAX)?)),
        "decimal" => {
            let s = match raw {
                J::String(s) => s.clone(),
                other => json::to_string(other),
            };
            if !is_decimal_text(&s) {
                return Err("decimal expects a plain decimal number or string".into());
            }
            // Bound as text; SQL Server converts exactly to the proc's decimal(p,s).
            ParamValue::String(Some(s))
        }
        "varchar" => ParamValue::VarChar(Some(text_of(raw)?)),
        "nvarchar" => ParamValue::String(Some(text_of(raw)?)),
        "date" => {
            let s = json_str(raw).ok_or("date expects ISO 8601 text")?;
            ParamValue::Date(Some(parse_date(s).ok_or("date expects YYYY-MM-DD")?))
        }
        "datetime2" | "datetimeoffset" => {
            let s = json_str(raw).ok_or_else(|| format!("{} expects ISO 8601 text", p.typ))?;
            let t = OffsetDateTime::parse_rfc3339(s)
                .map_err(|_| format!("{} expects ISO 8601 with an offset", p.typ))?;
            if p.typ == "datetimeoffset" {
                ParamValue::DateTimeOffset(Some(t))
            } else {
                // The instant in UTC without its offset (tedious useUTC).
                ParamValue::DateTime(Some(t.to_utc().datetime()))
            }
        }
        "varbinary" => {
            let s = json_str(raw).ok_or("varbinary expects base64 text")?;
            let clean: String = s.chars().filter(|c| *c != '\r' && *c != '\n').collect();
            let b = dl_encoding::base64::STANDARD
                .decode(&clean)
                .map_err(|_| "varbinary expects base64 text".to_owned())?;
            let n = p.max_size();
            if n >= 0 && b.len() as i64 > n {
                return Err(format!("varbinary longer than {n} bytes"));
            }
            ParamValue::Bytes(Some(b))
        }
        t => return Err(format!("type {t} is not bindable")),
    })
}

/// Checks the args against the manifest types and builds every call's parameters.
/// Values are only ever bound as parameters; nothing is spliced into SQL.
pub(super) fn bind_args(op: &ops::Op, args: &J) -> Result<Vec<BoundCall>, String> {
    let empty = Map::new();
    let args = match args {
        J::Object(m) => m,
        J::Null => &empty,
        _ => return Err("invalid_args: args must be an object".into()),
    };
    let mut used = std::collections::HashSet::new();
    let mut calls = Vec::with_capacity(op.calls.len());
    for c in &op.calls {
        let mut bc = BoundCall {
            proc_name: c.proc_name.clone(),
            params: Vec::new(),
            outputs: Vec::new(),
        };
        for p in &c.params {
            if p.output {
                bc.params.push(Param::output(&p.name, typed_null(&p.typ)));
                bc.outputs.push(Output {
                    name: p.name.clone(),
                    typ: p.typ.clone(),
                });
                continue;
            }
            let (src, label) = if p.arg.is_empty() {
                (p.value.as_ref(), p.name.as_str())
            } else {
                used.insert(p.arg.as_str());
                let v = args.get(&p.arg);
                if v.is_none() && !p.nullable {
                    return Err(format!("invalid_args: {}: required", p.arg));
                }
                (v, p.arg.as_str())
            };
            let val = bind_value(p, src).map_err(|e| format!("invalid_args: {label}: {e}"))?;
            bc.params.push(Param::new(&p.name, val));
        }
        calls.push(bc);
    }
    let mut extra: Vec<&String> = args.keys().filter(|k| !used.contains(k.as_str())).collect();
    extra.sort();
    if let Some(k) = extra.first() {
        return Err(format!("invalid_args: {k}: not an argument of this op"));
    }
    Ok(calls)
}
