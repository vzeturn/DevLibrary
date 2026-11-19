//! Go `internal/retention`: which stored backups a schedule's `retainDays` / `retainCount`
//! let go.

use crate::state::history::BackupRecord;

const DAY_MS: u64 = 86_400_000;

/// The records past `retain_days` (by age) or beyond the newest `retain_count`, newest
/// first. A limit of 0 or less is off; with both off nothing expires.
pub fn select_expired(
    records: &[BackupRecord],
    retain_days: i64,
    retain_count: i64,
    now_ms: u64,
) -> Vec<BackupRecord> {
    let mut sorted = records.to_vec();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.created_at));
    let keep = usize::try_from(retain_count).ok().filter(|n| *n > 0);
    let cutoff = u64::try_from(retain_days)
        .ok()
        .filter(|d| *d > 0)
        .map(|d| now_ms.saturating_sub(d.saturating_mul(DAY_MS)));
    sorted
        .into_iter()
        .enumerate()
        .filter(|(i, r)| keep.is_some_and(|k| *i >= k) || cutoff.is_some_and(|c| r.created_at < c))
        .map(|(_, r)| r)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_795_045_847_000;

    fn rec(days_old: u64, name: &str) -> BackupRecord {
        BackupRecord {
            schedule_id: "s".into(),
            hub_history_id: name.into(),
            database_name: "Sales".into(),
            local_file_path: format!("/b/{name}.bak"),
            cloud_file_path: format!("s3://b/{name}.bak"),
            provider: "s3".into(),
            created_at: NOW - days_old * DAY_MS - 1,
        }
    }

    fn names(v: &[BackupRecord]) -> Vec<&str> {
        v.iter().map(|r| r.hub_history_id.as_str()).collect()
    }

    #[test]
    fn by_age() {
        let recs = [rec(1, "a"), rec(10, "b"), rec(40, "c")];
        assert_eq!(names(&select_expired(&recs, 7, 0, NOW)), ["b", "c"]);
    }

    #[test]
    fn by_count_keeps_the_newest() {
        let recs = [rec(3, "c"), rec(1, "a"), rec(2, "b"), rec(4, "d")];
        assert_eq!(names(&select_expired(&recs, 0, 2, NOW)), ["c", "d"]);
    }

    #[test]
    fn both_constraints_expire_either_way() {
        let recs = [rec(1, "a"), rec(2, "b"), rec(30, "c"), rec(31, "d")];
        assert_eq!(names(&select_expired(&recs, 7, 3, NOW)), ["c", "d"]);
    }

    #[test]
    fn no_constraints_keep_everything() {
        let recs = [rec(1, "a"), rec(400, "b")];
        assert!(select_expired(&recs, 0, 0, NOW).is_empty());
        assert!(select_expired(&recs, -1, -5, NOW).is_empty());
    }
}
