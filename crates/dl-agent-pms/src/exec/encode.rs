//! Encoding SQL values and result sets to JSON exactly as the Go agent does.

use super::*;

/// Go's `json.Marshal(float64)`: shortest form, exponent only below 1e-6 or from 1e21.
pub fn go_float(f: f64) -> Result<String, String> {
    if !f.is_finite() {
        return Err(format!(
            "json: unsupported value: {}",
            if f.is_nan() {
                "NaN"
            } else if f > 0.0 {
                "+Inf"
            } else {
                "-Inf"
            }
        ));
    }
    let a = f.abs();
    if a != 0.0 && !(1e-6..1e21).contains(&a) {
        let s = format!("{f:e}");
        return Ok(match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        });
    }
    Ok(format!("{f}"))
}

/// Length of Go's `json.Encoder` text of `s` with `SetEscapeHTML(false)`.
pub(super) fn go_str_len(s: &str) -> usize {
    2 + s
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            '\u{0}'..='\u{1f}' => 6,
            '\u{2028}' | '\u{2029}' => 6,
            c => c.len_utf8(),
        })
        .sum::<usize>()
}

pub(super) fn raw_len(v: &J) -> usize {
    match v {
        J::String(s) => go_str_len(s),
        other => json::to_string(other).len(),
    }
}

pub(super) fn num(text: String) -> J {
    match Number::from_raw(&text) {
        Some(n) => J::Number(n),
        None => J::String(text),
    }
}

/// An instant as Go's `time.Time` holds it: the wall clock and its offset in seconds.
pub(super) struct GoTime {
    pub(super) dt: DateTime,
    pub(super) offset: i32,
}

pub(super) fn go_time(v: &V) -> Option<GoTime> {
    let utc = |dt| Some(GoTime { dt, offset: 0 });
    match v {
        V::Date(d) => utc(d.midnight()),
        V::Time(t) => utc(Date::new(1, 1, 1).ok()?.at(*t)),
        V::DateTime(dt) => utc(*dt),
        V::DateTimeOffset(t) => Some(GoTime {
            dt: t.datetime(),
            offset: t.offset().whole_seconds(),
        }),
        _ => None,
    }
}

/// ISO 8601 with an offset: datetimeoffset keeps its own, every other type is UTC;
/// milliseconds unless the value is finer, then the 7 digits of datetime2.
pub(super) fn format_time(t: &str, x: GoTime) -> Result<String, String> {
    let mut x = x;
    if t == "TIME" {
        return Ok(format!(
            "{:02}:{:02}:{:02}.{:07}",
            x.dt.hour(),
            x.dt.minute(),
            x.dt.second(),
            x.dt.nanosecond() / 100
        ));
    }
    if t != "DATETIMEOFFSET" && x.offset != 0 {
        let off = dl_time::Offset::from_seconds(x.offset).map_err(|e| e.to_string())?;
        x = GoTime {
            dt: x.dt.assume_offset(off).to_utc().datetime(),
            offset: 0,
        };
    }
    if t == "DATETIME" || t == "SMALLDATETIME" {
        let ns = x.dt.assume_utc().unix_nanos();
        let rounded = (ns + 500_000).div_euclid(1_000_000) * 1_000_000;
        x.dt = OffsetDateTime::from_unix_nanos(rounded)
            .map_err(|e| e.to_string())?
            .datetime();
    }
    let ns = x.dt.nanosecond();
    let frac = if !ns.is_multiple_of(1_000_000) {
        format!("{:07}", ns / 100)
    } else {
        format!("{:03}", ns / 1_000_000)
    };
    let zone = if x.offset == 0 {
        "Z".to_owned()
    } else {
        let a = x.offset.unsigned_abs();
        format!(
            "{}{:02}:{:02}",
            if x.offset < 0 { '-' } else { '+' },
            a / 3600,
            (a % 3600) / 60
        )
    };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{frac}{zone}",
        x.dt.year(),
        x.dt.month().number(),
        x.dt.day(),
        x.dt.hour(),
        x.dt.minute(),
        x.dt.second()
    ))
}

/// One column value as JSON, by its SQL Server type name (go-mssqldb's scan types).
pub fn encode_value(db_type: &str, v: &V) -> Result<J, String> {
    let t = db_type.to_uppercase();
    Ok(match v {
        V::Null => J::Null,
        V::Bool(b) => J::Bool(*b),
        V::I8(_) | V::I16(_) | V::I32(_) | V::I64(_) => {
            let n: i64 = v.convert().map_err(|e| e.to_string())?;
            // tedious returns bigint as a string; past 2^53 a number loses digits.
            if t == "BIGINT" {
                J::String(n.to_string())
            } else {
                num(n.to_string())
            }
        }
        V::F32(f) => num(go_float(f64::from(*f))?),
        V::F64(f) => num(go_float(*f)?),
        V::Decimal(_) | V::Uuid(_) | V::String(_) => {
            J::String(v.convert::<String>().map_err(|e| e.to_string())?)
        }
        V::Bytes(b) => match t.as_str() {
            "BINARY" | "VARBINARY" | "IMAGE" | "TIMESTAMP" | "ROWVERSION" | "UDT" => {
                J::String(dl_encoding::base64::STANDARD.encode(b))
            }
            _ => J::String(String::from_utf8_lossy(b).into_owned()),
        },
        V::Date(_) | V::Time(_) | V::DateTime(_) | V::DateTimeOffset(_) => {
            let gt = go_time(v).ok_or("bad time value")?;
            J::String(format_time(&t, gt)?)
        }
        other => return Err(format!("type {t}: unsupported value {}", other.kind())),
    })
}

/// A read-back OUTPUT parameter, encoded like a column of the same type.
pub(super) fn encode_output(o: &Output, v: Option<&V>) -> Result<J, ExecErr> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(J::Null);
    };
    let conv = |e: dl_tds::Error| ExecErr::Db(DbError::Tds(e));
    match o.typ.as_str() {
        "bit" => Ok(J::Bool(v.convert::<bool>().map_err(conv)?)),
        "tinyint" | "smallint" | "int" | "bigint" => {
            let n: i64 = v.convert().map_err(conv)?;
            Ok(if o.typ == "bigint" {
                J::String(n.to_string())
            } else {
                num(n.to_string())
            })
        }
        "date" | "datetime2" | "datetimeoffset" => {
            let gt = go_time(v).ok_or(ExecErr::Encode(format!("output {}: not a time", o.name)))?;
            let t = match o.typ.as_str() {
                "date" => "DATE",
                "datetime2" => "DATETIME2",
                _ => "DATETIMEOFFSET",
            };
            format_time(t, gt).map(J::String).map_err(ExecErr::Encode)
        }
        "varbinary" => Ok(J::String(
            dl_encoding::base64::STANDARD.encode(v.convert::<Vec<u8>>().map_err(conv)?),
        )),
        _ => Ok(J::String(v.convert::<String>().map_err(conv)?)),
    }
}

/// Collects the result sets of one op under its caps.
pub(super) struct SetBuilder {
    pub(super) max_rows: usize,
    pub(super) max_bytes: usize,
    pub(super) bytes: usize,
    pub(super) sets: Vec<Vec<Row>>,
    pub(super) rowcounts: Vec<i64>,
}

impl SetBuilder {
    pub(super) fn read_sets(&mut self, results: &dl_tds::Results) -> Result<(), ExecErr> {
        for rs in results.sets() {
            let cols = rs.columns();
            if cols.is_empty() {
                continue;
            }
            let mut set = Vec::new();
            for r in rs.rows() {
                if set.len() >= self.max_rows {
                    return Err(ExecErr::TooLarge);
                }
                let mut row = Row::new();
                for (i, (c, v)) in cols.iter().zip(r.values()).enumerate() {
                    let j = encode_value(c.type_name(), v)
                        .map_err(|e| ExecErr::Encode(format!("column {i}: {e}")))?;
                    self.bytes += raw_len(&j) + c.name().len() + 4;
                    row.insert(c.name().to_owned(), j);
                }
                if self.bytes > self.max_bytes {
                    return Err(ExecErr::TooLarge);
                }
                set.push(row);
            }
            self.rowcounts.push(set.len() as i64);
            self.sets.push(set);
        }
        Ok(())
    }
}
