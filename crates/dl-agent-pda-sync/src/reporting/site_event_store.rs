//! Site events waiting to be sent (port of `Reporting/SiteEventStore.cs`): the durable
//! queue in `events.db`, the in-memory fallback, and the queue stats for `status`/`diag`.
//!
//! Bounded: when full, the oldest `info` rows go first, then the oldest of anything, and
//! the number dropped is handed to the next event so the cloud learns about the gap.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use dl_sqlite::{Connection, OpenFlags, ToSql, TransactionBehavior, Value as SqlValue};
use dl_uuid::Uuid;

use crate::contracts::WireJson;
use crate::contracts::time::{DateTimeOffset, TICKS_PER_SECOND, civil_from_ticks};
use crate::contracts::wire::{SiteEvent, site_event_levels, site_event_limits};

/// `ISiteEventStore.DefaultCapacity`.
pub const DEFAULT_CAPACITY: usize = 5000;

/// Why a queue operation failed (`SqliteException`, `IOException`, ...).
#[derive(Debug)]
pub enum StoreError {
    /// SQLite refused.
    Sqlite(dl_sqlite::Error),
    /// A file operation failed.
    Io(std::io::Error),
}

impl StoreError {
    /// The short .NET exception type name (`SqliteException`, `IOException`).
    pub fn type_name(&self) -> &'static str {
        match self {
            StoreError::Sqlite(_) => "SqliteException",
            StoreError::Io(_) => "IOException",
        }
    }

    /// The full .NET exception type name.
    pub fn full_type_name(&self) -> &'static str {
        match self {
            StoreError::Sqlite(_) => "Microsoft.Data.Sqlite.SqliteException",
            StoreError::Io(_) => "System.IO.IOException",
        }
    }

    /// `"{type}: {message}"`, as the C# code reports an unusable file.
    pub fn describe(&self) -> String {
        format!("{}: {self}", self.type_name())
    }

    /// As a logged exception.
    pub fn to_exception(&self) -> super::log::Exception {
        super::log::Exception::new(self.full_type_name(), self.to_string())
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(e) => e.fmt(f),
            StoreError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sqlite(e) => Some(e),
            StoreError::Io(e) => Some(e),
        }
    }
}

impl From<dl_sqlite::Error> for StoreError {
    fn from(e: dl_sqlite::Error) -> StoreError {
        StoreError::Sqlite(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> StoreError {
        StoreError::Io(e)
    }
}

/// Result of the queue operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Site events waiting to be sent (`ISiteEventStore`).
pub trait SiteEventStore: Send + Sync {
    /// The SQLite file, or `None` for the in-memory queue.
    fn location(&self) -> Option<PathBuf>;
    /// Queues the event `build` returns; it receives the number of events dropped since
    /// the last enqueue.
    fn enqueue(&self, build: &mut dyn FnMut(i64) -> SiteEvent) -> Result<SiteEvent>;
    /// The oldest events, in order, without removing them.
    fn peek(&self, max: usize) -> Result<Vec<SiteEvent>>;
    /// Removes the given events.
    fn remove(&self, event_ids: &[Uuid]) -> Result<()>;
    /// Events queued.
    fn depth(&self) -> Result<i64>;
    /// When the oldest queued event happened.
    fn oldest_at_utc(&self) -> Result<Option<DateTimeOffset>>;
}

/// `at.UtcDateTime.ToString("yyyy-MM-dd'T'HH:mm:ss.fffffff'Z'")`.
pub fn stamp(at: DateTimeOffset) -> String {
    let (y, mo, d, h, mi, s, frac) = civil_from_ticks(at.utc_ticks());
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{frac:07}Z")
}

/// `DateTimeOffset.TryParse(s, AssumeUniversal | AdjustToUniversal)`.
pub fn parse_stamp(s: Option<&str>) -> Option<DateTimeOffset> {
    DateTimeOffset::parse(s?.trim()).map(DateTimeOffset::to_utc)
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS events(
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE, at_utc TEXT NOT NULL, level TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

fn count(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT COUNT(*) FROM events", &[], |r| r.get::<i64>(0))?)
}

fn meta(db: &Connection, key: &str) -> Result<Option<String>> {
    let rows = db.query("SELECT value FROM meta WHERE key=$k", &[&key], |r| {
        r.get::<SqlValue>(0)
    })?;
    Ok(match rows.into_iter().next() {
        Some(SqlValue::Text(s)) => Some(s),
        _ => None,
    })
}

fn set_meta(db: &Connection, key: &str, value: &str) -> Result<()> {
    db.execute(
        "INSERT INTO meta(key, value) VALUES ($k, $v) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        &[&key, &value],
    )?;
    Ok(())
}

/// `long.Parse` of a stored counter; garbage reads as 0 (C# would throw a
/// `FormatException`; the meta table is only written by this code).
fn parse_drops(s: Option<String>) -> i64 {
    s.and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

fn delete_oldest(db: &Connection, info_only: bool, n: i64) -> Result<i64> {
    let sql = if info_only {
        "DELETE FROM events WHERE seq IN (SELECT seq FROM events WHERE level='info' ORDER BY seq LIMIT $n)"
    } else {
        "DELETE FROM events WHERE seq IN (SELECT seq FROM events  ORDER BY seq LIMIT $n)"
    };
    Ok(i64::try_from(db.execute(sql, &[&n])?).unwrap_or(i64::MAX))
}

fn insert(db: &Connection, sql: &str, ev: &SiteEvent) -> Result<usize> {
    let id = ev.event_id.to_string();
    let at = stamp(ev.at_utc);
    let body = ev.to_json();
    let params: [&dyn ToSql; 4] = [&id, &at, &ev.level, &body];
    Ok(db.execute(sql, &params)?)
}

/// Durable queue in `events.db` (single connection, WAL), shaped like the batch outbox
/// (`SqliteSiteEventStore`).
pub struct SqliteSiteEventStore {
    db: Mutex<Connection>,
    capacity: usize,
    location: PathBuf,
}

impl std::fmt::Debug for SqliteSiteEventStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSiteEventStore")
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::path::absolute(a), std::path::absolute(b)) {
        (Ok(x), Ok(y)) => x
            .to_string_lossy()
            .to_uppercase()
            .eq(&y.to_string_lossy().to_uppercase()),
        _ => false,
    }
}

impl SqliteSiteEventStore {
    fn open(path: &Path, capacity: usize) -> Result<SqliteSiteEventStore> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(path, OpenFlags::READ_WRITE_CREATE)?;
        db.execute_batch("PRAGMA journal_mode=WAL;")?;
        db.execute_batch("PRAGMA busy_timeout=2000;")?;
        db.busy_timeout(Duration::from_millis(2000))?;
        db.execute_batch(SCHEMA)?;
        Ok(SqliteSiteEventStore {
            db: Mutex::new(db),
            capacity,
            location: path.to_path_buf(),
        })
    }

    /// Opens or creates the queue; `Err(reason)` (`"{type}: {message}"`) when the file
    /// cannot be used (`TryOpen`).
    pub fn try_open(
        path: impl AsRef<Path>,
        capacity: usize,
    ) -> std::result::Result<SqliteSiteEventStore, String> {
        SqliteSiteEventStore::open(path.as_ref(), capacity).map_err(|e| e.describe())
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn drop_corrupt(&self, db: &mut Connection, seqs: &[i64]) -> Result<()> {
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        for s in seqs {
            tx.execute("DELETE FROM events WHERE seq=$s", &[s])?;
        }
        let pending = parse_drops(meta(&tx, "pending_drops")?);
        set_meta(
            &tx,
            "pending_drops",
            &(pending + i64::try_from(seqs.len()).unwrap_or(i64::MAX)).to_string(),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Moves the events of another queue file into this one, oldest first, keeping their
    /// ids (`AdoptFrom`). Stops when this queue is full: what did not fit stays in the
    /// other file. Returns how many events left the other file, and the reason the other
    /// file could not be used, if any.
    pub fn adopt_from(&self, path: impl AsRef<Path>) -> (usize, Option<String>) {
        let path = path.as_ref();
        if !path.is_file() || same_path(path, &self.location) {
            return (0, None);
        }
        let other = match SqliteSiteEventStore::try_open(path, DEFAULT_CAPACITY) {
            Ok(o) => o,
            Err(e) => return (0, Some(e)),
        };
        let mut moved = 0;
        let result = (|| -> Result<()> {
            loop {
                let batch = other.peek(site_event_limits::MAX_BATCH)?;
                if batch.is_empty() {
                    return Ok(());
                }
                let taken = self.import(&batch)?;
                other.remove(&taken)?;
                moved += taken.len();
                if taken.len() < batch.len() {
                    return Ok(());
                }
            }
        })();
        (moved, result.err().map(|e| e.describe()))
    }

    /// Inserts events under their own ids (an id already queued is not inserted twice)
    /// until the queue is full.
    fn import(&self, events: &[SiteEvent]) -> Result<Vec<Uuid>> {
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let mut room = i64::try_from(self.capacity).unwrap_or(i64::MAX) - count(&tx)?;
        let mut taken = Vec::new();
        for ev in events {
            if room <= 0 {
                break;
            }
            let n = insert(
                &tx,
                "INSERT OR IGNORE INTO events(event_id, at_utc, level, body) VALUES ($id, $at, $level, $body)",
                ev,
            )?;
            room -= i64::try_from(n).unwrap_or(0);
            taken.push(ev.event_id);
        }
        tx.commit()?;
        Ok(taken)
    }
}

impl SiteEventStore for SqliteSiteEventStore {
    fn location(&self) -> Option<PathBuf> {
        Some(self.location.clone())
    }

    fn enqueue(&self, build: &mut dyn FnMut(i64) -> SiteEvent) -> Result<SiteEvent> {
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let mut dropped = parse_drops(meta(&tx, "pending_drops")?);
        let over = count(&tx)? - i64::try_from(self.capacity).unwrap_or(i64::MAX) + 1;
        if over > 0 {
            let info = delete_oldest(&tx, true, over)?;
            dropped += info
                + if over > info {
                    delete_oldest(&tx, false, over - info)?
                } else {
                    0
                };
        }
        let ev = build(dropped);
        insert(
            &tx,
            "INSERT INTO events(event_id, at_utc, level, body) VALUES ($id, $at, $level, $body)",
            &ev,
        )?;
        set_meta(&tx, "pending_drops", "0")?;
        tx.commit()?;
        Ok(ev)
    }

    fn peek(&self, max: usize) -> Result<Vec<SiteEvent>> {
        let mut db = self.lock();
        let n = i64::try_from(max).unwrap_or(i64::MAX);
        let rows = db.query(
            "SELECT seq, body FROM events ORDER BY seq LIMIT $n",
            &[&n],
            |r| Ok((r.get::<i64>(0)?, r.get::<String>(1)?)),
        )?;
        let mut list = Vec::new();
        let mut corrupt = Vec::new();
        for (seq, body) in rows {
            match SiteEvent::from_json(&body) {
                Ok(ev) => list.push(ev),
                Err(_) => corrupt.push(seq),
            }
        }
        if !corrupt.is_empty() {
            self.drop_corrupt(&mut db, &corrupt)?;
        }
        Ok(list)
    }

    fn remove(&self, event_ids: &[Uuid]) -> Result<()> {
        if event_ids.is_empty() {
            return Ok(());
        }
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        for id in event_ids {
            tx.execute("DELETE FROM events WHERE event_id=$id", &[&id.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn depth(&self) -> Result<i64> {
        count(&self.lock())
    }

    fn oldest_at_utc(&self) -> Result<Option<DateTimeOffset>> {
        let rows =
            self.lock()
                .query("SELECT at_utc FROM events ORDER BY seq LIMIT 1", &[], |r| {
                    r.get::<SqlValue>(0)
                })?;
        Ok(match rows.into_iter().next() {
            Some(SqlValue::Text(s)) => parse_stamp(Some(&s)),
            _ => None,
        })
    }
}

#[derive(Debug, Default)]
struct MemoryInner {
    items: Vec<SiteEvent>,
    pending_drops: i64,
}

/// Same rules as [`SqliteSiteEventStore`], for a process that has no writable place for
/// the file (`MemorySiteEventStore`).
#[derive(Debug)]
pub struct MemorySiteEventStore {
    capacity: usize,
    inner: Mutex<MemoryInner>,
}

impl Default for MemorySiteEventStore {
    fn default() -> MemorySiteEventStore {
        MemorySiteEventStore::new(DEFAULT_CAPACITY)
    }
}

impl MemorySiteEventStore {
    /// An empty queue of `capacity`.
    pub fn new(capacity: usize) -> MemorySiteEventStore {
        MemorySiteEventStore {
            capacity,
            inner: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, MemoryInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SiteEventStore for MemorySiteEventStore {
    fn location(&self) -> Option<PathBuf> {
        None
    }

    fn enqueue(&self, build: &mut dyn FnMut(i64) -> SiteEvent) -> Result<SiteEvent> {
        let mut st = self.lock();
        let mut dropped = st.pending_drops;
        let mut over = i64::try_from(st.items.len()).unwrap_or(i64::MAX)
            - i64::try_from(self.capacity).unwrap_or(i64::MAX)
            + 1;
        // Oldest info rows first, then the oldest of anything.
        let mut info = 0;
        while over > 0 && info < st.items.len() {
            if st.items[info].level == site_event_levels::INFO {
                st.items.remove(info);
                over -= 1;
                dropped += 1;
            } else {
                info += 1;
            }
        }
        while over > 0 && !st.items.is_empty() {
            st.items.remove(0);
            over -= 1;
            dropped += 1;
        }
        let ev = build(dropped);
        st.items.push(ev.clone());
        st.pending_drops = 0;
        Ok(ev)
    }

    fn peek(&self, max: usize) -> Result<Vec<SiteEvent>> {
        Ok(self.lock().items.iter().take(max).cloned().collect())
    }

    fn remove(&self, event_ids: &[Uuid]) -> Result<()> {
        self.lock()
            .items
            .retain(|e| !event_ids.contains(&e.event_id));
        Ok(())
    }

    fn depth(&self) -> Result<i64> {
        Ok(i64::try_from(self.lock().items.len()).unwrap_or(i64::MAX))
    }

    fn oldest_at_utc(&self) -> Result<Option<DateTimeOffset>> {
        Ok(self.lock().items.first().map(|e| e.at_utc))
    }
}

/// Depth and oldest unsent event of a queue file, for `status` and `diag`; never creates
/// the file (`SiteEventQueueStats`).
#[derive(Debug, Clone, PartialEq)]
pub struct SiteEventQueueStats {
    /// The file.
    pub location: PathBuf,
    /// Events queued.
    pub depth: i64,
    /// When the oldest queued event happened.
    pub oldest_at_utc: Option<DateTimeOffset>,
    /// Why the file could not be read.
    pub error: Option<String>,
}

impl SiteEventQueueStats {
    /// Reads the stats of `path`.
    pub fn read(path: impl AsRef<Path>) -> SiteEventQueueStats {
        let path = path.as_ref();
        let stats = |depth, oldest, error| SiteEventQueueStats {
            location: path.to_path_buf(),
            depth,
            oldest_at_utc: oldest,
            error,
        };
        if !path.is_file() {
            return stats(0, None, None);
        }
        let read = || -> Result<(i64, Option<String>)> {
            let db = Connection::open(path, OpenFlags::READ_WRITE)?;
            Ok(db.query_row(
                "SELECT (SELECT COUNT(*) FROM events), (SELECT at_utc FROM events ORDER BY seq LIMIT 1)",
                &[],
                |r| {
                    let oldest = match r.get::<SqlValue>(1)? {
                        SqlValue::Text(s) => Some(s),
                        _ => None,
                    };
                    Ok((r.get::<i64>(0)?, oldest))
                },
            )?)
        };
        match read() {
            Ok((depth, oldest)) => stats(depth, parse_stamp(oldest.as_deref()), None),
            Err(e) => stats(0, None, Some(e.describe())),
        }
    }

    /// `N queued, oldest unsent 3m 12s ago` (or the reason the file could not be read).
    pub fn describe(&self, now: DateTimeOffset) -> String {
        if let Some(e) = &self.error {
            return format!("unreadable ({e})");
        }
        match self.oldest_at_utc {
            Some(oldest) if self.depth != 0 => format!(
                "{} queued, oldest unsent {} ago",
                self.depth,
                age_text(now.ticks_since(oldest))
            ),
            _ => format!("{} queued", self.depth),
        }
    }
}

/// `SiteEventQueueStats.Age`: `2d 3h`, `4h 5m`, `3m 12s`, `42s` (ticks).
pub fn age_text(ticks: i64) -> String {
    let t = ticks.max(0);
    let secs = t / TICKS_PER_SECOND;
    let (days, hours, minutes, seconds) = (secs / 86_400, secs / 3600, secs / 60, secs);
    if days >= 1 {
        format!("{days}d {}h", hours % 24)
    } else if hours >= 1 {
        format!("{hours}h {}m", minutes % 60)
    } else if minutes >= 1 {
        format!("{minutes}m {}s", seconds % 60)
    } else {
        format!("{seconds}s")
    }
}
