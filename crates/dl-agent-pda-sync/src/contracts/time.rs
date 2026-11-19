//! .NET date and time values the wire and the mapper use: [`DateTimeOffset`] (an instant
//! with the offset it was written at), [`DateTime`] (a wall clock with a kind),
//! [`TimeSpan`] text, and [`TimeZone`] (the part of `TimeZoneInfo` the agent needs: base
//! offset plus yearly daylight-saving rules, `IsInvalidTime`, `IsAmbiguousTime`,
//! `ConvertTimeToUtc`, `ConvertTime`).
//!
//! Everything counts .NET ticks (100 ns) so values round-trip exactly with the C# agent
//! and the cloud: the range is 0001-01-01 to 9999-12-31, sub-tick precision does not
//! exist.

use std::sync::{Arc, RwLock};

use crate::util::stj::{self, IsoOffset};

/// Ticks per second.
pub const TICKS_PER_SECOND: i64 = 10_000_000;
/// Ticks per minute.
pub const TICKS_PER_MINUTE: i64 = 60 * TICKS_PER_SECOND;
/// Ticks per hour.
pub const TICKS_PER_HOUR: i64 = 60 * TICKS_PER_MINUTE;
/// Ticks per day.
pub const TICKS_PER_DAY: i64 = 24 * TICKS_PER_HOUR;
/// Ticks of 1970-01-01T00:00:00 (`DateTime.UnixEpoch.Ticks`).
pub const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
/// Ticks of 9999-12-31T23:59:59.9999999 (`DateTime.MaxValue.Ticks`).
pub const MAX_TICKS: i64 = 3_155_378_975_999_999_999;

/// Calendar parts of a tick count: (year, month, day, hour, minute, second, ticks of the
/// second).
pub fn civil_from_ticks(ticks: i64) -> (i32, u32, u32, u32, u32, u32, i64) {
    let days = ticks.div_euclid(TICKS_PER_DAY);
    let tod = ticks.rem_euclid(TICKS_PER_DAY);
    // Days since 0001-01-01 -> civil date (H. Hinnant, shifted to the 0000-03-01 era).
    let z = days + 306; // 0000-03-01 .. 0001-01-01 is 306 days
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    let secs = tod / TICKS_PER_SECOND;
    (
        y,
        m,
        d,
        (secs / 3600) as u32,
        ((secs / 60) % 60) as u32,
        (secs % 60) as u32,
        tod % TICKS_PER_SECOND,
    )
}

/// Ticks of a calendar date and time; `None` outside 0001..9999 or for an invalid date.
pub fn ticks_from_civil(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = stj::days_from_civil(i64::from(year), month, day)?;
    Some(
        days * TICKS_PER_DAY
            + i64::from(hour) * TICKS_PER_HOUR
            + i64::from(minute) * TICKS_PER_MINUTE
            + i64::from(second) * TICKS_PER_SECOND,
    )
}

fn ticks_to_unix_nanos(ticks: i64) -> i128 {
    i128::from(ticks - UNIX_EPOCH_TICKS) * 100
}

fn in_range(ticks: i64) -> bool {
    (0..=MAX_TICKS).contains(&ticks)
}

/// `yyyy-MM-ddTHH:mm:ss.fffffff` (always 7 fraction digits, the "O" format).
fn round_trip_wall(ticks: i64) -> String {
    let (y, mo, d, h, mi, s, f) = civil_from_ticks(ticks);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{f:07}")
}

fn offset_text(minutes: i16) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let a = minutes.unsigned_abs();
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

/// A .NET `DateTimeOffset`: an instant (UTC ticks) and the offset it is shown at.
///
/// Equality, ordering and hashing compare the instant only, as .NET does
/// (`2026-01-01T07:00+07:00 == 2026-01-01T00:00Z`). JSON text is what System.Text.Json
/// writes ([`DateTimeOffset::to_json_string`]: `2026-11-04T10:00:00+07:00`, fraction
/// digits only when non-zero); [`DateTimeOffset::to_round_trip_string`] is the "O"
/// format with 7 fraction digits.
#[derive(Debug, Clone, Copy)]
pub struct DateTimeOffset {
    utc_ticks: i64,
    offset_minutes: i16,
}

impl PartialEq for DateTimeOffset {
    fn eq(&self, other: &Self) -> bool {
        self.utc_ticks == other.utc_ticks
    }
}

impl Eq for DateTimeOffset {}

impl PartialOrd for DateTimeOffset {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DateTimeOffset {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.utc_ticks.cmp(&other.utc_ticks)
    }
}

impl std::hash::Hash for DateTimeOffset {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.utc_ticks.hash(state);
    }
}

impl std::fmt::Display for DateTimeOffset {
    /// The System.Text.Json text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_json_string())
    }
}

impl DateTimeOffset {
    /// 1970-01-01T00:00:00+00:00 (`DateTimeOffset.UnixEpoch`).
    pub const UNIX_EPOCH: DateTimeOffset = DateTimeOffset {
        utc_ticks: UNIX_EPOCH_TICKS,
        offset_minutes: 0,
    };

    /// 0001-01-01T00:00:00+00:00 (`DateTimeOffset.MinValue`, the default of a missing
    /// member).
    pub const MIN: DateTimeOffset = DateTimeOffset {
        utc_ticks: 0,
        offset_minutes: 0,
    };

    /// From UTC ticks and an offset in minutes (-840..=840); `None` when the UTC or the
    /// local clock falls outside 0001..9999, as .NET refuses.
    pub fn new(utc_ticks: i64, offset_minutes: i16) -> Option<DateTimeOffset> {
        if !(-840..=840).contains(&offset_minutes) || !in_range(utc_ticks) {
            return None;
        }
        let local = utc_ticks.checked_add(i64::from(offset_minutes) * TICKS_PER_MINUTE)?;
        in_range(local).then_some(DateTimeOffset {
            utc_ticks,
            offset_minutes,
        })
    }

    /// From a local wall clock (ticks) and its offset.
    pub fn from_local(local_ticks: i64, offset_minutes: i16) -> Option<DateTimeOffset> {
        let utc = local_ticks.checked_sub(i64::from(offset_minutes) * TICKS_PER_MINUTE)?;
        DateTimeOffset::new(utc, offset_minutes)
    }

    /// A UTC calendar time (`new DateTimeOffset(y, M, d, h, m, s, TimeSpan.Zero)`).
    pub fn utc(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<Self> {
        DateTimeOffset::new(ticks_from_civil(year, month, day, hour, minute, second)?, 0)
    }

    /// From nanoseconds since the Unix epoch (truncated to ticks) at an offset.
    pub fn from_unix_nanos(nanos: i128, offset_minutes: i16) -> Option<DateTimeOffset> {
        let ticks = i64::try_from(nanos.div_euclid(100))
            .ok()?
            .checked_add(UNIX_EPOCH_TICKS)?;
        DateTimeOffset::new(ticks, offset_minutes)
    }

    /// From milliseconds since the Unix epoch, UTC (`FromUnixTimeMilliseconds`).
    pub fn from_unix_millis(millis: i64) -> Option<DateTimeOffset> {
        DateTimeOffset::from_unix_nanos(i128::from(millis) * 1_000_000, 0)
    }

    /// From seconds since the Unix epoch, UTC (`FromUnixTimeSeconds`).
    pub fn from_unix_seconds(seconds: i64) -> Option<DateTimeOffset> {
        DateTimeOffset::from_unix_nanos(i128::from(seconds) * 1_000_000_000, 0)
    }

    /// The current time in UTC (`DateTimeOffset.UtcNow`).
    pub fn now_utc() -> DateTimeOffset {
        let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i128::try_from(d.as_nanos()).unwrap_or(0),
            Err(e) => -i128::try_from(e.duration().as_nanos()).unwrap_or(0),
        };
        DateTimeOffset::from_unix_nanos(nanos, 0).unwrap_or(DateTimeOffset::UNIX_EPOCH)
    }

    /// UTC ticks since 0001-01-01 (`UtcTicks`).
    pub fn utc_ticks(self) -> i64 {
        self.utc_ticks
    }

    /// Ticks of the local wall clock (`Ticks`).
    pub fn local_ticks(self) -> i64 {
        self.utc_ticks + i64::from(self.offset_minutes) * TICKS_PER_MINUTE
    }

    /// The offset in minutes.
    pub fn offset_minutes(self) -> i16 {
        self.offset_minutes
    }

    /// Nanoseconds since the Unix epoch.
    pub fn unix_nanos(self) -> i128 {
        ticks_to_unix_nanos(self.utc_ticks)
    }

    /// Milliseconds since the Unix epoch (`ToUnixTimeMilliseconds`, floored).
    pub fn unix_millis(self) -> i64 {
        (self.utc_ticks - UNIX_EPOCH_TICKS).div_euclid(10_000)
    }

    /// Seconds since the Unix epoch (`ToUnixTimeSeconds`, floored).
    pub fn unix_seconds(self) -> i64 {
        (self.utc_ticks - UNIX_EPOCH_TICKS).div_euclid(TICKS_PER_SECOND)
    }

    /// The same instant at another offset (`ToOffset`); `None` out of range.
    pub fn to_offset(self, offset_minutes: i16) -> Option<DateTimeOffset> {
        DateTimeOffset::new(self.utc_ticks, offset_minutes)
    }

    /// The same instant at +00:00 (`ToUniversalTime`).
    pub fn to_utc(self) -> DateTimeOffset {
        DateTimeOffset {
            utc_ticks: self.utc_ticks,
            offset_minutes: 0,
        }
    }

    /// The UTC `DateTime` (`UtcDateTime`).
    pub fn utc_date_time(self) -> DateTime {
        DateTime {
            ticks: self.utc_ticks,
            kind: DateTimeKind::Utc,
        }
    }

    /// Adds ticks (may be negative); `None` out of range.
    pub fn checked_add_ticks(self, ticks: i64) -> Option<DateTimeOffset> {
        DateTimeOffset::new(self.utc_ticks.checked_add(ticks)?, self.offset_minutes)
    }

    /// Adds a duration; `None` out of range.
    pub fn checked_add(self, d: std::time::Duration) -> Option<DateTimeOffset> {
        self.checked_add_ticks(i64::try_from(d.as_nanos() / 100).ok()?)
    }

    /// Subtracts a duration; `None` out of range.
    pub fn checked_sub(self, d: std::time::Duration) -> Option<DateTimeOffset> {
        self.checked_add_ticks(-i64::try_from(d.as_nanos() / 100).ok()?)
    }

    /// Adds seconds like `AddSeconds(double)` (rounded to whole ticks); saturates at the
    /// ends of the range.
    pub fn add_seconds(self, seconds: f64) -> DateTimeOffset {
        let ticks = (seconds * TICKS_PER_SECOND as f64).round();
        let ticks = if ticks.is_nan() { 0.0 } else { ticks };
        let t = (self.utc_ticks as f64 + ticks).clamp(0.0, MAX_TICKS as f64) as i64;
        DateTimeOffset {
            utc_ticks: t,
            offset_minutes: self.offset_minutes,
        }
        .clamped()
    }

    fn clamped(self) -> DateTimeOffset {
        DateTimeOffset::new(self.utc_ticks, self.offset_minutes).unwrap_or(DateTimeOffset {
            utc_ticks: self.utc_ticks.clamp(0, MAX_TICKS),
            offset_minutes: 0,
        })
    }

    /// `self - earlier` in ticks (negative when `earlier` is later).
    pub fn ticks_since(self, earlier: DateTimeOffset) -> i64 {
        self.utc_ticks - earlier.utc_ticks
    }

    /// `(self - earlier).TotalSeconds`.
    pub fn seconds_since(self, earlier: DateTimeOffset) -> f64 {
        self.ticks_since(earlier) as f64 / TICKS_PER_SECOND as f64
    }

    /// `self - earlier` as a duration, `None` when negative.
    pub fn duration_since(self, earlier: DateTimeOffset) -> Option<std::time::Duration> {
        let t = u64::try_from(self.ticks_since(earlier)).ok()?;
        Some(std::time::Duration::from_nanos(t.saturating_mul(100)))
    }

    /// The System.Text.Json text: `2026-11-04T10:00:00+07:00`, `...:00.1234567+00:00`.
    pub fn to_json_string(self) -> String {
        stj::date_time_offset(self.unix_nanos(), self.offset_minutes)
    }

    /// The "O" round-trip format: `2026-11-04T10:00:00.0000000+07:00`.
    pub fn to_round_trip_string(self) -> String {
        let mut s = round_trip_wall(self.local_ticks());
        s.push_str(&offset_text(self.offset_minutes));
        s
    }

    /// Parses the ISO 8601 forms System.Text.Json reads. Text without an offset is local
    /// time of [`TimeZone::local`] (the machine zone in .NET).
    pub fn parse(s: &str) -> Option<DateTimeOffset> {
        DateTimeOffset::from_iso(stj::parse_iso_date_time(s)?)
    }

    /// From a parsed ISO value (see [`DateTimeOffset::parse`]).
    pub fn from_iso(iso: stj::IsoDateTime) -> Option<DateTimeOffset> {
        match iso.offset {
            IsoOffset::Utc => DateTimeOffset::new(iso.ticks, 0),
            IsoOffset::Minutes(m) => DateTimeOffset::from_local(iso.ticks, m),
            IsoOffset::None => {
                let zone = TimeZone::local();
                let offset = zone.utc_offset_minutes_for_local(iso.ticks);
                DateTimeOffset::from_local(iso.ticks, offset)
            }
        }
    }
}

/// `DateTimeKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DateTimeKind {
    /// No zone (a vendor wall clock, a reservation time).
    #[default]
    Unspecified,
    /// UTC.
    Utc,
    /// The machine's local zone ([`TimeZone::local`]).
    Local,
}

/// A .NET `DateTime`: wall-clock ticks and a kind. Equality and ordering compare the
/// ticks only, as .NET does.
#[derive(Debug, Clone, Copy, Default)]
pub struct DateTime {
    ticks: i64,
    kind: DateTimeKind,
}

impl PartialEq for DateTime {
    fn eq(&self, other: &Self) -> bool {
        self.ticks == other.ticks
    }
}

impl Eq for DateTime {}

impl PartialOrd for DateTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DateTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ticks.cmp(&other.ticks)
    }
}

impl std::hash::Hash for DateTime {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ticks.hash(state);
    }
}

impl DateTime {
    /// From ticks and a kind; `None` out of range.
    pub fn from_ticks(ticks: i64, kind: DateTimeKind) -> Option<DateTime> {
        in_range(ticks).then_some(DateTime { ticks, kind })
    }

    /// `new DateTime(y, M, d, h, m, s)` (kind Unspecified).
    pub fn new(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<Self> {
        DateTime::from_ticks(
            ticks_from_civil(year, month, day, hour, minute, second)?,
            DateTimeKind::Unspecified,
        )
    }

    /// The same clock with another kind (`DateTime.SpecifyKind`).
    pub fn with_kind(self, kind: DateTimeKind) -> DateTime {
        DateTime {
            ticks: self.ticks,
            kind,
        }
    }

    /// Ticks since 0001-01-01 on this clock.
    pub fn ticks(self) -> i64 {
        self.ticks
    }

    /// The kind.
    pub fn kind(self) -> DateTimeKind {
        self.kind
    }

    /// Adds ticks; `None` out of range.
    pub fn checked_add_ticks(self, ticks: i64) -> Option<DateTime> {
        DateTime::from_ticks(self.ticks.checked_add(ticks)?, self.kind)
    }

    /// Calendar parts (year, month, day, hour, minute, second, ticks of the second).
    pub fn parts(self) -> (i32, u32, u32, u32, u32, u32, i64) {
        civil_from_ticks(self.ticks)
    }

    /// The System.Text.Json text: no suffix for Unspecified, `Z` for UTC, the local
    /// offset for Local; fraction digits only when non-zero.
    pub fn to_json_string(self) -> String {
        let wall = ticks_to_unix_nanos(self.ticks);
        match self.kind {
            DateTimeKind::Unspecified => stj::date_time_unspecified(wall),
            DateTimeKind::Utc => stj::date_time_utc(wall),
            DateTimeKind::Local => {
                let off = TimeZone::local().utc_offset_minutes_for_local(self.ticks);
                let utc = wall - i128::from(off) * 60_000_000_000;
                stj::date_time_offset(utc, off)
            }
        }
    }

    /// The "O" round-trip format (`...T19:00:00.0000000`, `Z` for UTC).
    pub fn to_round_trip_string(self) -> String {
        let mut s = round_trip_wall(self.ticks);
        match self.kind {
            DateTimeKind::Unspecified => {}
            DateTimeKind::Utc => s.push('Z'),
            DateTimeKind::Local => s.push_str(&offset_text(
                TimeZone::local().utc_offset_minutes_for_local(self.ticks),
            )),
        }
        s
    }

    /// From a parsed ISO value as System.Text.Json reads a `DateTime`: no offset gives
    /// Unspecified, `Z` gives UTC, an offset converts to the local zone (kind Local).
    pub fn from_iso(iso: stj::IsoDateTime) -> Option<DateTime> {
        match iso.offset {
            IsoOffset::None => DateTime::from_ticks(iso.ticks, DateTimeKind::Unspecified),
            IsoOffset::Utc => DateTime::from_ticks(iso.ticks, DateTimeKind::Utc),
            IsoOffset::Minutes(m) => {
                let utc = iso.ticks - i64::from(m) * TICKS_PER_MINUTE;
                let local = TimeZone::local().utc_to_local_ticks(utc);
                DateTime::from_ticks(local, DateTimeKind::Local)
            }
        }
    }

    /// Parses the ISO 8601 forms System.Text.Json reads (see [`DateTime::from_iso`]).
    pub fn parse_json(s: &str) -> Option<DateTime> {
        DateTime::from_iso(stj::parse_iso_date_time(s)?)
    }

    /// The subset of `DateTime.TryParse(s, InvariantCulture, DateTimeStyles.None)` that
    /// vendor text dates use: `yyyy-MM-dd[( |T)HH:mm[:ss[.fffffff]]][Z|±HH:mm]` and
    /// `MM/dd/yyyy[ h:mm[:ss][ AM|PM]]`, surrounding spaces allowed. A `Z` or an offset
    /// converts to the local zone (kind Local), as .NET does.
    pub fn try_parse_invariant(s: &str) -> Option<DateTime> {
        let s = s.trim();
        if let Some(iso) = stj::parse_iso_date_time(&s.replacen(' ', "T", 1)) {
            return match iso.offset {
                IsoOffset::None => DateTime::from_ticks(iso.ticks, DateTimeKind::Unspecified),
                IsoOffset::Utc => {
                    let local = TimeZone::local().utc_to_local_ticks(iso.ticks);
                    DateTime::from_ticks(local, DateTimeKind::Local)
                }
                IsoOffset::Minutes(_) => DateTime::from_iso(iso),
            };
        }
        parse_us_date(s)
    }
}

/// `MM/dd/yyyy[ h:mm[:ss][ AM|PM]]` (the invariant culture's short date and long time).
fn parse_us_date(s: &str) -> Option<DateTime> {
    let mut parts = s.split_whitespace();
    let date = parts.next()?;
    let mut d = date.split('/');
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let year: i32 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    let (mut h, mut mi, mut sec) = (0u32, 0u32, 0u32);
    if let Some(time) = parts.next() {
        let mut t = time.split(':');
        h = t.next()?.parse().ok()?;
        mi = t.next()?.parse().ok()?;
        if let Some(x) = t.next() {
            sec = x.parse().ok()?;
        }
        if t.next().is_some() {
            return None;
        }
        match parts.next().map(str::to_ascii_uppercase).as_deref() {
            None => {}
            Some("AM") if (1..=12).contains(&h) => h %= 12,
            Some("PM") if (1..=12).contains(&h) => h = h % 12 + 12,
            _ => return None,
        }
    }
    if parts.next().is_some() {
        return None;
    }
    DateTime::new(year, month, day, h, mi, sec)
}

/// `TimeSpan.ToString()` ("c"): `[-][d.]hh:mm:ss[.fffffff]`.
pub fn time_span_text(ticks: i64) -> String {
    let neg = ticks < 0;
    let a = ticks.unsigned_abs();
    let tps = TICKS_PER_SECOND as u64;
    let days = a / (TICKS_PER_DAY as u64);
    let rem = a % (TICKS_PER_DAY as u64);
    let secs = rem / tps;
    let frac = rem % tps;
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    if days > 0 {
        s.push_str(&format!("{days}."));
    }
    s.push_str(&format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    ));
    if frac > 0 {
        s.push_str(&format!(".{frac:07}"));
    }
    s
}

/// When a daylight-saving transition happens in a year (`TimeZoneInfo.TransitionTime`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionTime {
    /// A fixed day of a month.
    Fixed {
        /// 1..=12.
        month: u32,
        /// 1..=31.
        day: u32,
        /// Time of day in ticks.
        time_of_day: i64,
    },
    /// The `week`-th `weekday` of a month (`week` 5 = the last one).
    Floating {
        /// 1..=12.
        month: u32,
        /// 1..=5.
        week: u32,
        /// 0 = Sunday .. 6 = Saturday.
        weekday: u32,
        /// Time of day in ticks.
        time_of_day: i64,
    },
}

impl TransitionTime {
    /// The local tick count of the transition in `year`.
    fn ticks_in(self, year: i32) -> Option<i64> {
        match self {
            TransitionTime::Fixed {
                month,
                day,
                time_of_day,
            } => {
                let days = stj::days_from_civil(i64::from(year), month, day)?;
                Some(days * TICKS_PER_DAY + time_of_day)
            }
            TransitionTime::Floating {
                month,
                week,
                weekday,
                time_of_day,
            } => {
                let first = stj::days_from_civil(i64::from(year), month, 1)?;
                // 0001-01-01 was a Monday.
                let first_weekday = (first + 1).rem_euclid(7) as u32;
                let mut day = first + i64::from((weekday + 7 - first_weekday) % 7);
                day += i64::from(week.clamp(1, 5) - 1) * 7;
                let (_, m, ..) = civil_from_ticks(day * TICKS_PER_DAY);
                if m != month {
                    day -= 7;
                }
                Some(day * TICKS_PER_DAY + time_of_day)
            }
        }
    }
}

/// One `TimeZoneInfo.AdjustmentRule`: daylight saving between `start` (local standard
/// time) and `end` (local daylight time) of every year from `first_year` to `last_year`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustmentRule {
    /// First year the rule applies to.
    pub first_year: i32,
    /// Last year the rule applies to (inclusive).
    pub last_year: i32,
    /// `DaylightDelta` in ticks (usually one hour).
    pub daylight_delta: i64,
    /// `DaylightTransitionStart`.
    pub start: TransitionTime,
    /// `DaylightTransitionEnd`.
    pub end: TransitionTime,
}

/// The part of `TimeZoneInfo` the agent uses: a base offset and yearly daylight rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeZone {
    id: String,
    base_offset: i64,
    rules: Vec<AdjustmentRule>,
}

static LOCAL: RwLock<Option<Arc<TimeZone>>> = RwLock::new(None);

const fn floating(month: u32, week: u32, weekday: u32, hour: i64) -> TransitionTime {
    TransitionTime::Floating {
        month,
        week,
        weekday,
        time_of_day: hour * TICKS_PER_HOUR,
    }
}

/// US daylight saving (Eastern, Central, Mountain, Pacific): before 2007 first Sunday
/// of April to last Sunday of October, since 2007 second Sunday of March to first
/// Sunday of November, both at 02:00.
const US_RULES: [AdjustmentRule; 2] = [
    AdjustmentRule {
        first_year: 1,
        last_year: 2006,
        daylight_delta: TICKS_PER_HOUR,
        start: floating(4, 1, 0, 2),
        end: floating(10, 5, 0, 2),
    },
    AdjustmentRule {
        first_year: 2007,
        last_year: 9999,
        daylight_delta: TICKS_PER_HOUR,
        start: floating(3, 2, 0, 2),
        end: floating(11, 1, 0, 2),
    },
];

impl TimeZone {
    /// A zone without daylight saving.
    pub fn fixed(id: &str, offset_minutes: i32) -> TimeZone {
        TimeZone {
            id: id.to_owned(),
            base_offset: i64::from(offset_minutes) * TICKS_PER_MINUTE,
            rules: Vec::new(),
        }
    }

    /// A zone with rules (e.g. built from the Windows registry `TZI` value).
    pub fn with_rules(id: &str, base_offset_minutes: i32, rules: Vec<AdjustmentRule>) -> TimeZone {
        TimeZone {
            id: id.to_owned(),
            base_offset: i64::from(base_offset_minutes) * TICKS_PER_MINUTE,
            rules,
        }
    }

    /// `TimeZoneInfo.Utc`.
    pub fn utc() -> TimeZone {
        TimeZone::fixed("UTC", 0)
    }

    /// `TimeZoneInfo.FindSystemTimeZoneById` for the zones the stores use, by Windows
    /// or IANA id (case-insensitive): UTC, Indochina (+07:00, Vietnam, Thailand,
    /// Jakarta), +08:00 (Singapore, China, Malaysia, Philippines, Taiwan, Hong Kong),
    /// +09:00 (Japan, Korea) and US Eastern (with daylight saving, used by the tests).
    /// `None` for other ids (.NET: `TimeZoneNotFoundException`).
    pub fn find_system_time_zone_by_id(id: &str) -> Option<TimeZone> {
        let key = id.trim().to_ascii_lowercase();
        let fixed = |m| Some(TimeZone::fixed(id.trim(), m));
        match key.as_str() {
            "utc" | "etc/utc" | "etc/gmt" | "gmt" | "coordinated universal time" => fixed(0),
            "se asia standard time"
            | "asia/ho_chi_minh"
            | "asia/saigon"
            | "asia/bangkok"
            | "asia/jakarta"
            | "asia/phnom_penh"
            | "asia/vientiane"
            | "etc/gmt-7" => fixed(420),
            "singapore standard time"
            | "asia/singapore"
            | "asia/kuala_lumpur"
            | "china standard time"
            | "asia/shanghai"
            | "asia/hong_kong"
            | "taipei standard time"
            | "asia/taipei"
            | "asia/manila"
            | "etc/gmt-8" => fixed(480),
            "tokyo standard time"
            | "asia/tokyo"
            | "korea standard time"
            | "asia/seoul"
            | "etc/gmt-9" => fixed(540),
            "eastern standard time" | "america/new_york" | "us/eastern" => {
                Some(TimeZone::with_rules(id.trim(), -300, US_RULES.to_vec()))
            }
            _ => None,
        }
    }

    /// The machine zone (`TimeZoneInfo.Local`) as set by [`TimeZone::set_local`]; UTC
    /// until the host sets it.
    pub fn local() -> Arc<TimeZone> {
        match LOCAL.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
        .unwrap_or_else(|| Arc::new(TimeZone::utc()))
    }

    /// Sets the machine zone returned by [`TimeZone::local`] (the host does this once at
    /// start-up from the OS settings).
    pub fn set_local(zone: TimeZone) {
        let mut g = match LOCAL.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *g = Some(Arc::new(zone));
    }

    /// The id it was created with.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `BaseUtcOffset` in minutes.
    pub fn base_utc_offset_minutes(&self) -> i32 {
        (self.base_offset / TICKS_PER_MINUTE) as i32
    }

    /// `BaseUtcOffset` in ticks.
    pub fn base_utc_offset_ticks(&self) -> i64 {
        self.base_offset
    }

    /// `GetAdjustmentRules()`.
    pub fn adjustment_rules(&self) -> &[AdjustmentRule] {
        &self.rules
    }

    /// The rule and its transitions (start, end, delta) for the year of `local_ticks`.
    fn transitions(&self, local_ticks: i64) -> Option<(i64, i64, i64)> {
        let (year, ..) = civil_from_ticks(local_ticks);
        let rule = self
            .rules
            .iter()
            .find(|r| (r.first_year..=r.last_year).contains(&year))?;
        if rule.daylight_delta <= 0 {
            return None;
        }
        Some((
            rule.start.ticks_in(year)?,
            rule.end.ticks_in(year)?,
            rule.daylight_delta,
        ))
    }

    /// `IsInvalidTime`: the wall clock skipped when daylight saving starts.
    pub fn is_invalid_time(&self, local_ticks: i64) -> bool {
        self.transitions(local_ticks)
            .is_some_and(|(s, _, d)| (s..s + d).contains(&local_ticks))
    }

    /// `IsAmbiguousTime`: the wall clock repeated when daylight saving ends.
    pub fn is_ambiguous_time(&self, local_ticks: i64) -> bool {
        self.transitions(local_ticks)
            .is_some_and(|(_, e, d)| (e - d..e).contains(&local_ticks))
    }

    /// Whether an unambiguous valid wall clock is in daylight time.
    fn is_daylight_local(&self, local_ticks: i64) -> bool {
        let Some((s, e, d)) = self.transitions(local_ticks) else {
            return false;
        };
        if s < e {
            local_ticks >= s + d && local_ticks < e - d
        } else {
            local_ticks < e - d || local_ticks >= s + d
        }
    }

    /// The UTC offset of a wall clock in minutes (standard offset for ambiguous and
    /// invalid times).
    pub fn utc_offset_minutes_for_local(&self, local_ticks: i64) -> i16 {
        let mut off = self.base_offset;
        if !self.is_ambiguous_time(local_ticks) && self.is_daylight_local(local_ticks) {
            off += self.transitions(local_ticks).map_or(0, |t| t.2);
        }
        (off / TICKS_PER_MINUTE) as i16
    }

    /// `ConvertTimeToUtc(wall, zone)` in ticks: `None` for an invalid time (.NET throws
    /// `ArgumentException`); an ambiguous time takes the standard offset.
    pub fn convert_time_to_utc(&self, local_ticks: i64) -> Option<i64> {
        if self.is_invalid_time(local_ticks) {
            return None;
        }
        let off = i64::from(self.utc_offset_minutes_for_local(local_ticks)) * TICKS_PER_MINUTE;
        Some(local_ticks - off)
    }

    /// The UTC offset in minutes at a UTC instant (`GetUtcOffset`).
    pub fn utc_offset_minutes_at(&self, utc_ticks: i64) -> i16 {
        let std_clock = utc_ticks + self.base_offset;
        let mut off = self.base_offset;
        if let Some((s, e, d)) = self.transitions(std_clock) {
            let daylight = if s < e {
                std_clock >= s && std_clock < e - d
            } else {
                std_clock < e - d || std_clock >= s
            };
            if daylight {
                off += d;
            }
        }
        (off / TICKS_PER_MINUTE) as i16
    }

    /// The wall clock of a UTC instant in ticks.
    pub fn utc_to_local_ticks(&self, utc_ticks: i64) -> i64 {
        utc_ticks + i64::from(self.utc_offset_minutes_at(utc_ticks)) * TICKS_PER_MINUTE
    }

    /// `TimeZoneInfo.ConvertTime(instant, zone)`: the instant at the zone's offset.
    pub fn convert_time(&self, t: DateTimeOffset) -> DateTimeOffset {
        let off = self.utc_offset_minutes_at(t.utc_ticks());
        t.to_offset(off).unwrap_or(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        ticks_from_civil(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn civil_round_trips() {
        for t in [
            0,
            UNIX_EPOCH_TICKS,
            MAX_TICKS,
            wall(2024, 2, 29, 23, 59, 59) + 1234567,
        ] {
            let (y, mo, d, h, mi, s, f) = civil_from_ticks(t);
            assert_eq!(ticks_from_civil(y, mo, d, h, mi, s).unwrap() + f, t);
        }
        assert_eq!(civil_from_ticks(UNIX_EPOCH_TICKS).0, 1970);
    }

    #[test]
    fn offsets_and_text() {
        let t = DateTimeOffset::utc(2026, 10, 24, 1, 2, 3).unwrap();
        assert_eq!(t.to_json_string(), "2026-10-24T01:02:03+00:00");
        assert_eq!(
            t.to_round_trip_string(),
            "2026-10-24T01:02:03.0000000+00:00"
        );
        let v = t.to_offset(420).unwrap().checked_add_ticks(1).unwrap();
        assert_eq!(v.to_json_string(), "2026-10-24T08:02:03.0000001+07:00");
        assert_eq!(v, t.checked_add_ticks(1).unwrap());
        assert_eq!(
            DateTimeOffset::parse("2026-10-24T08:02:03.0000001+07:00"),
            Some(v)
        );
        assert!(DateTimeOffset::parse("0001-01-01T00:00:00+01:00").is_none());
        assert_eq!(DateTimeOffset::UNIX_EPOCH.unix_millis(), 0);
        assert_eq!(
            time_span_text(
                TICKS_PER_DAY
                    + 2 * TICKS_PER_HOUR
                    + 3 * TICKS_PER_MINUTE
                    + 4 * TICKS_PER_SECOND
                    + 50_000
            ),
            "1.02:03:04.0050000"
        );
        assert_eq!(time_span_text(-1_234_567), "-00:00:00.1234567");
        assert_eq!(
            time_span_text(13 * TICKS_PER_HOUR + 30 * TICKS_PER_MINUTE),
            "13:30:00"
        );
    }

    #[test]
    fn new_york_like_dotnet() {
        // Values printed by .NET 10 for TimeZoneInfo "America/New_York".
        let ny = TimeZone::find_system_time_zone_by_id("America/New_York").unwrap();
        let cases = [
            (
                wall(2026, 3, 8, 1, 59, 59),
                false,
                false,
                Some(wall(2026, 3, 8, 6, 59, 59)),
            ),
            (wall(2026, 3, 8, 2, 0, 0), true, false, None),
            (wall(2026, 3, 8, 2, 59, 59), true, false, None),
            (
                wall(2026, 3, 8, 3, 0, 0),
                false,
                false,
                Some(wall(2026, 3, 8, 7, 0, 0)),
            ),
            (
                wall(2026, 11, 1, 0, 59, 59),
                false,
                false,
                Some(wall(2026, 11, 1, 4, 59, 59)),
            ),
            (
                wall(2026, 11, 1, 1, 0, 0),
                false,
                true,
                Some(wall(2026, 11, 1, 6, 0, 0)),
            ),
            (
                wall(2026, 11, 1, 1, 59, 59),
                false,
                true,
                Some(wall(2026, 11, 1, 6, 59, 59)),
            ),
            (
                wall(2026, 11, 1, 2, 0, 0),
                false,
                false,
                Some(wall(2026, 11, 1, 7, 0, 0)),
            ),
        ];
        for (t, inv, amb, utc) in cases {
            assert_eq!(ny.is_invalid_time(t), inv);
            assert_eq!(ny.is_ambiguous_time(t), amb);
            assert_eq!(ny.convert_time_to_utc(t), utc);
        }
        let at = |u: i64| {
            ny.convert_time(DateTimeOffset::new(u, 0).unwrap())
                .to_round_trip_string()
        };
        assert_eq!(
            at(wall(2026, 3, 8, 6, 59, 59)),
            "2026-03-08T01:59:59.0000000-05:00"
        );
        assert_eq!(
            at(wall(2026, 3, 8, 7, 0, 0)),
            "2026-03-08T03:00:00.0000000-04:00"
        );
        assert_eq!(
            at(wall(2026, 11, 1, 5, 59, 59)),
            "2026-11-01T01:59:59.0000000-04:00"
        );
        assert_eq!(
            at(wall(2026, 11, 1, 6, 0, 0)),
            "2026-11-01T01:00:00.0000000-05:00"
        );
    }

    #[test]
    fn invariant_date_parsing() {
        let p = |s: &str| {
            DateTime::try_parse_invariant(s).map(|d| (d.to_round_trip_string(), d.kind()))
        };
        assert_eq!(
            p("2026-10-24 12:00"),
            Some((
                "2026-10-24T12:00:00.0000000".to_owned(),
                DateTimeKind::Unspecified
            ))
        );
        assert_eq!(
            p("10/24/2026 1:02:03 PM").unwrap().0,
            "2026-10-24T13:02:03.0000000"
        );
        assert!(p("24/10/2026").is_none());
        assert!(p("nope").is_none());
    }
}
