//! `CheckoutScanTally`: per hotel day, which checkouts the scan saw, reported and skipped
//! (with the reason), so the hub has the denominator of its checkout → earn reconciliation.
//!
//! Counted by key, not summed: a booking that fails is scanned again every minute, and only
//! its last state counts. Memory only: a restart counts again and the hub keeps the
//! greatest numbers.

use std::collections::{BTreeMap, HashMap};

use dl_time::Date;

use crate::earn::wire::ScanSummary;

/// `RetainDays`.
pub const RETAIN_DAYS: i64 = 2;
/// `ReasonReportFailed`.
pub const REASON_REPORT_FAILED: &str = "REPORT_FAILED";
/// `ReasonNoMember`.
pub const REASON_NO_MEMBER: &str = "NO_MEMBER";

/// The tally.
#[derive(Debug, Default)]
pub struct Tally {
    /// Per day, every key in first-seen order with its skip reason (`None`: reported).
    days: BTreeMap<Date, Vec<(String, Option<String>)>>,
    last_pushed: BTreeMap<Date, ScanSummary>,
    /// The day each key was first seen: a key keeps its day.
    key_day: HashMap<String, Date>,
}

impl Tally {
    /// `Reported`.
    pub fn reported(&mut self, day: Date, key: &str) {
        self.set(day, key, None);
    }

    /// `Skipped`.
    pub fn skipped(&mut self, day: Date, key: &str, reason: &str) {
        self.set(day, key, Some(reason.to_owned()));
    }

    fn set(&mut self, day: Date, key: &str, reason: Option<String>) {
        let day = *self.key_day.entry(key.to_owned()).or_insert(day);
        let keys = self.days.entry(day).or_default();
        match keys.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = reason,
            None => keys.push((key.to_owned(), reason)),
        }
    }

    /// `PendingReports`: the days that changed since their last push, oldest first.
    pub fn pending(&self, agent_version: Option<&str>) -> Vec<ScanSummary> {
        let mut out = Vec::new();
        for (day, keys) in &self.days {
            let mut skips: Vec<(String, i64)> = Vec::new();
            for reason in keys.iter().filter_map(|(_, r)| r.as_deref()) {
                match skips.iter_mut().find(|(r, _)| r == reason) {
                    Some(entry) => entry.1 += 1,
                    None => skips.push((reason.to_owned(), 1)),
                }
            }
            let report = ScanSummary {
                business_date: *day,
                folios_seen: keys.len() as i64,
                folios_reported: keys.iter().filter(|(_, r)| r.is_none()).count() as i64,
                folios_skipped: skips.iter().map(|(_, n)| n).sum(),
                skip_reasons: skips,
                agent_version: agent_version.map(str::to_owned),
            };
            if self
                .last_pushed
                .get(day)
                .is_some_and(|last| same(last, &report))
            {
                continue;
            }
            out.push(report);
        }
        out
    }

    /// `MarkPushed`.
    pub fn mark_pushed(&mut self, report: &ScanSummary) {
        self.last_pushed
            .insert(report.business_date, report.clone());
    }

    /// `Prune`: drops the days older than `today - RETAIN_DAYS`.
    pub fn prune(&mut self, today: Date) {
        let cutoff = today.add_days(-RETAIN_DAYS);
        self.days.retain(|d, _| *d >= cutoff);
        self.last_pushed.retain(|d, _| *d >= cutoff);
        self.key_day.retain(|_, d| *d >= cutoff);
    }
}

/// `Same`: the counts and the reasons as a dictionary (order does not matter).
fn same(a: &ScanSummary, b: &ScanSummary) -> bool {
    a.folios_seen == b.folios_seen
        && a.folios_reported == b.folios_reported
        && a.folios_skipped == b.folios_skipped
        && a.skip_reasons.len() == b.skip_reasons.len()
        && a.skip_reasons
            .iter()
            .all(|(k, n)| b.skip_reasons.iter().any(|(k2, n2)| k == k2 && n == n2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(day: u8) -> Date {
        Date::new(2026, 11, day).unwrap()
    }

    #[test]
    fn keys_count_once_and_keep_their_first_day() {
        let mut t = Tally::default();
        t.skipped(d(18), "B:1", REASON_REPORT_FAILED);
        t.skipped(d(18), "B:2", REASON_NO_MEMBER);
        t.skipped(d(18), "B:3", REASON_REPORT_FAILED);
        // Failed at 23:58, succeeded at 00:02: still the 18th, counted once.
        t.reported(d(19), "B:1");
        let p = t.pending(Some("v"));
        assert_eq!(p.len(), 1);
        assert_eq!(
            (p[0].folios_seen, p[0].folios_reported, p[0].folios_skipped),
            (3, 1, 2)
        );
        assert_eq!(
            p[0].skip_reasons,
            vec![
                (REASON_NO_MEMBER.to_owned(), 1),
                (REASON_REPORT_FAILED.to_owned(), 1)
            ]
        );
        t.mark_pushed(&p[0]);
        assert!(
            t.pending(Some("v")).is_empty(),
            "unchanged days are not pushed again"
        );
        t.reported(d(18), "B:3");
        assert_eq!(t.pending(Some("v")).len(), 1);
        t.prune(d(21));
        assert!(t.pending(None).is_empty());
        t.reported(d(21), "B:1");
        assert_eq!(
            t.pending(None)[0].business_date,
            d(21),
            "a pruned key takes a new day"
        );
    }
}
