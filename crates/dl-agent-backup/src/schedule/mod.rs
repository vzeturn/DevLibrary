//! When schedules are due (the Go `scheduler` package): a cron expression read in the
//! schedule's time zone.

pub mod cron;
pub mod tz;

use crate::Error;
pub use cron::Cron;

/// The next run strictly after `after_secs` (Unix seconds).
pub fn next_run(cron_expr: &str, time_zone: &str, after_secs: i64) -> Result<i64, Error> {
    let offset = tz::offset_minutes(time_zone)?;
    Cron::parse(cron_expr)?
        .next_after(after_secs, offset)
        .ok_or_else(|| Error::Schedule(format!("cron expression {cron_expr:?} never fires")))
}

/// Whether a schedule last run at `last_run` (`None` = never) is due at `now`. A schedule
/// that never ran is due at once, as in Go (its zero `lastRun` is always in the past).
pub fn is_due(
    cron_expr: &str,
    time_zone: &str,
    last_run: Option<i64>,
    now: i64,
) -> Result<bool, Error> {
    let offset = tz::offset_minutes(time_zone)?;
    let cron = Cron::parse(cron_expr)?;
    match last_run {
        None => Ok(true),
        Some(last) => Ok(cron
            .next_after(last, offset)
            .is_some_and(|next| now >= next)),
    }
}

#[cfg(test)]
mod tests {
    use super::cron::days_from_civil;
    use super::*;

    fn hcm(y: i64, mo: u32, d: u32, h: i64, mi: i64, s: i64) -> i64 {
        (days_from_civil(y, mo, d) * 1440 + h * 60 + mi - 420) * 60 + s
    }

    #[test]
    fn is_due_before_and_after_next_run() {
        let last = hcm(2026, 9, 6, 2, 0, 0);
        let before = hcm(2026, 9, 6, 12, 0, 0);
        let after = hcm(2026, 9, 7, 2, 0, 1);
        assert!(!is_due("0 2 * * *", "Asia/Ho_Chi_Minh", Some(last), before).unwrap());
        assert!(is_due("0 2 * * *", "Asia/Ho_Chi_Minh", Some(last), after).unwrap());
    }

    #[test]
    fn never_run_is_due() {
        assert!(is_due("0 2 * * *", "UTC", None, 0).unwrap());
    }

    #[test]
    fn invalid_cron_or_zone_is_an_error() {
        assert!(next_run("not a cron expr", "UTC", 0).is_err());
        assert!(next_run("0 2 * * *", "Not/ARealZone", 0).is_err());
        assert!(is_due("0 2 * * *", "Not/ARealZone", None, 0).is_err());
        assert!(next_run("0 0 31 2 *", "UTC", 0).is_err());
    }
}
