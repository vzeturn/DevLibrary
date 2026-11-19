//! Five-field cron expressions with the semantics of `robfig/cron/v3`'s standard parser,
//! which the Go agent used: minute, hour, day of month, month, day of week; `*` and `?`,
//! `a-b` ranges, `/step` (`N/step` runs from N to the field's maximum), comma lists, month
//! and weekday names, weekdays 0–6 (Sunday = 0). When either day field is `*` both must
//! match; otherwise either may (the classic cron rule robfig follows).

use crate::Error;

/// A parsed expression: one bit per allowed value, plus the star flag of the day fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    dom_star: bool,
    dow_star: bool,
}

struct Bounds {
    min: u32,
    max: u32,
    names: &'static [&'static str],
}

const MINUTE: Bounds = Bounds {
    min: 0,
    max: 59,
    names: &[],
};
const HOUR: Bounds = Bounds {
    min: 0,
    max: 23,
    names: &[],
};
const DOM: Bounds = Bounds {
    min: 1,
    max: 31,
    names: &[],
};
const MONTH: Bounds = Bounds {
    min: 1,
    max: 12,
    names: &[
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ],
};
const DOW: Bounds = Bounds {
    min: 0,
    max: 6,
    names: &["sun", "mon", "tue", "wed", "thu", "fri", "sat"],
};

fn bad(expr: &str, why: impl std::fmt::Display) -> Error {
    Error::Schedule(format!("cron expression {expr:?}: {why}"))
}

impl Cron {
    /// Parses a five-field expression.
    pub fn parse(expr: &str) -> Result<Cron, Error> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if expr.trim_start().starts_with('@') {
            return Err(bad(expr, "descriptors such as @daily are not accepted"));
        }
        if fields.len() != 5 {
            return Err(bad(
                expr,
                format!("expected exactly 5 fields, found {}", fields.len()),
            ));
        }
        let (minute, _) = field(expr, fields[0], &MINUTE)?;
        let (hour, _) = field(expr, fields[1], &HOUR)?;
        let (dom, dom_star) = field(expr, fields[2], &DOM)?;
        let (month, _) = field(expr, fields[3], &MONTH)?;
        let (dow, dow_star) = field(expr, fields[4], &DOW)?;
        Ok(Cron {
            minute,
            hour,
            dom,
            month,
            dow,
            dom_star,
            dow_star,
        })
    }

    fn day_matches(&self, day: u32, weekday: u32) -> bool {
        let dom = self.dom & (1 << day) != 0;
        let dow = self.dow & (1 << weekday) != 0;
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    /// The first matching minute strictly after `after_secs` (Unix seconds), read in a zone
    /// `offset_min` minutes east of UTC; `None` when nothing matches within five years.
    pub fn next_after(&self, after_secs: i64, offset_min: i32) -> Option<i64> {
        let offset = offset_min as i64 * 60;
        let start_min = (after_secs + offset).div_euclid(60) + 1;
        let start_day = start_min.div_euclid(1440);
        let first_minute_of_day = start_min.rem_euclid(1440);
        for d in 0..=(5 * 366) {
            let day = start_day + d;
            let (_, month, mday) = civil_from_days(day);
            let weekday = (day + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
            if self.month & (1 << month) == 0 || !self.day_matches(mday, weekday) {
                continue;
            }
            let from = if d == 0 { first_minute_of_day } else { 0 };
            for m in from..1440 {
                let (h, mi) = (m / 60, m % 60);
                if self.hour & (1 << h) != 0 && self.minute & (1 << mi) != 0 {
                    return Some((day * 1440 + m) * 60 - offset);
                }
            }
        }
        None
    }
}

/// One field: a comma list of ranges. Returns the bits and whether it was a bare star.
fn field(expr: &str, f: &str, b: &Bounds) -> Result<(u64, bool), Error> {
    let mut bits = 0u64;
    let mut star = false;
    for part in f.split(',') {
        let (r, s) = range(expr, part, b)?;
        bits |= r;
        star |= s;
    }
    Ok((bits, star))
}

fn range(expr: &str, part: &str, b: &Bounds) -> Result<(u64, bool), Error> {
    let mut steps = part.split('/');
    let span = steps.next().unwrap_or_default();
    let step_text = steps.next();
    if steps.next().is_some() {
        return Err(bad(expr, format!("too many slashes: {part}")));
    }
    let bounds: Vec<&str> = span.split('-').collect();
    let single = bounds.len() == 1;
    let (start, mut end, mut star) = if bounds[0] == "*" || bounds[0] == "?" {
        (b.min, b.max, true)
    } else {
        let start = number(expr, bounds[0], b)?;
        let end = match bounds.len() {
            1 => start,
            2 => number(expr, bounds[1], b)?,
            _ => return Err(bad(expr, format!("too many hyphens: {part}"))),
        };
        (start, end, false)
    };
    let step = match step_text {
        None => 1,
        Some(t) => {
            let step = plain_number(expr, t)?;
            if single {
                end = b.max;
            }
            if step > 1 {
                star = false;
            }
            step
        }
    };
    if start < b.min {
        return Err(bad(
            expr,
            format!("beginning of range ({start}) below minimum ({})", b.min),
        ));
    }
    if end > b.max {
        return Err(bad(
            expr,
            format!("end of range ({end}) above maximum ({})", b.max),
        ));
    }
    if start > end {
        return Err(bad(
            expr,
            format!("beginning of range ({start}) beyond end of range ({end})"),
        ));
    }
    if step == 0 {
        return Err(bad(expr, "step of range should be a positive number"));
    }
    let mut bits = 0u64;
    let mut v = start;
    while v <= end {
        bits |= 1 << v;
        v += step;
    }
    Ok((bits, star))
}

fn number(expr: &str, s: &str, b: &Bounds) -> Result<u32, Error> {
    let lower = s.to_ascii_lowercase();
    if let Some(i) = b.names.iter().position(|n| *n == lower) {
        return Ok(i as u32 + b.min);
    }
    plain_number(expr, s)
}

fn plain_number(expr: &str, s: &str) -> Result<u32, Error> {
    if s.is_empty() || s.len() > 4 || !s.bytes().all(|c| c.is_ascii_digit()) {
        return Err(bad(expr, format!("failed to parse int from {s:?}")));
    }
    s.parse()
        .map_err(|_| bad(expr, format!("failed to parse int from {s:?}")))
}

/// Days since 1970-01-01 → (year, month 1–12, day 1–31); Howard Hinnant's algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// (year, month, day) → days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
#[path = "cron_tests.rs"]
mod tests;
