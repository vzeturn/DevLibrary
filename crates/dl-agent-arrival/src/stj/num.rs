//! .NET number and date text: shortest round-trip doubles and floats (`R` with the
//! scientific switch of `double.ToString()`), `DateTime`, `TimeSpan` (`c`) and
//! `DateTimeOffset` as STJ writes them, and the round-trip `O` form.

use dl_time::{DateTime, OffsetDateTime, Time};

/// A double as STJ writes it: `0.1`, `-0`, `1E-05`, `1E+17`, `1.7976931348623157E+308`.
pub fn format_f64(d: f64) -> String {
    shortest(&format!("{d:e}"), 17)
}

/// A float (SQL `real`) as STJ writes it: `0.1`, `1E-45`, `3.4028235E+38`.
pub fn format_f32(f: f32) -> String {
    shortest(&format!("{f:e}"), 9)
}

/// Turns Rust's shortest scientific text (`-1.25e-5`) into .NET's general format.
fn shortest(sci: &str, max_digits: i32) -> String {
    let (neg, rest) = match sci.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, sci),
    };
    let (mantissa, exp) = rest.split_once('e').unwrap_or((rest, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    if digits.is_empty() {
        return if neg { "-0".into() } else { "0".into() };
    }
    general(neg, digits, exp + 1, max_digits)
}

/// .NET `FormatGeneral`: `digits` are the significant digits (no leading or trailing
/// zeros), `dig_pos` the position of the decimal point relative to them.
pub(crate) fn general(neg: bool, digits: &str, dig_pos: i32, max_digits: i32) -> String {
    let mut out = String::with_capacity(digits.len() + 8);
    if neg {
        out.push('-');
    }
    if dig_pos > max_digits || dig_pos < -3 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = dig_pos - 1;
        out.push('E');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", e.unsigned_abs()));
        return out;
    }
    if dig_pos <= 0 {
        out.push_str("0.");
        for _ in 0..(-dig_pos) {
            out.push('0');
        }
        out.push_str(digits);
        return out;
    }
    let pos = dig_pos as usize;
    if digits.len() <= pos {
        out.push_str(digits);
        for _ in digits.len()..pos {
            out.push('0');
        }
    } else {
        out.push_str(&digits[..pos]);
        out.push('.');
        out.push_str(&digits[pos..]);
    }
    out
}

/// Seven-digit tick fraction of `nanos` (100 ns ticks).
fn ticks(nanos: u32) -> u32 {
    nanos / 100
}

fn date_time_text(dt: &DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        dt.year(),
        dt.month().number(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// A `DateTime` (kind unspecified) as STJ writes it: `2026-11-19T08:30:15.997`.
pub fn format_datetime(dt: &DateTime) -> String {
    let mut s = date_time_text(dt);
    let t = ticks(dt.nanosecond());
    if t != 0 {
        let frac = format!("{t:07}");
        s.push('.');
        s.push_str(frac.trim_end_matches('0'));
    }
    s
}

/// A `DateTime` in the round-trip `O` form without a kind suffix: `2026-11-19T00:00:00.0000000`.
pub fn format_datetime_o(dt: &DateTime) -> String {
    format!("{}.{:07}", date_time_text(dt), ticks(dt.nanosecond()))
}

/// A `TimeSpan` below one day in the constant (`c`) form: `13:05:07.0000005`.
pub fn format_time(t: &Time) -> String {
    let mut s = format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second());
    let f = ticks(t.nanosecond());
    if f != 0 {
        s.push_str(&format!(".{f:07}"));
    }
    s
}

/// A `DateTimeOffset` as STJ writes it: `2026-11-19T08:30:15.997+07:00`.
pub fn format_dto(dto: &OffsetDateTime) -> String {
    let mut s = format_datetime(&dto.datetime());
    let minutes = dto.offset().whole_minutes();
    s.push(if minutes < 0 { '-' } else { '+' });
    let m = minutes.unsigned_abs();
    s.push_str(&format!("{:02}:{:02}", m / 60, m % 60));
    s
}

/// A UTC instant in the round-trip `O` form: `2026-11-19T01:02:03.4567891Z`.
pub fn format_utc_o(at: &OffsetDateTime) -> String {
    let utc = at.to_utc();
    format!("{}Z", format_datetime_o(&utc.datetime()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_switch_to_scientific_like_dotnet() {
        assert_eq!(format_f64(0.0), "0");
        assert_eq!(format_f64(-0.0), "-0");
        assert_eq!(format_f64(0.1), "0.1");
        assert_eq!(format_f64(1e-4), "0.0001");
        assert_eq!(format_f64(1e-5), "1E-05");
        assert_eq!(format_f64(1e16), "10000000000000000");
        assert_eq!(format_f64(1e17), "1E+17");
        assert_eq!(format_f32(1e8), "100000000");
        assert_eq!(format_f32(f32::MAX), "3.4028235E+38");
    }
}
