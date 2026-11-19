//! Time zones for schedules. The agent has no tz database (no third-party crates), so it
//! knows the zones without daylight saving time that SRBackup's sites use, fixed offsets
//! (`+07:00`, `UTC+7`, `Etc/GMT-7`) and UTC. Any other zone is a schedule error
//! (`backup_schedule_invalid`) instead of a silently shifted run time.

use crate::Error;

const ZONES: &[(&str, i32)] = &[
    ("Asia/Ho_Chi_Minh", 7 * 60),
    ("Asia/Saigon", 7 * 60),
    ("Asia/Bangkok", 7 * 60),
    ("Asia/Jakarta", 7 * 60),
    ("Asia/Phnom_Penh", 7 * 60),
    ("Asia/Vientiane", 7 * 60),
    ("SE Asia Standard Time", 7 * 60),
    ("Asia/Singapore", 8 * 60),
    ("Asia/Shanghai", 8 * 60),
    ("Asia/Hong_Kong", 8 * 60),
    ("Asia/Taipei", 8 * 60),
    ("Asia/Kuala_Lumpur", 8 * 60),
    ("Asia/Manila", 8 * 60),
    ("Australia/Perth", 8 * 60),
    ("Asia/Tokyo", 9 * 60),
    ("Asia/Seoul", 9 * 60),
    ("Asia/Kolkata", 5 * 60 + 30),
    ("Asia/Calcutta", 5 * 60 + 30),
    ("Asia/Dubai", 4 * 60),
    ("UTC", 0),
    ("Etc/UTC", 0),
    ("GMT", 0),
    ("Etc/GMT", 0),
    ("Z", 0),
];

/// The zone's offset from UTC in minutes. An empty id is UTC, as Go's `time.LoadLocation`.
pub fn offset_minutes(zone: &str) -> Result<i32, Error> {
    let zone = zone.trim();
    if zone.is_empty() {
        return Ok(0);
    }
    if let Some((_, off)) = ZONES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(zone))
    {
        return Ok(*off);
    }
    // POSIX-style Etc/GMT±N has the sign inverted: Etc/GMT-7 is UTC+7.
    if let Some(rest) = zone.strip_prefix("Etc/GMT") {
        return parse_offset(rest).map(|m| -m).ok_or_else(|| unknown(zone));
    }
    let rest = zone
        .strip_prefix("UTC")
        .or_else(|| zone.strip_prefix("GMT"))
        .unwrap_or(zone);
    parse_offset(rest).ok_or_else(|| unknown(zone))
}

fn unknown(zone: &str) -> Error {
    Error::Schedule(format!(
        "time zone {zone:?} is not known to the agent (use an IANA zone without daylight \
         saving such as Asia/Ho_Chi_Minh, or a fixed offset such as +07:00)"
    ))
}

/// `+7`, `-03`, `+07:00`, `+0530`.
fn parse_offset(s: &str) -> Option<i32> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (h, m) = match digits.split_once(':') {
        Some((h, m)) => (h, m),
        None if digits.len() == 4 => digits.split_at(2),
        None => (digits, "0"),
    };
    if h.is_empty() || h.len() > 2 || !h.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if m.is_empty() || m.len() > 2 || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    if h > 14 || m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_zones_and_offsets() {
        assert_eq!(offset_minutes("Asia/Ho_Chi_Minh").unwrap(), 420);
        assert_eq!(offset_minutes("asia/saigon").unwrap(), 420);
        assert_eq!(offset_minutes("").unwrap(), 0);
        assert_eq!(offset_minutes("UTC").unwrap(), 0);
        assert_eq!(offset_minutes("+07:00").unwrap(), 420);
        assert_eq!(offset_minutes("UTC+7").unwrap(), 420);
        assert_eq!(offset_minutes("-0330").unwrap(), -210);
        assert_eq!(offset_minutes("Etc/GMT-7").unwrap(), 420);
        assert_eq!(offset_minutes("Asia/Kolkata").unwrap(), 330);
    }

    #[test]
    fn zones_with_dst_or_garbage_are_schedule_errors() {
        for z in [
            "Europe/Berlin",
            "America/New_York",
            "Mars/Olympus",
            "+25",
            "+7:99",
            "UTC+",
        ] {
            let e = offset_minutes(z).unwrap_err();
            assert_eq!(e.code(), "backup_schedule_invalid", "{z}");
        }
    }
}
