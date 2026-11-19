use super::*;

/// Unix seconds of a wall-clock time in a zone `off` minutes east of UTC.
fn at(y: i64, mo: u32, d: u32, h: i64, mi: i64, off: i32) -> i64 {
    (days_from_civil(y, mo, d) * 1440 + h * 60 + mi - off as i64) * 60
}

const HCM: i32 = 420;

#[test]
fn daily_at_two_in_ho_chi_minh() {
    // Go TestNextRun: after 2026-09-06 10:00 UTC → 2026-09-07 02:00 +07.
    let c = Cron::parse("0 2 * * *").unwrap();
    let after = at(2026, 9, 6, 10, 0, 0);
    assert_eq!(c.next_after(after, HCM), Some(at(2026, 9, 7, 2, 0, HCM)));
}

#[test]
fn next_is_strictly_after_and_minute_aligned() {
    let c = Cron::parse("* * * * *").unwrap();
    let t = at(2026, 1, 1, 0, 0, 0);
    assert_eq!(c.next_after(t, 0), Some(t + 60));
    assert_eq!(c.next_after(t + 30, 0), Some(t + 60));
    assert_eq!(c.next_after(t - 1, 0), Some(t));
}

#[test]
fn invalid_expressions_are_schedule_errors() {
    for e in [
        "not a cron expr",
        "0 2 * *",
        "0 2 * * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 7",
        "5-1 * * * *",
        "*/0 * * * *",
        "1-2-3 * * * *",
        "1/2/3 * * * *",
        "@daily",
        "x * * * *",
        "99999 * * * *",
    ] {
        let err = Cron::parse(e).unwrap_err();
        assert_eq!(err.code(), "backup_schedule_invalid", "{e}");
    }
}

#[test]
fn steps_ranges_lists_and_names() {
    let c = Cron::parse("*/15 9-17 * * mon-fri").unwrap();
    // Friday 2026-11-20 17:50 → Monday 2026-11-23 09:00.
    assert_eq!(
        c.next_after(at(2026, 11, 20, 17, 50, 0), 0),
        Some(at(2026, 11, 23, 9, 0, 0))
    );
    let c = Cron::parse("30 1,13 * * *").unwrap();
    assert_eq!(
        c.next_after(at(2026, 3, 1, 2, 0, 0), 0),
        Some(at(2026, 3, 1, 13, 30, 0))
    );
    // robfig: "N/step" runs from N to the field's maximum.
    let c = Cron::parse("50/5 * * * *").unwrap();
    assert_eq!(
        c.next_after(at(2026, 3, 1, 2, 56, 0), 0),
        Some(at(2026, 3, 1, 3, 50, 0))
    );
    let c = Cron::parse("0 0 1 jan,JUL ?").unwrap();
    assert_eq!(
        c.next_after(at(2026, 2, 1, 0, 0, 0), 0),
        Some(at(2026, 7, 1, 0, 0, 0))
    );
}

#[test]
fn day_fields_or_when_both_restricted_and_when_one_is_star() {
    // Both restricted: the 15th OR any Sunday. 2026-11-15 is a Sunday; 2026-11-22 too.
    let c = Cron::parse("0 0 13 * sun").unwrap();
    assert_eq!(
        c.next_after(at(2026, 11, 1, 1, 0, 0), 0),
        Some(at(2026, 11, 8, 0, 0, 0))
    );
    assert_eq!(
        c.next_after(at(2026, 11, 8, 1, 0, 0), 0),
        Some(at(2026, 11, 13, 0, 0, 0))
    );
    // Day of month star: only Sundays.
    let c = Cron::parse("0 0 * * 0").unwrap();
    assert_eq!(
        c.next_after(at(2026, 11, 9, 0, 0, 0), 0),
        Some(at(2026, 11, 15, 0, 0, 0))
    );
}

#[test]
fn impossible_dates_never_match() {
    let c = Cron::parse("0 0 31 2 *").unwrap();
    assert_eq!(c.next_after(at(2026, 1, 1, 0, 0, 0), 0), None);
}

#[test]
fn leap_day_is_found() {
    let c = Cron::parse("0 12 29 2 *").unwrap();
    assert_eq!(
        c.next_after(at(2026, 3, 1, 0, 0, 0), HCM),
        Some(at(2028, 2, 29, 12, 0, HCM))
    );
}

#[test]
fn civil_conversion_round_trips() {
    for days in [-1_000_000i64, -1, 0, 1, 365, 20_000, 2_000_000] {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
}
