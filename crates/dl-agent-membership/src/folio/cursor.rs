//! `FolioObservationCursor`, `FolioObservationWatermark` and
//! `FolioObservationCursorStore`: where the next folio scan starts, kept per SMILE
//! database in `folio-observation-cursor.json` next to `config.dpapi`, in the .NET file
//! format (v1 is read and adopted, v2 is written), so either agent can resume the other's
//! cursor.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dl_time::{DateTime, Duration, OffsetDateTime};

use crate::identity::same_ignoring_missing_guid;
use crate::log::{self, V};
use crate::stj::{escape_into, format_datetime, format_dto};
use crate::wire::parse::{datetime_prop, int_prop, json, prop, str_prop};

/// The cursor file name.
pub const FILE_NAME: &str = "folio-observation-cursor.json";
const CURRENT_VERSION: i64 = 2;
const UNKNOWN_KEY: &str = "unknown";
const LEGACY_KEY: &str = "legacy";

/// One cursor (`FolioObservationCursor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The hotel it belongs to.
    pub hotel_id: String,
    /// Scan from here (PMS local time).
    pub watermark: DateTime,
    /// When it was written.
    pub last_scan_at: OffsetDateTime,
    /// Observations in that scan.
    pub last_batch_size: i64,
}

/// `FolioCursorIdentity`: the database being read and the hotel's baseline database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorIdentity {
    /// The fingerprint of the database being read.
    pub current: Option<String>,
    /// The fingerprint of the hotel's baseline database.
    pub baseline: Option<String>,
}

impl CursorIdentity {
    /// The key the cursor is stored under.
    pub fn key(&self) -> &str {
        match self.current.as_deref() {
            Some(c) if !c.trim().is_empty() => c,
            _ => UNKNOWN_KEY,
        }
    }

    /// Whether a v1 (single, unkeyed) cursor may be used for this database.
    pub fn can_adopt_v1(&self) -> bool {
        let Some(current) = self.current.as_deref().filter(|c| !c.trim().is_empty()) else {
            return false;
        };
        match self.baseline.as_deref() {
            None => true,
            Some(b) => b == current || same_ignoring_missing_guid(b, current),
        }
    }
}

/// `FolioObservationWatermark.Next`: past the batch, minus the overlap unless the batch was
/// full, never backwards.
pub fn next_watermark(
    current: DateTime,
    max_last_change: Option<DateTime>,
    batch: usize,
    max_row: usize,
    overlap: std::time::Duration,
) -> DateTime {
    let Some(max) = max_last_change.filter(|_| batch > 0) else {
        return current;
    };
    let candidate = if batch >= max_row {
        max
    } else {
        let secs = i64::try_from(overlap.as_secs()).unwrap_or(i64::MAX);
        max.checked_sub(Duration::from_seconds(secs)).unwrap_or(max)
    };
    candidate.max(current)
}

/// `FolioObservationWatermark.ColdStart`.
pub fn cold_start(now_pms: DateTime, lookback_days: i64) -> DateTime {
    now_pms
        .checked_sub(Duration::from_days(lookback_days.saturating_abs()))
        .unwrap_or(now_pms)
}

#[derive(Debug, Clone)]
struct Entry {
    watermark: DateTime,
    last_scan_at: OffsetDateTime,
    last_batch_size: i64,
}

impl Entry {
    fn from(c: &Cursor) -> Entry {
        Entry {
            watermark: c.watermark,
            last_scan_at: c.last_scan_at,
            last_batch_size: c.last_batch_size,
        }
    }

    fn to_cursor(&self, hotel_id: &str) -> Cursor {
        Cursor {
            hotel_id: hotel_id.to_owned(),
            watermark: self.watermark,
            last_scan_at: self.last_scan_at,
            last_batch_size: self.last_batch_size,
        }
    }
}

struct Parsed {
    hotel_id: String,
    cursors: BTreeMap<String, Entry>,
    v1: Option<Cursor>,
}

/// `FolioObservationCursorStore`.
#[derive(Debug)]
pub struct CursorStore {
    path: PathBuf,
    last_written: Mutex<BTreeMap<String, Cursor>>,
    v1_logged: Mutex<bool>,
}

impl CursorStore {
    /// A store over `dir/folio-observation-cursor.json`.
    pub fn new(dir: &Path) -> CursorStore {
        CursorStore {
            path: dir.join(FILE_NAME),
            last_written: Mutex::new(BTreeMap::new()),
            v1_logged: Mutex::new(false),
        }
    }

    /// The cursor for `hotel_id` and `identity`, or `None` for a cold start. An unreadable
    /// file is logged and treated as missing (this process's last write still counts).
    pub fn load(&self, hotel_id: &str, identity: &CursorIdentity) -> Option<Cursor> {
        let Some(file) = self.read(true) else {
            return self.remembered(hotel_id, identity.key());
        };
        if !file.hotel_id.eq_ignore_ascii_case(hotel_id) {
            log::warn(
                "folio cursor belongs to another hotel; cold start",
                &[("path", V::S(&self.path.display().to_string()))],
            );
            return None;
        }
        if let Some(v1) = file.v1 {
            if identity.key() == UNKNOWN_KEY {
                return self.remembered(hotel_id, identity.key());
            }
            self.log_v1_decision(identity);
            return identity.can_adopt_v1().then_some(v1);
        }
        if let Some(entry) = file.cursors.get(identity.key()) {
            return Some(entry.to_cursor(&file.hotel_id));
        }
        if identity.key() != UNKNOWN_KEY {
            for (key, entry) in &file.cursors {
                if key != UNKNOWN_KEY
                    && key != LEGACY_KEY
                    && same_ignoring_missing_guid(key, identity.key())
                {
                    return Some(entry.to_cursor(&file.hotel_id));
                }
            }
        }
        None
    }

    /// Writes `cursor` for `identity` (atomically: temp file, then rename). The cursor is
    /// remembered in memory first, so a write failure only costs a wider scan after a restart.
    pub fn save(&self, cursor: &Cursor, identity: &CursorIdentity) -> std::io::Result<()> {
        lock(&self.last_written).insert(identity.key().to_owned(), cursor.clone());
        let mut cursors = BTreeMap::new();
        if let Some(existing) = self.read(false)
            && existing.hotel_id.eq_ignore_ascii_case(&cursor.hotel_id)
        {
            if existing.v1.is_some() && identity.key() == UNKNOWN_KEY {
                return Ok(());
            }
            cursors = existing.cursors;
            if let Some(v1) = existing.v1.filter(|_| !identity.can_adopt_v1()) {
                cursors.insert(LEGACY_KEY.to_owned(), Entry::from(&v1));
            }
        }
        cursors.insert(identity.key().to_owned(), Entry::from(cursor));
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, render(&cursor.hotel_id, &cursors))?;
        std::fs::rename(&tmp, &self.path)
    }

    fn remembered(&self, hotel_id: &str, key: &str) -> Option<Cursor> {
        lock(&self.last_written)
            .get(key)
            .filter(|c| c.hotel_id.eq_ignore_ascii_case(hotel_id))
            .cloned()
    }

    fn log_v1_decision(&self, identity: &CursorIdentity) {
        {
            let mut logged = lock(&self.v1_logged);
            if *logged {
                return;
            }
            *logged = true;
        }
        if identity.can_adopt_v1() {
            log::info(
                "folio cursor is v1; adopted for the hotel's baseline database, rewritten as v2 next save",
                &[("identity", V::S(identity.key()))],
            );
        } else {
            log::warn(
                "folio cursor is v1 but this is not the baseline database; cold start, v1 kept as 'legacy'",
                &[("identity", V::S(identity.key()))],
            );
        }
    }

    fn read(&self, log_failure: bool) -> Option<Parsed> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                if log_failure {
                    log::error(
                        "folio cursor unreadable; cold start from the look-back window",
                        &[("error", V::S(&e.kind().to_string()))],
                    );
                }
                return None;
            }
        };
        match parse(&bytes) {
            Ok(p) => Some(p),
            Err(problem) => {
                if log_failure {
                    log::error(
                        "folio cursor unreadable; cold start from the look-back window",
                        &[("error", V::S(&problem))],
                    );
                }
                None
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn entry(v: &dl_json::Value) -> Result<Entry, String> {
    let watermark = datetime_prop(v, "watermark").ok_or("cursor without a watermark")?;
    let last_scan_at = str_prop(v, "lastScanAt")
        .and_then(|s| OffsetDateTime::parse_rfc3339(&s).ok())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    Ok(Entry {
        watermark,
        last_scan_at,
        last_batch_size: int_prop(v, "lastBatchSize").unwrap_or(0),
    })
}

fn parse(bytes: &[u8]) -> Result<Parsed, String> {
    let v = json(bytes).map_err(|e| e.0)?;
    let hotel_id = str_prop(&v, "hotelId").ok_or("cursor without a hotelId")?;
    match int_prop(&v, "version").unwrap_or(0) {
        1 => {
            let e = entry(&v)?;
            Ok(Parsed {
                v1: Some(e.to_cursor(&hotel_id)),
                hotel_id,
                cursors: BTreeMap::new(),
            })
        }
        CURRENT_VERSION => {
            let mut cursors = BTreeMap::new();
            if let Some(obj) = prop(&v, "cursors").and_then(dl_json::Value::as_object) {
                for (key, value) in obj.iter() {
                    if !value.is_null() {
                        cursors.insert(key.to_owned(), entry(value)?);
                    }
                }
            }
            Ok(Parsed {
                hotel_id,
                cursors,
                v1: None,
            })
        }
        other => Err(format!("unsupported cursor version {other}")),
    }
}

/// The v2 file, indented like `JsonSerializerOptions { WriteIndented = true }`.
fn render(hotel_id: &str, cursors: &BTreeMap<String, Entry>) -> String {
    let nl = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut s = String::new();
    let _ = write!(
        s,
        "{{{nl}  \"version\": {CURRENT_VERSION},{nl}  \"hotelId\": \""
    );
    escape_into(&mut s, hotel_id);
    let _ = write!(s, "\",{nl}  \"cursors\": {{");
    for (i, (key, e)) in cursors.iter().enumerate() {
        s.push_str(if i == 0 { "" } else { "," });
        let _ = write!(s, "{nl}    \"");
        escape_into(&mut s, key);
        let _ = write!(
            s,
            "\": {{{nl}      \"watermark\": \"{}\",{nl}      \"lastScanAt\": \"{}\",{nl}      \"lastBatchSize\": {}{nl}    }}",
            format_datetime(e.watermark),
            format_dto(e.last_scan_at),
            e.last_batch_size
        );
    }
    if !cursors.is_empty() {
        s.push_str(nl);
        s.push_str("  ");
    }
    let _ = write!(s, "}}{nl}}}");
    s
}

#[cfg(test)]
mod tests {
    use dl_time::Date;

    use super::*;

    fn at(day: u8) -> DateTime {
        Date::new(2026, 3, day).unwrap().midnight()
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dl-membership-cursor-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn id(current: &str, baseline: Option<&str>) -> CursorIdentity {
        CursorIdentity {
            current: Some(current.into()),
            baseline: baseline.map(Into::into),
        }
    }

    #[test]
    fn watermark_moves_forward_with_overlap() {
        let overlap = std::time::Duration::from_secs(900);
        assert_eq!(next_watermark(at(1), None, 0, 500, overlap), at(1));
        assert_eq!(next_watermark(at(1), Some(at(3)), 500, 500, overlap), at(3));
        let partial = next_watermark(at(1), Some(at(3)), 10, 500, overlap);
        assert_eq!(partial, at(3) - Duration::from_minutes(15));
        assert_eq!(
            next_watermark(at(3), Some(at(3)), 1, 500, overlap),
            at(3),
            "never backwards"
        );
        assert_eq!(cold_start(at(8), -7), at(1));
    }

    #[test]
    fn cursors_are_kept_per_database_and_v1_is_adopted_for_the_baseline() {
        let d = dir("v2");
        let store = CursorStore::new(&d);
        let c = Cursor {
            hotel_id: "H".into(),
            watermark: at(2),
            last_scan_at: OffsetDateTime::UNIX_EPOCH,
            last_batch_size: 3,
        };
        store.save(&c, &id("db-a", None)).unwrap();
        store
            .save(
                &Cursor {
                    watermark: at(5),
                    ..c.clone()
                },
                &id("db-b", None),
            )
            .unwrap();
        let fresh = CursorStore::new(&d);
        assert_eq!(fresh.load("h", &id("db-a", None)).unwrap().watermark, at(2));
        assert_eq!(fresh.load("H", &id("db-b", None)).unwrap().watermark, at(5));
        assert!(fresh.load("other", &id("db-a", None)).is_none());
        let text = std::fs::read_to_string(d.join(FILE_NAME)).unwrap();
        assert!(text.contains("\"version\": 2"), "{text}");

        std::fs::write(
            d.join(FILE_NAME),
            r#"{"version":1,"hotelId":"H","watermark":"2026-03-04T05:06:07.1234567","lastScanAt":"2026-03-04T00:00:00+00:00","lastBatchSize":9}"#,
        )
        .unwrap();
        let v1 = CursorStore::new(&d);
        assert_eq!(
            v1.load("H", &id("db-a", Some("db-a")))
                .unwrap()
                .last_batch_size,
            9
        );
        assert!(
            v1.load("H", &id("db-a", Some("db-z"))).is_none(),
            "not the baseline"
        );
        v1.save(&c, &id("db-a", Some("db-z"))).unwrap();
        let after = CursorStore::new(&d);
        assert!(
            after.load("H", &id("legacy", None)).is_some(),
            "v1 kept under 'legacy'"
        );
        std::fs::write(d.join(FILE_NAME), b"{not json").unwrap();
        assert!(CursorStore::new(&d).load("H", &id("db-a", None)).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
