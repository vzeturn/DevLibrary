//! The rolling log file (the part of `Serilog.Sinks.File` the agent configures in
//! `AgentLogging.cs`): `logs/<prefix>.log` becomes `<prefix>yyyyMMdd.log` for the local
//! day, then `<prefix>yyyyMMdd_001.log`, `_002`, ... once a file reaches the size limit;
//! the 14 newest files are kept. The file is opened in shared append mode, one
//! [`format_line`](super::format::format_line) per write.
//!
//! As in Serilog, the file is opened at the first event of a period; when it cannot be
//! opened, events are dropped until the next period (the next local midnight).

use super::format::format_line;
use super::log::{Event, Sink};
use crate::config::time::{SharedClock, SystemClock, Timestamp, local_offset_minutes};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

const NANOS_PER_DAY: i128 = 86_400 * 1_000_000_000;

/// Where the local wall clock is read from (tests inject a fixed offset).
pub type LocalNow = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// A daily, size-limited, retained log file (`WriteTo.File(..., rollingInterval: Day,
/// retainedFileCountLimit, fileSizeLimitBytes, rollOnFileSizeLimit: true, shared: true)`).
pub struct RollingFileSink {
    dir: PathBuf,
    prefix: String,
    suffix: String,
    size_limit: u64,
    retained: usize,
    now: LocalNow,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    file: Option<File>,
    sequence: Option<u32>,
    /// Local wall-clock nanoseconds of the next period start.
    next_checkpoint: Option<i128>,
}

impl std::fmt::Debug for RollingFileSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RollingFileSink")
            .field("dir", &self.dir)
            .field("prefix", &self.prefix)
            .field("size_limit", &self.size_limit)
            .field("retained", &self.retained)
            .finish_non_exhaustive()
    }
}

/// A rolled file name parsed back: its local day (`yyyyMMdd`) and sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rolled {
    name: String,
    date: u32,
    sequence: Option<u32>,
}

impl RollingFileSink {
    /// A sink for `path` (`logs/agent-.log`): files `agent-yyyyMMdd[_nnn].log` beside it.
    pub fn new(path: &Path, size_limit: u64, retained: usize) -> RollingFileSink {
        let clock: SharedClock = SystemClock::shared();
        let now: LocalNow = Arc::new(move || clock.now().to_offset(local_offset_minutes()));
        RollingFileSink::with_clock(path, size_limit, retained, now)
    }

    /// [`RollingFileSink::new`] with an injected local clock.
    pub fn with_clock(
        path: &Path,
        size_limit: u64,
        retained: usize,
        now: LocalNow,
    ) -> RollingFileSink {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (prefix, suffix) = match file.rfind('.') {
            Some(i) => (file[..i].to_owned(), file[i..].to_owned()),
            None => (file, String::new()),
        };
        RollingFileSink {
            dir,
            prefix,
            suffix,
            size_limit,
            retained,
            now,
            state: Mutex::new(State::default()),
        }
    }

    /// The file name of a period and sequence.
    fn file_name(&self, date: u32, sequence: Option<u32>) -> String {
        match sequence {
            Some(s) => format!("{}{date:08}_{s:03}{}", self.prefix, self.suffix),
            None => format!("{}{date:08}{}", self.prefix, self.suffix),
        }
    }

    /// `^prefix(\d{8})(_[0-9]{3,})?suffix$` with a valid date.
    fn parse_name(&self, name: &str) -> Option<Rolled> {
        let rest = name.strip_prefix(self.prefix.as_str())?;
        let rest = rest.strip_suffix(self.suffix.as_str())?;
        let date_text = rest.get(..8)?;
        if !date_text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let date: u32 = date_text.parse().ok()?;
        let (y, m, d) = (date / 10_000, (date / 100) % 100, date % 100);
        if y == 0 || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
            return None;
        }
        let tail = &rest[8..];
        let sequence = if tail.is_empty() {
            None
        } else {
            let digits = tail.strip_prefix('_')?;
            if digits.len() < 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(digits.parse().ok()?)
        };
        Some(Rolled {
            name: name.to_owned(),
            date,
            sequence,
        })
    }

    fn existing(&self) -> Vec<Rolled> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|n| self.parse_name(&n))
            .collect()
    }

    fn open(&self, st: &mut State, wall: i128, min_sequence: Option<u32>) {
        let date = date_number(wall);
        st.next_checkpoint = Some((wall.div_euclid(NANOS_PER_DAY) + 1) * NANOS_PER_DAY);
        let mut sequence = self
            .existing()
            .into_iter()
            .filter(|r| r.date == date)
            .map(|r| r.sequence)
            .max_by(|a, b| a.cmp(b))
            .flatten();
        if let Some(min) = min_sequence
            && sequence.is_none_or(|s| s < min)
        {
            sequence = Some(min);
        }
        for _ in 0..3 {
            let path = self.dir.join(self.file_name(date, sequence));
            if std::fs::create_dir_all(&self.dir).is_err() {
                return;
            }
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(f) => {
                    st.file = Some(f);
                    st.sequence = sequence;
                    self.apply_retention(&path);
                    return;
                }
                Err(e) if matches!(e.raw_os_error(), Some(32 | 33)) && cfg!(windows) => {
                    sequence = Some(sequence.unwrap_or(0) + 1);
                }
                Err(_) => return,
            }
        }
    }

    fn apply_retention(&self, current: &Path) {
        if self.retained == 0 {
            return;
        }
        let current_name = current
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut files = self.existing();
        if !files.iter().any(|f| f.name == current_name)
            && let Some(r) = self.parse_name(&current_name)
        {
            files.push(r);
        }
        files.sort_by(|a, b| b.date.cmp(&a.date).then(b.sequence.cmp(&a.sequence)));
        let others = files
            .into_iter()
            .filter(|f| !f.name.eq_ignore_ascii_case(&current_name));
        for (i, f) in others.enumerate() {
            if i + 1 >= self.retained {
                let _ = std::fs::remove_file(self.dir.join(&f.name));
            }
        }
    }

    fn align(&self, st: &mut State, wall: i128, next_sequence: bool) {
        match st.next_checkpoint {
            None => self.open(st, wall, None),
            Some(next) if next_sequence || wall >= next => {
                let min = next_sequence.then(|| st.sequence.map_or(1, |s| s + 1));
                st.file = None;
                self.open(st, wall, min);
            }
            Some(_) => {}
        }
    }
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `yyyyMMdd` of a local wall-clock instant.
fn date_number(wall: i128) -> u32 {
    let (y, m, d, ..) = crate::config::time::civil_parts(wall);
    u32::try_from(y).unwrap_or(0) * 10_000 + m * 100 + d
}

impl Sink for RollingFileSink {
    fn emit(&self, event: &Event) {
        let line = format_line(event);
        let wall = (self.now)().wall_nanos();
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.align(&mut st, wall, false);
        loop {
            let Some(file) = st.file.as_mut() else { return };
            let full = file
                .metadata()
                .map(|m| m.len() >= self.size_limit)
                .unwrap_or(false);
            if !full {
                let _ = file.write_all(line.as_bytes());
                return;
            }
            self.align(&mut st, wall, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporting::log::Level;

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("pda-roll-{}", dl_uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn rolls_daily_and_by_size_and_keeps_the_newest() {
        let dir = temp_dir();
        let now = Arc::new(Mutex::new(
            Timestamp::parse_iso("2026-11-04T23:59:00+07:00").unwrap(),
        ));
        let n = now.clone();
        let sink = RollingFileSink::with_clock(
            &dir.join("agent-.log"),
            200,
            3,
            Arc::new(move || *n.lock().unwrap()),
        );
        let e = Event::new(Timestamp::from_unix_seconds(0), Level::Information, "hello");
        sink.emit(&e);
        assert_eq!(names(&dir), ["agent-20261104.log"]);
        // The line is the formatter's line.
        assert_eq!(
            std::fs::read_to_string(dir.join("agent-20261104.log")).unwrap(),
            "{\"ts\":\"1970-01-01T00:00:00.000Z\",\"level\":\"info\",\"msg\":\"hello\"}\n"
        );
        for _ in 0..5 {
            sink.emit(&e);
        }
        assert_eq!(
            names(&dir),
            ["agent-20261104.log", "agent-20261104_001.log"]
        );
        *now.lock().unwrap() = Timestamp::parse_iso("2026-11-05T00:00:01+07:00").unwrap();
        sink.emit(&e);
        // Retention 3: the current file and the two newest others.
        assert_eq!(
            names(&dir),
            [
                "agent-20261104.log",
                "agent-20261104_001.log",
                "agent-20261105.log"
            ]
        );
        for _ in 0..8 {
            sink.emit(&e);
        }
        assert_eq!(
            names(&dir),
            [
                "agent-20261105.log",
                "agent-20261105_001.log",
                "agent-20261105_002.log"
            ]
        );
        // A new sink (restart) appends to the latest file of the day.
        let n2 = now.clone();
        let again = RollingFileSink::with_clock(
            &dir.join("agent-.log"),
            200,
            3,
            Arc::new(move || *n2.lock().unwrap()),
        );
        again.emit(&e);
        assert_eq!(
            std::fs::read_to_string(dir.join("agent-20261105_002.log"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_parse_like_serilog() {
        let s = RollingFileSink::new(Path::new("logs/tools-.log"), 1, 1);
        assert_eq!(s.parse_name("tools-20261104.log").unwrap().sequence, None);
        assert_eq!(
            s.parse_name("tools-20261104_1234.log").unwrap().sequence,
            Some(1234)
        );
        for bad in [
            "tools-20261304.log",
            "tools-2026110.log",
            "tools-20261104_01.log",
            "agent-20261104.log",
            "tools-20261104.txt",
        ] {
            assert!(s.parse_name(bad).is_none(), "{bad}");
        }
    }
}
