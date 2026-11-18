//! MS-DOS date and time, the timestamp format of ZIP headers.

/// An MS-DOS date and time: years 1980 to 2107, two-second resolution, no time zone
/// (writers usually store local time; [`DosDateTime::from_unix`] stores UTC).
///
/// The default is 1980-01-01 00:00:00.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DosDateTime {
    date: u16,
    time: u16,
}

impl Default for DosDateTime {
    fn default() -> Self {
        DosDateTime {
            date: (1 << 5) | 1,
            time: 0,
        }
    }
}

const MIN_UNIX: i64 = 315_532_800; // 1980-01-01T00:00:00Z

impl DosDateTime {
    /// Builds a date-time from its fields; seconds are rounded down to even. Returns
    /// `None` outside 1980-01-01 .. 2107-12-31 or for an invalid date or time.
    ///
    /// ```
    /// let t = dl_zip::DosDateTime::new(2024, 5, 6, 7, 8, 11).unwrap();
    /// assert_eq!((t.year(), t.month(), t.day()), (2024, 5, 6));
    /// assert_eq!((t.hour(), t.minute(), t.second()), (7, 8, 10));
    /// ```
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<Self> {
        if !(1980..=2107).contains(&year)
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(i64::from(year), month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return None;
        }
        Some(DosDateTime {
            date: ((year - 1980) << 9) | (u16::from(month) << 5) | u16::from(day),
            time: (u16::from(hour) << 11) | (u16::from(minute) << 5) | u16::from(second / 2),
        })
    }

    /// The raw header fields, as found in an archive (not validated).
    pub const fn from_raw(date: u16, time: u16) -> Self {
        DosDateTime { date, time }
    }

    /// The raw date field.
    pub const fn raw_date(&self) -> u16 {
        self.date
    }

    /// The raw time field.
    pub const fn raw_time(&self) -> u16 {
        self.time
    }

    /// Converts seconds since 1970-01-01T00:00:00Z to a date-time in UTC, clamped to the
    /// DOS range and rounded down to even seconds.
    pub fn from_unix(secs: i64) -> Self {
        if secs < MIN_UNIX {
            return DosDateTime::default();
        }
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        if y > 2107 {
            return DosDateTime {
                date: (127 << 9) | (12 << 5) | 31,
                time: (23 << 11) | (59 << 5) | 29,
            };
        }
        // The ranges were checked: 1980 <= y <= 2107, rem < 86 400.
        DosDateTime::new(
            y as u16,
            m,
            d,
            (rem / 3600) as u8,
            (rem / 60 % 60) as u8,
            (rem % 60) as u8,
        )
        .unwrap_or_default()
    }

    /// Interprets the fields as UTC and returns seconds since 1970-01-01T00:00:00Z, or
    /// `None` when the fields are not a valid date and time.
    pub fn to_unix(&self) -> Option<i64> {
        let (y, mo, d) = (self.year(), self.month(), self.day());
        let (h, mi, s) = (self.hour(), self.minute(), self.second());
        DosDateTime::new(y, mo, d, h, mi, s)?;
        let days = days_from_civil(i64::from(y), mo, d);
        Some(days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(s))
    }

    /// Year (1980 + the 7-bit field).
    pub const fn year(&self) -> u16 {
        1980 + (self.date >> 9)
    }

    /// Month field (1 to 12 when valid).
    pub const fn month(&self) -> u8 {
        ((self.date >> 5) & 0x0F) as u8
    }

    /// Day field (1 to 31 when valid).
    pub const fn day(&self) -> u8 {
        (self.date & 0x1F) as u8
    }

    /// Hour field (0 to 23 when valid).
    pub const fn hour(&self) -> u8 {
        (self.time >> 11) as u8
    }

    /// Minute field (0 to 59 when valid).
    pub const fn minute(&self) -> u8 {
        ((self.time >> 5) & 0x3F) as u8
    }

    /// Seconds (twice the 5-bit field, so always even).
    pub const fn second(&self) -> u8 {
        ((self.time & 0x1F) * 2) as u8
    }
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: u8) -> u8 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_and_raw_values() {
        let t = DosDateTime::new(2024, 5, 6, 7, 8, 10).unwrap();
        assert_eq!(t.raw_date(), (44 << 9) | (5 << 5) | 6);
        assert_eq!(t.raw_time(), (7 << 11) | (8 << 5) | 5);
        assert_eq!(DosDateTime::from_raw(t.raw_date(), t.raw_time()), t);
        assert_eq!(DosDateTime::default().to_unix(), Some(MIN_UNIX));
        assert!(DosDateTime::new(2023, 2, 29, 0, 0, 0).is_none());
        assert!(DosDateTime::new(2024, 2, 29, 0, 0, 0).is_some());
        assert!(DosDateTime::new(1979, 12, 31, 0, 0, 0).is_none());
        assert!(DosDateTime::new(2108, 1, 1, 0, 0, 0).is_none());
        assert_eq!(DosDateTime::from_raw(0, 0).to_unix(), None);
    }

    #[test]
    fn unix_conversion() {
        // 2024-05-06T07:08:10Z
        let t = DosDateTime::from_unix(1_714_979_290);
        assert_eq!(t, DosDateTime::new(2024, 5, 6, 7, 8, 10).unwrap());
        assert_eq!(t.to_unix(), Some(1_714_979_290));
        assert_eq!(
            DosDateTime::from_unix(1_714_979_291).to_unix(),
            Some(1_714_979_290)
        );
        assert_eq!(DosDateTime::from_unix(0), DosDateTime::default());
        assert_eq!(DosDateTime::from_unix(i64::MIN), DosDateTime::default());
        let max = DosDateTime::from_unix(i64::MAX);
        assert_eq!((max.year(), max.month(), max.day()), (2107, 12, 31));
        assert_eq!((max.hour(), max.minute(), max.second()), (23, 59, 58));
        for secs in (MIN_UNIX..4_354_819_199).step_by(86_400 * 37 + 3_602) {
            let t = DosDateTime::from_unix(secs);
            assert_eq!(t.to_unix(), Some(secs - secs % 2), "{secs}");
        }
    }
}
