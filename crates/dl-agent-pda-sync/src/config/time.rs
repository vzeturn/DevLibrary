//! Instants with an offset (the part of .NET's `DateTimeOffset` these modules use), the
//! injectable clock (`TimeProvider`) and this machine's local UTC offset.
//!
//! A narrow local type so `config`, `transport` and `reporting` do not depend on
//! `contracts`; the integration phase may replace it by the shared one.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Nanoseconds in one .NET tick.
const TICK_NANOS: i128 = 100;
const NANOS_PER_SEC: i128 = 1_000_000_000;
const NANOS_PER_MIN: i128 = 60 * NANOS_PER_SEC;
/// 0001-01-01T00:00:00Z in nanoseconds since the Unix epoch (`DateTimeOffset.MinValue`).
pub const MIN_UNIX_NANOS: i128 = -62_135_596_800 * NANOS_PER_SEC;
/// 9999-12-31T23:59:59.9999999Z in nanoseconds since the Unix epoch.
pub const MAX_UNIX_NANOS: i128 = 253_402_300_800 * NANOS_PER_SEC - TICK_NANOS;

/// An instant (100 ns precision) and the UTC offset it was observed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
    unix_nanos: i128,
    offset_minutes: i16,
}

impl Default for Timestamp {
    /// `default(DateTimeOffset)`: 0001-01-01T00:00:00+00:00.
    fn default() -> Timestamp {
        Timestamp {
            unix_nanos: MIN_UNIX_NANOS,
            offset_minutes: 0,
        }
    }
}

impl Timestamp {
    /// An instant at UTC; sub-tick nanoseconds are dropped like .NET ticks.
    pub fn from_unix_nanos(unix_nanos: i128) -> Timestamp {
        Timestamp::with_offset(unix_nanos, 0)
    }

    /// An instant observed at `offset_minutes` east of UTC.
    pub fn with_offset(unix_nanos: i128, offset_minutes: i16) -> Timestamp {
        Timestamp {
            unix_nanos: unix_nanos.div_euclid(TICK_NANOS) * TICK_NANOS,
            offset_minutes,
        }
    }

    /// Whole seconds since the Unix epoch, at UTC.
    pub fn from_unix_seconds(seconds: i64) -> Timestamp {
        Timestamp::from_unix_nanos(i128::from(seconds) * NANOS_PER_SEC)
    }

    /// The current time from the system clock, at UTC (`DateTimeOffset.UtcNow`).
    pub fn now_utc() -> Timestamp {
        Timestamp::from_unix_nanos(system_unix_nanos())
    }

    /// Nanoseconds since 1970-01-01T00:00:00Z.
    pub fn unix_nanos(&self) -> i128 {
        self.unix_nanos
    }

    /// `ToUnixTimeSeconds()`: whole seconds since the epoch, rounded down.
    pub fn unix_seconds(&self) -> i64 {
        i64::try_from(self.unix_nanos.div_euclid(NANOS_PER_SEC)).unwrap_or(i64::MAX)
    }

    /// The offset in minutes east of UTC.
    pub fn offset_minutes(&self) -> i16 {
        self.offset_minutes
    }

    /// The same instant at UTC (`ToUniversalTime()`).
    pub fn to_utc(&self) -> Timestamp {
        Timestamp::from_unix_nanos(self.unix_nanos)
    }

    /// The same instant at `offset_minutes`.
    pub fn to_offset(&self, offset_minutes: i16) -> Timestamp {
        Timestamp::with_offset(self.unix_nanos, offset_minutes)
    }

    /// This instant moved by `d` (saturating at the representable range).
    pub fn add(&self, d: Duration) -> Timestamp {
        let n = i128::try_from(d.as_nanos()).unwrap_or(i128::MAX);
        Timestamp::with_offset(self.unix_nanos.saturating_add(n), self.offset_minutes)
    }

    /// This instant moved back by `d`.
    pub fn sub(&self, d: Duration) -> Timestamp {
        let n = i128::try_from(d.as_nanos()).unwrap_or(i128::MAX);
        Timestamp::with_offset(self.unix_nanos.saturating_sub(n), self.offset_minutes)
    }

    /// Nanoseconds since the epoch on the wall clock of the offset.
    pub fn wall_nanos(&self) -> i128 {
        self.unix_nanos + i128::from(self.offset_minutes) * NANOS_PER_MIN
    }

    /// The text System.Text.Json writes for a `DateTimeOffset`
    /// (`2026-11-04T10:00:00.12+07:00`).
    pub fn to_stj(&self) -> String {
        crate::util::stj::date_time_offset(self.unix_nanos, self.offset_minutes)
    }

    /// The round-trip `"O"` format: always seven fraction digits
    /// (`2026-11-04T10:00:00.1200000+07:00`).
    pub fn to_round_trip(&self) -> String {
        let mut s = wall_text(self.wall_nanos(), true);
        push_offset(&mut s, self.offset_minutes);
        s
    }

    /// `DateTimeOffset.ToString()` with the invariant culture:
    /// `MM/dd/yyyy HH:mm:ss zzz` (`11/04/2026 10:00:00 +07:00`).
    pub fn to_invariant_general(&self) -> String {
        let (y, mo, d, h, mi, s, _) = civil_parts(self.wall_nanos());
        let mut out = format!("{mo:02}/{d:02}/{y:04} {h:02}:{mi:02}:{s:02} ");
        push_offset(&mut out, self.offset_minutes);
        out
    }

    /// The local calendar date at the offset as `yyyyMMdd`.
    pub fn date_compact(&self) -> String {
        let (y, mo, d, ..) = civil_parts(self.wall_nanos());
        format!("{y:04}{mo:02}{d:02}")
    }

    /// The calendar parts at the offset: year, month, day, hour, minute, second, ticks.
    pub fn parts(&self) -> (i64, u32, u32, u32, u32, u32, u32) {
        civil_parts(self.wall_nanos())
    }

    /// Parses the ISO 8601 text System.Text.Json reads as a `DateTimeOffset`:
    /// `yyyy-MM-dd[THH:mm[:ss[.fffffff]]][Z|±HH:mm|±HH]`. Without an offset the
    /// machine's local offset applies, as in .NET. `None` for anything else.
    pub fn parse_iso(s: &str) -> Option<Timestamp> {
        let b = s.as_bytes();
        let num = |from: usize, len: usize| -> Option<i64> {
            let part = b.get(from..from + len)?;
            if !part.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(part).ok()?.parse().ok()
        };
        if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let year = num(0, 4)?;
        let month = u32::try_from(num(5, 2)?).ok()?;
        let day = u32::try_from(num(8, 2)?).ok()?;
        if year < 1 || !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        let mut i = 10;
        let (mut hour, mut minute, mut second, mut ticks) = (0i64, 0i64, 0i64, 0i64);
        if b.get(i) == Some(&b'T') {
            hour = num(i + 1, 2)?;
            if b.get(i + 3) != Some(&b':') {
                return None;
            }
            minute = num(i + 4, 2)?;
            i += 6;
            if b.get(i) == Some(&b':') {
                second = num(i + 1, 2)?;
                i += 3;
                if b.get(i) == Some(&b'.') {
                    i += 1;
                    let start = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    let digits = &s[start..i];
                    if digits.is_empty() || digits.len() > 16 {
                        return None;
                    }
                    let seven: String = digits.chars().chain("0000000".chars()).take(7).collect();
                    ticks = seven.parse().ok()?;
                }
            }
            if hour > 23 || minute > 59 || second > 59 {
                return None;
            }
        }
        let offset: Option<i16> = match b.get(i) {
            None => None,
            Some(b'Z') if i + 1 == b.len() => Some(0),
            Some(&c @ (b'+' | b'-')) => {
                let oh = num(i + 1, 2)?;
                let om = match b.len() - i {
                    3 => 0,
                    6 if b[i + 3] == b':' => num(i + 4, 2)?,
                    _ => return None,
                };
                if oh > 14 || om > 59 {
                    return None;
                }
                let total = i16::try_from(oh * 60 + om).ok()?;
                Some(if c == b'-' { -total } else { total })
            }
            _ => return None,
        };
        let days = days_from_civil(year, month, day);
        let wall = (i128::from(days) * 86_400 + i128::from(hour * 3600 + minute * 60 + second))
            * NANOS_PER_SEC
            + i128::from(ticks) * TICK_NANOS;
        let offset = match offset {
            Some(o) => o,
            None => local_offset_minutes(),
        };
        let unix = wall - i128::from(offset) * NANOS_PER_MIN;
        if !(MIN_UNIX_NANOS..=MAX_UNIX_NANOS).contains(&unix) {
            return None;
        }
        Some(Timestamp::with_offset(unix, offset))
    }
}

fn push_offset(out: &mut String, offset_minutes: i16) {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let a = offset_minutes.unsigned_abs();
    out.push_str(&format!("{sign}{:02}:{:02}", a / 60, a % 60));
}

/// `yyyy-MM-ddTHH:mm:ss` plus `.fffffff` (always seven digits when `seven`).
pub(crate) fn wall_text(wall_nanos: i128, seven: bool) -> String {
    let (y, mo, d, h, mi, s, ticks) = civil_parts(wall_nanos);
    let mut out = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}");
    if seven {
        out.push_str(&format!(".{ticks:07}"));
    }
    out
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date and time of a wall-clock instant: year, month, day, hour, minute, second,
/// ticks of the second.
pub(crate) fn civil_parts(wall_nanos: i128) -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = i64::try_from(wall_nanos.div_euclid(NANOS_PER_SEC)).unwrap_or(0);
    let ticks = u32::try_from(wall_nanos.rem_euclid(NANOS_PER_SEC) / TICK_NANOS).unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    let sod = u32::try_from(sod).unwrap_or(0);
    (y, m, d, sod / 3600, (sod / 60) % 60, sod % 60, ticks)
}

/// Nanoseconds since the Unix epoch from the system clock.
pub fn system_unix_nanos() -> i128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i128::try_from(d.as_nanos()).unwrap_or(i128::MAX),
        Err(e) => -i128::try_from(e.duration().as_nanos()).unwrap_or(i128::MAX),
    }
}

/// The source of "now" (`TimeProvider`); tests inject a [`ManualClock`].
pub trait Clock: Send + Sync {
    /// The current instant at UTC (`GetUtcNow()`).
    fn now(&self) -> Timestamp;
}

/// A shared clock.
pub type SharedClock = Arc<dyn Clock>;

/// The system clock (`TimeProvider.System`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now_utc()
    }
}

impl SystemClock {
    /// The system clock as a [`SharedClock`].
    pub fn shared() -> SharedClock {
        Arc::new(SystemClock)
    }
}

/// A clock that only moves when told to (the tests' `ManualTime`).
#[derive(Debug)]
pub struct ManualClock(Mutex<Timestamp>);

impl ManualClock {
    /// A clock standing at `start`.
    pub fn new(start: Timestamp) -> ManualClock {
        ManualClock(Mutex::new(start.to_utc()))
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let mut t = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *t = t.add(by);
    }

    /// Sets the clock.
    pub fn set(&self, now: Timestamp) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = now.to_utc();
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The machine's current UTC offset in minutes (local time minus UTC, `DateTimeOffset.Now`).
///
/// Windows: `ActiveTimeBias` under `HKLM\SYSTEM\CurrentControlSet\Control\TimeZoneInformation`
/// (kept current by Windows across daylight-saving changes), read at most once a minute.
/// Elsewhere, and when the value cannot be read: 0 (UTC).
pub fn local_offset_minutes() -> i16 {
    static CACHE: Mutex<Option<(Instant, i16)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, v)) = *cache
        && at.elapsed() < Duration::from_secs(60)
    {
        return v;
    }
    let v = read_local_offset();
    *cache = Some((Instant::now(), v));
    v
}

#[cfg(windows)]
fn read_local_offset() -> i16 {
    use dl_winos::registry::{Hive, Key, Value, View};
    let key = Key::open(
        Hive::LocalMachine,
        r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation",
        View::Default,
    );
    let bias = match key.and_then(|k| k.value("ActiveTimeBias")) {
        Ok(Some(Value::Dword(v))) => v as i32,
        _ => return 0,
    };
    // UTC = local + bias.
    i16::try_from(-bias)
        .ok()
        .filter(|m| m.abs() <= 14 * 60)
        .unwrap_or(0)
}

#[cfg(not(windows))]
fn read_local_offset() -> i16 {
    0
}

/// The current local time (`DateTimeOffset.Now`).
pub fn now_local() -> Timestamp {
    Timestamp::now_utc().to_offset(local_offset_minutes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_dotnet() {
        let t = Timestamp::parse_iso("2026-11-04T10:00:00.12+07:00").unwrap();
        assert_eq!(t.to_stj(), "2026-11-04T10:00:00.12+07:00");
        assert_eq!(t.to_round_trip(), "2026-11-04T10:00:00.1200000+07:00");
        assert_eq!(t.to_invariant_general(), "11/04/2026 10:00:00 +07:00");
        assert_eq!(t.to_utc().to_stj(), "2026-11-04T03:00:00.12+00:00");
        assert_eq!(t.unix_seconds(), 1_793_761_200);
        assert_eq!(t.date_compact(), "20261104");
        assert_eq!(
            Timestamp::parse_iso("2026-11-04T03:00:00Z").unwrap(),
            Timestamp::from_unix_seconds(1_793_761_200)
        );
        assert_eq!(Timestamp::default().to_stj(), "0001-01-01T00:00:00+00:00");
        for bad in [
            "2026-13-01",
            "2026-02-30T00:00",
            "x",
            "2026-11-04T10:00:00+7",
            "2026-11-04 10:00",
        ] {
            assert!(Timestamp::parse_iso(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn manual_clock_moves_on_demand() {
        let c = ManualClock::new(Timestamp::from_unix_seconds(100));
        c.advance(Duration::from_secs(5));
        assert_eq!(c.now().unix_seconds(), 105);
    }
}
