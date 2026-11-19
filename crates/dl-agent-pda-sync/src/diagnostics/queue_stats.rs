//! Depth and oldest unsent event of a site event queue file (port of
//! `SiteEventQueueStats` in `Reporting/SiteEventStore.cs`), for `status` and `diag`.
//!
//! Read-only and self-contained so it does not depend on the event store itself; it reads
//! the `events` table the store writes (`seq`, `at_utc` as `yyyy-MM-ddTHH:mm:ss.fffffffZ`).

use crate::config::time::Timestamp;
use std::path::{Path, PathBuf};

/// What a queue file holds; never creates the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteEventQueueStats {
    /// The file read.
    pub location: PathBuf,
    /// Events waiting for the cloud.
    pub depth: i64,
    /// When the oldest of them happened.
    pub oldest_at_utc: Option<Timestamp>,
    /// `<ExceptionType>: <message>` when the file could not be read.
    pub error: Option<String>,
}

/// The SQL `SiteEventQueueStats.Read` runs.
pub const STATS_SQL: &str =
    "SELECT (SELECT COUNT(*) FROM events), (SELECT at_utc FROM events ORDER BY seq LIMIT 1)";

/// `Microsoft.Data.Sqlite`'s `SqliteException` text: `SQLite Error 1: 'no such table: events'.`
fn sqlite_error_text(e: &dl_sqlite::Error) -> String {
    match e {
        dl_sqlite::Error::Sqlite { code, message, .. } => {
            format!("SqliteException: SQLite Error {code}: '{message}'.")
        }
        other => format!("SqliteException: {other}"),
    }
}

/// `SqliteSiteEventStore.ParseStamp`: an ISO date-time, read as UTC.
fn parse_stamp(s: Option<&str>) -> Option<Timestamp> {
    let s = s?.trim();
    let has_offset = s.ends_with('Z')
        || s.ends_with('z')
        || s.get(s.len().saturating_sub(6)..)
            .is_some_and(|t| t.starts_with('+') || t.starts_with('-'));
    // DateTimeStyles.AssumeUniversal: a stamp without an offset is UTC, not local.
    let text = if has_offset || s.len() <= 10 {
        s.to_owned()
    } else {
        format!("{s}Z")
    };
    Timestamp::parse_iso(&text).map(|t| t.to_utc())
}

impl SiteEventQueueStats {
    /// Reads `path` (read-write, no create, as the C# connection string asks).
    pub fn read(path: &Path) -> SiteEventQueueStats {
        let empty = |error: Option<String>| SiteEventQueueStats {
            location: path.to_path_buf(),
            depth: 0,
            oldest_at_utc: None,
            error,
        };
        if !path.is_file() {
            return empty(None);
        }
        let result =
            dl_sqlite::Connection::open(path, dl_sqlite::OpenFlags::READ_WRITE).and_then(|db| {
                db.query_row(STATS_SQL, &[], |r| {
                    Ok((r.get::<i64>(0)?, r.get::<Option<String>>(1)?))
                })
            });
        match result {
            Ok((depth, oldest)) => SiteEventQueueStats {
                location: path.to_path_buf(),
                depth,
                oldest_at_utc: parse_stamp(oldest.as_deref()),
                error: None,
            },
            Err(e) => empty(Some(sqlite_error_text(&e))),
        }
    }

    /// `N queued, oldest unsent 3m 12s ago` (or why the file could not be read).
    pub fn describe(&self, now: Timestamp) -> String {
        if let Some(e) = &self.error {
            return format!("unreadable ({e})");
        }
        match self.oldest_at_utc {
            Some(oldest) if self.depth != 0 => format!(
                "{} queued, oldest unsent {} ago",
                self.depth,
                age(now.unix_nanos() - oldest.unix_nanos())
            ),
            _ => format!("{} queued", self.depth),
        }
    }

    /// Whole seconds since the oldest event (never negative), `None` without one.
    pub fn oldest_age_seconds(&self, now: Timestamp) -> Option<i64> {
        self.oldest_at_utc.map(|o| {
            let secs = (now.unix_nanos() - o.unix_nanos()) / 1_000_000_000;
            i64::try_from(secs.max(0)).unwrap_or(i64::MAX)
        })
    }
}

/// `SiteEventQueueStats.Age`: `2d 3h`, `4h 5m`, `6m 7s` or `8s` (negative is 0).
pub fn age(nanos: i128) -> String {
    let secs = i64::try_from(nanos.max(0) / 1_000_000_000).unwrap_or(i64::MAX);
    let (days, hours, minutes) = (secs / 86_400, secs / 3600, secs / 60);
    if days >= 1 {
        format!("{days}d {}h", hours % 24)
    } else if hours >= 1 {
        format!("{hours}h {}m", minutes % 60)
    } else if minutes >= 1 {
        format!("{minutes}m {}s", secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i128 = 1_000_000_000;

    #[test]
    fn ages_read_like_the_csharp_ones() {
        assert_eq!(age(-5 * S), "0s");
        assert_eq!(age(S / 2), "0s");
        assert_eq!(age(59 * S), "59s");
        assert_eq!(age(192 * S), "3m 12s");
        assert_eq!(age((3600 * 4 + 5 * 60 + 9) * S), "4h 5m");
        assert_eq!(age((86_400 * 2 + 3600 * 3 + 1) * S), "2d 3h");
    }

    #[test]
    fn a_missing_file_is_zero_queued_and_not_created() {
        let p = std::env::temp_dir().join(format!("none-{}.db", dl_uuid::Uuid::new_v4().simple()));
        let s = SiteEventQueueStats::read(&p);
        assert_eq!(s.describe(Timestamp::now_utc()), "0 queued");
        assert!(!p.exists());
    }

    #[test]
    fn stamps_are_utc() {
        let t = parse_stamp(Some("2026-11-04T03:00:00.1234567Z")).unwrap();
        assert_eq!(t.to_stj(), "2026-11-04T03:00:00.1234567+00:00");
        let bare = parse_stamp(Some("2026-11-04T03:00:00")).unwrap();
        assert_eq!(bare.to_stj(), "2026-11-04T03:00:00+00:00");
        assert_eq!(parse_stamp(Some("x")), None);
        assert_eq!(parse_stamp(None), None);
    }
}
