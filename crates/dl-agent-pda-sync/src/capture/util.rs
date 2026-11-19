//! Small helpers of the capture module: a dictionary that enumerates like .NET's
//! `Dictionary<TKey, TValue>`, case-insensitive comparisons, and the calendar arithmetic
//! of the vendor's naive datetimes (`AddDays`, `AddMonths`, `Date`, day keys).

use std::collections::HashMap;

use crate::config::time::{Clock, Timestamp};
use crate::contracts::time::{
    DateTime, DateTimeKind, TICKS_PER_DAY, TimeZone, UNIX_EPOCH_TICKS, ticks_from_civil,
};
use crate::util::stj::eq_ignore_case;

/// A string-keyed dictionary that enumerates in the order .NET's `Dictionary` does:
/// insertion order, with the slot of a removed entry reused by the next insert (LIFO).
/// The order shows in wire bytes (tombstones of a scan, the open-check miss counters).
#[derive(Debug, Clone)]
pub struct DotNetDict<V> {
    entries: Vec<Option<(String, V)>>,
    free: Vec<usize>,
    index: HashMap<String, usize>,
}

impl<V> Default for DotNetDict<V> {
    fn default() -> Self {
        DotNetDict {
            entries: Vec::new(),
            free: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<V> DotNetDict<V> {
    /// An empty dictionary.
    pub fn new() -> Self {
        DotNetDict::default()
    }

    /// The value of `key`.
    pub fn get(&self, key: &str) -> Option<&V> {
        let i = *self.index.get(key)?;
        self.entries.get(i)?.as_ref().map(|(_, v)| v)
    }

    /// The value of `key`, mutable.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        let i = *self.index.get(key)?;
        self.entries.get_mut(i)?.as_mut().map(|(_, v)| v)
    }

    /// Whether `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// `dict[key] = value`: replaces in place, else takes a freed slot or appends.
    pub fn insert(&mut self, key: &str, value: V) {
        if let Some(&i) = self.index.get(key) {
            self.entries[i] = Some((key.to_owned(), value));
            return;
        }
        let i = match self.free.pop() {
            Some(i) => {
                self.entries[i] = Some((key.to_owned(), value));
                i
            }
            None => {
                self.entries.push(Some((key.to_owned(), value)));
                self.entries.len() - 1
            }
        };
        self.index.insert(key.to_owned(), i);
    }

    /// `Remove(key)`; true when it was present.
    pub fn remove(&mut self, key: &str) -> bool {
        let Some(i) = self.index.remove(key) else {
            return false;
        };
        self.entries[i] = None;
        self.free.push(i);
        true
    }

    /// `Remove(key, out value)`: removes and returns the value.
    pub fn take(&mut self, key: &str) -> Option<V> {
        let i = self.index.remove(key)?;
        self.free.push(i);
        self.entries.get_mut(i)?.take().map(|(_, v)| v)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Entries in .NET enumeration order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().flatten().map(|(k, v)| (k.as_str(), v))
    }

    /// Keys in .NET enumeration order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.iter().map(|(k, _)| k)
    }
}

/// `StringComparer.OrdinalIgnoreCase.Compare`: upper-cased UTF-16 code units.
pub fn cmp_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    let up = |s: &str| -> Vec<u16> {
        s.chars()
            .map(|c| {
                let mut u = c.to_uppercase();
                match (u.next(), u.next()) {
                    (Some(x), None) => x,
                    _ => c,
                }
            })
            .collect::<String>()
            .encode_utf16()
            .collect()
    };
    up(a).cmp(&up(b))
}

/// Whether `list` contains `s` ignoring case.
pub fn contains_ignore_case<S: AsRef<str>>(list: &[S], s: &str) -> bool {
    list.iter().any(|x| eq_ignore_case(x.as_ref(), s))
}

/// `Distinct(StringComparer.OrdinalIgnoreCase)`: first spelling kept, order kept.
pub fn distinct_ignore_case(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in items {
        if !contains_ignore_case(&out, &s) {
            out.push(s);
        }
    }
    out
}

/// .NET ticks (UTC) of a clock instant.
pub fn utc_ticks(t: Timestamp) -> i64 {
    i64::try_from(t.unix_nanos().div_euclid(100)).unwrap_or(0) + UNIX_EPOCH_TICKS
}

/// `TimeZoneInfo.ConvertTime(clock.GetUtcNow(), zone).DateTime`: the site's wall clock.
pub fn site_now(clock: &dyn Clock, zone: &TimeZone) -> DateTime {
    let local = zone.utc_to_local_ticks(utc_ticks(clock.now()));
    DateTime::from_ticks(local, DateTimeKind::Unspecified).unwrap_or_default()
}

/// `DateTime.Date`.
pub fn date_of(d: DateTime) -> DateTime {
    DateTime::from_ticks(d.ticks() - d.ticks().rem_euclid(TICKS_PER_DAY), d.kind()).unwrap_or(d)
}

/// `DateTime.AddDays(days)` (clamped to the representable range).
pub fn add_days(d: DateTime, days: i64) -> DateTime {
    d.checked_add_ticks(days.saturating_mul(TICKS_PER_DAY))
        .unwrap_or(d)
}

/// `DateTime.AddMonths(months)`: the day is clamped to the target month's length.
pub fn add_months(d: DateTime, months: i32) -> DateTime {
    let (y, m, day, h, mi, s, frac) = d.parts();
    let total = i64::from(y) * 12 + i64::from(m) - 1 + i64::from(months);
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) as u32 + 1);
    let Ok(ny) = i32::try_from(ny) else { return d };
    let dim = days_in_month(ny, nm);
    match ticks_from_civil(ny, nm, day.min(dim), h, mi, s) {
        Some(t) => DateTime::from_ticks(t + frac, d.kind()).unwrap_or(d),
        None => d,
    }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `d.ToString("yyyy-MM-dd", InvariantCulture)`.
pub fn day_key(d: DateTime) -> String {
    let (y, m, day, ..) = d.parts();
    format!("{y:04}-{m:02}-{day:02}")
}

/// `DateTime.ParseExact(day, "yyyy-MM-dd", InvariantCulture)`.
pub fn parse_day_key(day: &str) -> Option<DateTime> {
    let b = day.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let p = day.get(r)?;
        if !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let y = i32::try_from(num(0..4)?).ok()?;
    DateTime::new(y, num(5..7)?, num(8..10)?, 0, 0, 0)
}

/// `Convert.ToString(value, CultureInfo.InvariantCulture)`; `None` for NULL.
pub fn invariant_string(v: &crate::contracts::mapping::RawValue) -> Option<String> {
    use crate::contracts::mapping::RawValue as V;
    let dt = |d: &DateTime| {
        let (y, m, day, h, mi, s, _) = d.parts();
        format!("{m:02}/{day:02}/{y:04} {h:02}:{mi:02}:{s:02}")
    };
    Some(match v {
        V::Null => return None,
        V::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        V::U8(n) => n.to_string(),
        V::I8(n) => n.to_string(),
        V::I16(n) => n.to_string(),
        V::U16(n) => n.to_string(),
        V::I32(n) => n.to_string(),
        V::U32(n) => n.to_string(),
        V::I64(n) => n.to_string(),
        V::U64(n) => n.to_string(),
        V::F32(f) => {
            crate::util::stj::f32_text(*f).unwrap_or_else(|| super::codecs::double_r(f64::from(*f)))
        }
        V::F64(f) => super::codecs::double_r(*f),
        V::Decimal(d) => d.to_string(),
        V::String(s) => s.clone(),
        V::Bytes(_) => "System.Byte[]".to_owned(),
        V::Guid(g) => g.to_string().to_lowercase(),
        V::DateTime(d) => dt(d),
        V::DateTimeOffset(o) => {
            let local = DateTime::from_ticks(o.local_ticks(), DateTimeKind::Unspecified)
                .unwrap_or_default();
            let off = o.offset_minutes();
            let sign = if off < 0 { '-' } else { '+' };
            let a = off.unsigned_abs();
            format!("{} {sign}{:02}:{:02}", dt(&local), a / 60, a % 60)
        }
        V::TimeSpan(t) => crate::contracts::time::time_span_text(*t),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_reuses_freed_slots_like_dotnet() {
        let mut d = DotNetDict::new();
        d.insert("a", 1);
        d.insert("b", 2);
        d.insert("c", 3);
        d.remove("a");
        d.remove("c");
        d.insert("d", 4);
        d.insert("e", 5);
        d.insert("b", 20);
        let order: Vec<(&str, i32)> = d.iter().map(|(k, v)| (k, *v)).collect();
        assert_eq!(order, [("e", 5), ("b", 20), ("d", 4)]);
        assert_eq!(d.len(), 3);
    }

    #[test]
    fn calendar_arithmetic_matches_dotnet() {
        let d = DateTime::new(2026, 3, 31, 10, 30, 0).unwrap();
        assert_eq!(day_key(add_months(d, -1)), "2026-02-28");
        assert_eq!(day_key(add_months(d, -25)), "2024-02-29");
        assert_eq!(day_key(add_days(d, 1)), "2026-04-01");
        assert_eq!(date_of(d), DateTime::new(2026, 3, 31, 0, 0, 0).unwrap());
        assert_eq!(
            parse_day_key("2026-10-24"),
            DateTime::new(2026, 10, 24, 0, 0, 0)
        );
        assert_eq!(parse_day_key("2026-1-24"), None);
        assert_eq!(
            cmp_ignore_case("tmpTrn", "TmpTax"),
            std::cmp::Ordering::Greater
        );
    }
}
