//! Log files with rolling by day and/or size, and retention.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use dl_time::Date;

use crate::sink::{lock, with_line};
use crate::{Error, Format, Level, Record, Sink, TextFormat};

/// When a [`FileSink`] pushes data to the operating system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FlushPolicy {
    /// Every record is written with its own `write` call and no user-space buffer
    /// (the default). Nothing is lost if the process crashes, and with several processes
    /// appending to one file each line stays whole.
    #[default]
    EveryRecord,
    /// Records collect in a 64 KiB buffer that is written when full, when a record at
    /// `flush_level` or above arrives, when `interval` has passed since the last write
    /// to the file (checked when a record arrives), and on [`Sink::flush`],
    /// [`Logger::shutdown`](crate::Logger::shutdown) and drop. Several times faster for
    /// chatty logs; the newest records can be lost if the process is killed. Pair it with
    /// [`AsyncSink`](crate::AsyncSink) or call `flush` periodically if logs may stay
    /// idle.
    Buffered {
        /// Records at this level or above are flushed at once.
        flush_level: Level,
        /// Maximum age of buffered data when a new record arrives.
        interval: Duration,
    },
}

const BUFFER_CAPACITY: usize = 64 * 1024;

/// Appends lines to a log file, optionally rolling to a new file every day and/or when
/// the file reaches a size limit, and deleting the oldest files beyond a retention
/// count.
///
/// File names follow Serilog's `RollingInterval.Day` + `rollOnFileSizeLimit` scheme,
/// built from the configured path `dir/app.log`:
///
/// | Rolling | Files |
/// |---|---|
/// | none | `app.log` |
/// | size | `app.log`, `app_001.log`, `app_002.log`, … |
/// | daily | `app-20261115.log`, `app-20261116.log`, … |
/// | daily + size | `app-20261115.log`, `app-20261115_001.log`, …, `app-20261116.log` |
///
/// The day comes from each record's timestamp at the logger's offset, so with
/// `Offset::VIETNAM` files change at local midnight. Files are never renamed: the
/// newest file is the one being written, and after a restart the sink continues in the
/// newest file of the current day (or the next sequence number if that one is full). A
/// single record larger than the size limit is still written, to an otherwise empty file.
///
/// The sink is safe to share between threads: each line is formatted outside the lock
/// and written with one call under it. Write errors (disk full, directory deleted) are
/// returned to the logger, which reports them to its error hook; the next record
/// reopens the file.
///
/// ```no_run
/// use dl_log::{FileSink, FlushPolicy, JsonFormat, Level};
/// use std::time::Duration;
///
/// let file = FileSink::builder("/var/log/booking/app.log")
///     .daily(true)
///     .max_size(50 * 1024 * 1024)
///     .keep(30)
///     .flush_policy(FlushPolicy::Buffered { flush_level: Level::Warn, interval: Duration::from_secs(1) })
///     .format(JsonFormat::new())
///     .build()?;
/// # Ok::<(), dl_log::Error>(())
/// ```
///
/// Not available on `wasm32-unknown-unknown`.
pub struct FileSink {
    name: String,
    format: Box<dyn Format>,
    config: Config,
    state: Mutex<State>,
}

struct Config {
    dir: PathBuf,
    stem: String,
    /// Extension including the dot, or empty.
    ext: String,
    daily: bool,
    max_size: Option<u64>,
    keep: Option<usize>,
    flush: FlushPolicy,
}

struct State {
    out: Option<Output>,
    /// `yyyymmdd` of the open file when rolling daily, otherwise 0.
    period: u32,
    seq: u32,
    size: u64,
    path: Option<PathBuf>,
    last_flush: Instant,
}

enum Output {
    Direct(File),
    Buffered(BufWriter<File>),
}

impl Output {
    fn write_all(&mut self, line: &[u8]) -> io::Result<()> {
        match self {
            Output::Direct(f) => f.write_all(line),
            Output::Buffered(w) => w.write_all(line),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Output::Direct(f) => f.flush(),
            Output::Buffered(w) => w.flush(),
        }
    }
}

/// Builder for [`FileSink`], returned by [`FileSink::builder`].
pub struct FileSinkBuilder {
    path: PathBuf,
    daily: bool,
    max_size: Option<u64>,
    keep: Option<usize>,
    flush: FlushPolicy,
    format: Box<dyn Format>,
}

impl FileSinkBuilder {
    /// Starts a new file every day (default `false`).
    pub fn daily(mut self, yes: bool) -> FileSinkBuilder {
        self.daily = yes;
        self
    }

    /// Starts a new file before a write would make the current one larger than `bytes`.
    /// `0` turns size rolling off.
    pub fn max_size(mut self, bytes: u64) -> FileSinkBuilder {
        self.max_size = (bytes > 0).then_some(bytes);
        self
    }

    /// Keeps at most `files` log files (including the one being written), deleting the
    /// oldest after each roll. Default: keep everything. `0` is treated as `1`.
    pub fn keep(mut self, files: usize) -> FileSinkBuilder {
        self.keep = Some(files.max(1));
        self
    }

    /// Sets the [`FlushPolicy`] (default [`FlushPolicy::EveryRecord`]).
    pub fn flush_policy(mut self, policy: FlushPolicy) -> FileSinkBuilder {
        self.flush = policy;
        self
    }

    /// Uses `format` for lines (default [`TextFormat::new`]).
    pub fn format(mut self, format: impl Format + 'static) -> FileSinkBuilder {
        self.format = Box::new(format);
        self
    }

    /// Creates the directory if needed and, unless rolling daily, opens the file, so
    /// permission problems show up at startup. With daily rolling the file is opened by
    /// the first record, whose timestamp decides its name.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the path has no file name, the directory cannot be created or
    /// the file cannot be opened.
    pub fn build(self) -> Result<FileSink, Error> {
        let io_err = |context: String, source: io::Error| Error::Io { context, source };
        let file_name = self.path.file_name().ok_or_else(|| {
            io_err(
                format!("log file path {} has no file name", self.path.display()),
                io::Error::from(io::ErrorKind::InvalidInput),
            )
        })?;
        let file_path = Path::new(file_name);
        let stem = file_path
            .file_stem()
            .unwrap_or(file_name)
            .to_string_lossy()
            .into_owned();
        let ext = file_path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let dir = match self.path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        fs::create_dir_all(&dir)
            .map_err(|e| io_err(format!("create log directory {}", dir.display()), e))?;
        let sink = FileSink {
            name: format!("file:{}", self.path.display()),
            format: self.format,
            config: Config {
                dir,
                stem,
                ext,
                daily: self.daily,
                max_size: self.max_size,
                keep: self.keep,
                flush: self.flush,
            },
            state: Mutex::new(State {
                out: None,
                period: 0,
                seq: 0,
                size: 0,
                path: None,
                last_flush: Instant::now(),
            }),
        };
        if !sink.config.daily {
            let mut state = lock(&sink.state);
            sink.open(&mut state, 0)
                .map_err(|e| io_err(format!("open log file {}", self.path.display()), e))?;
            // Retention problems at startup are not fatal; they are retried on each roll.
            let _ = sink.apply_retention(&state);
        }
        Ok(sink)
    }
}

impl FileSink {
    /// A sink appending [`TextFormat`] lines to `path`, without rolling.
    ///
    /// # Errors
    ///
    /// See [`FileSinkBuilder::build`].
    pub fn new(path: impl Into<PathBuf>) -> Result<FileSink, Error> {
        FileSink::builder(path).build()
    }

    /// Starts configuring a sink for `path`, such as `logs/app.log`. Rolled file names
    /// are derived from its stem (`app`) and extension (`.log`).
    pub fn builder(path: impl Into<PathBuf>) -> FileSinkBuilder {
        FileSinkBuilder {
            path: path.into(),
            daily: false,
            max_size: None,
            keep: None,
            flush: FlushPolicy::EveryRecord,
            format: Box::new(TextFormat::new()),
        }
    }

    /// The file currently being written, if one is open.
    pub fn current_path(&self) -> Option<PathBuf> {
        lock(&self.state).path.clone()
    }

    fn file_name(&self, period: u32, seq: u32) -> String {
        let c = &self.config;
        let mut name = c.stem.clone();
        if c.daily {
            name.push_str(&format!("-{period:08}"));
        }
        if seq > 0 {
            name.push_str(&format!("_{seq:03}"));
        }
        name.push_str(&c.ext);
        name
    }

    /// Parses a file name produced by [`file_name`](Self::file_name).
    fn parse_name(&self, name: &str) -> Option<(u32, u32)> {
        let c = &self.config;
        let mut rest = name
            .strip_prefix(c.stem.as_str())?
            .strip_suffix(c.ext.as_str())?;
        let mut period = 0;
        if c.daily {
            let digits = rest.strip_prefix('-')?.get(..8)?;
            if !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            period = digits.parse().ok()?;
            rest = &rest[9..];
        }
        if rest.is_empty() {
            return Some((period, 0));
        }
        let seq = rest.strip_prefix('_')?;
        if seq.len() < 3 || !seq.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some((period, seq.parse().ok()?))
    }

    /// All managed log files, oldest first.
    fn list_files(&self) -> io::Result<Vec<((u32, u32), PathBuf)>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.config.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(key) = name.to_str().and_then(|n| self.parse_name(n))
                && entry.file_type().is_ok_and(|t| t.is_file())
            {
                files.push((key, entry.path()));
            }
        }
        files.sort();
        Ok(files)
    }

    /// Opens the newest file of `period` (or its first file).
    fn open(&self, state: &mut State, period: u32) -> io::Result<()> {
        let seq = self
            .list_files()?
            .into_iter()
            .filter(|((p, _), _)| *p == period)
            .map(|((_, s), _)| s)
            .max()
            .unwrap_or(0);
        self.open_seq(state, period, seq)
    }

    /// Opens `period`/`seq`, moving to later sequence numbers while files are full.
    fn open_seq(&self, state: &mut State, period: u32, mut seq: u32) -> io::Result<()> {
        self.close(state);
        loop {
            let path = self.config.dir.join(self.file_name(period, seq));
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            let size = file.metadata()?.len();
            let full = self.config.max_size.is_some_and(|max| size >= max);
            if full && seq < u32::MAX {
                seq += 1;
                continue;
            }
            state.out = Some(match self.config.flush {
                FlushPolicy::EveryRecord => Output::Direct(file),
                FlushPolicy::Buffered { .. } => {
                    Output::Buffered(BufWriter::with_capacity(BUFFER_CAPACITY, file))
                }
            });
            state.period = period;
            state.seq = seq;
            state.size = size;
            state.path = Some(path);
            state.last_flush = Instant::now();
            return Ok(());
        }
    }

    fn close(&self, state: &mut State) {
        if let Some(mut out) = state.out.take() {
            // A failed flush here loses buffered lines; the write that follows reports
            // any persistent problem.
            let _ = out.flush();
        }
    }

    fn apply_retention(&self, state: &State) -> io::Result<()> {
        let Some(keep) = self.config.keep else {
            return Ok(());
        };
        let files = self.list_files()?;
        let excess = files.len().saturating_sub(keep);
        let mut result = Ok(());
        for (_, path) in files.into_iter().take(excess) {
            if Some(&path) == state.path.as_ref() {
                continue;
            }
            if let Err(e) = fs::remove_file(&path)
                && e.kind() != io::ErrorKind::NotFound
                && result.is_ok()
            {
                result = Err(io::Error::new(
                    e.kind(),
                    format!("delete old log file {}: {e}", path.display()),
                ));
            }
        }
        result
    }

    fn write_line(&self, state: &mut State, record: &Record, line: &[u8]) -> io::Result<()> {
        let period = if self.config.daily {
            period_of(record.time.date())
        } else {
            0
        };
        let mut rolled = false;
        if state.out.is_none() {
            self.open(state, period.max(state.period))?;
            rolled = true;
        } else if period > state.period {
            self.open(state, period)?;
            rolled = true;
        }
        let len = line.len() as u64;
        if let Some(max) = self.config.max_size
            && state.size > 0
            && state.size.saturating_add(len) > max
        {
            let (period, next) = (state.period, state.seq.saturating_add(1));
            self.open_seq(state, period, next)?;
            rolled = true;
        }
        let Some(out) = state.out.as_mut() else {
            return Err(io::Error::other("log file is not open"));
        };
        if let Err(e) = out.write_all(line) {
            // Drop the handle so the next record reopens the file (it may have been
            // deleted or its volume remounted).
            state.out = None;
            return Err(e);
        }
        state.size += len;
        if let FlushPolicy::Buffered {
            flush_level,
            interval,
        } = self.config.flush
            && (record.level >= flush_level || state.last_flush.elapsed() >= interval)
        {
            state.last_flush = Instant::now();
            out.flush()?;
        }
        if rolled {
            self.apply_retention(state)
        } else {
            Ok(())
        }
    }
}

fn period_of(date: Date) -> u32 {
    let year = date.year().clamp(0, 9999) as u32;
    year * 10_000 + u32::from(date.month().number()) * 100 + u32::from(date.day())
}

impl Sink for FileSink {
    fn write(&self, record: &Record) -> io::Result<()> {
        with_line(&*self.format, record, |line| {
            let mut state = lock(&self.state);
            self.write_line(&mut state, record, line)
        })
    }

    fn flush(&self) -> io::Result<()> {
        let mut state = lock(&self.state);
        state.last_flush = Instant::now();
        match state.out.as_mut() {
            Some(out) => out.flush(),
            None => Ok(()),
        }
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        self.close(&mut state);
    }
}

impl fmt::Debug for FileSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSink")
            .field("name", &self.name)
            .field("daily", &self.config.daily)
            .field("max_size", &self.config.max_size)
            .field("keep", &self.config.keep)
            .field("flush", &self.config.flush)
            .finish()
    }
}

impl fmt::Debug for FileSinkBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSinkBuilder")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use dl_time::OffsetDateTime;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique, empty directory under the system temp dir, removed on drop.
    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> TempDir {
            static N: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "dl-log-{tag}-{}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed),
                OffsetDateTime::now_utc().unix_nanos()
            );
            let dir = std::env::temp_dir().join(unique);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub(crate) fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        pub(crate) fn read(&self, name: &str) -> String {
            fs::read_to_string(self.0.join(name)).unwrap()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn at(s: &str, msg: &str) -> Record {
        Record::new(
            OffsetDateTime::parse_rfc3339(s).unwrap(),
            Level::Info,
            "t",
            msg,
        )
    }

    fn msg_only(r: &Record, out: &mut String) {
        out.push_str(&r.message);
    }

    #[test]
    fn plain_file_appends_and_reopens() {
        let tmp = TempDir::new("plain");
        let path = tmp.0.join("nested").join("app.log");
        {
            let sink = FileSink::builder(&path).format(msg_only).build().unwrap();
            assert_eq!(sink.current_path().as_deref(), Some(path.as_path()));
            sink.write(&at("2026-11-15T10:00:00Z", "one")).unwrap();
        }
        let sink = FileSink::builder(&path).format(msg_only).build().unwrap();
        sink.write(&at("2026-11-15T10:00:00Z", "two")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        assert!(sink.name().starts_with("file:") && sink.name().ends_with("app.log"));
        assert!(format!("{sink:?}").contains("FileSink"));
    }

    #[test]
    fn rolls_by_size_with_serilog_names() {
        let tmp = TempDir::new("size");
        let sink = FileSink::builder(tmp.0.join("app.log"))
            .max_size(10)
            .format(msg_only)
            .build()
            .unwrap();
        for msg in ["aaaa", "bbbb", "cccc", "dddddddddddddddd", "e"] {
            sink.write(&at("2026-11-15T10:00:00Z", msg)).unwrap();
        }
        assert_eq!(
            tmp.names(),
            ["app.log", "app_001.log", "app_002.log", "app_003.log"]
        );
        assert_eq!(tmp.read("app.log"), "aaaa\nbbbb\n");
        assert_eq!(tmp.read("app_001.log"), "cccc\n");
        assert_eq!(tmp.read("app_002.log"), "dddddddddddddddd\n"); // oversized line still written
        assert_eq!(tmp.read("app_003.log"), "e\n");
        drop(sink);
        // A restart continues in the newest file.
        let sink = FileSink::builder(tmp.0.join("app.log"))
            .max_size(10)
            .format(msg_only)
            .build()
            .unwrap();
        sink.write(&at("2026-11-15T10:00:00Z", "f")).unwrap();
        assert_eq!(tmp.read("app_003.log"), "e\nf\n");
    }

    #[test]
    fn rolls_by_day_and_size() {
        let tmp = TempDir::new("daily");
        let sink = FileSink::builder(tmp.0.join("app.log"))
            .daily(true)
            .max_size(12)
            .format(msg_only)
            .build()
            .unwrap();
        assert_eq!(sink.current_path(), None); // opened by the first record
        sink.write(&at("2026-11-15T23:59:59.999+07:00", "day1-a"))
            .unwrap();
        sink.write(&at("2026-11-15T23:59:59.999+07:00", "day1-b"))
            .unwrap();
        sink.write(&at("2026-11-16T00:00:00+07:00", "day2-a"))
            .unwrap();
        // A late record from the previous day goes to the current file.
        sink.write(&at("2026-11-15T23:59:59+07:00", "late"))
            .unwrap();
        sink.write(&at("2026-12-01T08:00:00+07:00", "dec")).unwrap();
        assert_eq!(
            tmp.names(),
            [
                "app-20261115.log",
                "app-20261115_001.log",
                "app-20261116.log",
                "app-20261201.log"
            ]
        );
        assert_eq!(tmp.read("app-20261115.log"), "day1-a\n");
        assert_eq!(tmp.read("app-20261115_001.log"), "day1-b\n");
        assert_eq!(tmp.read("app-20261116.log"), "day2-a\nlate\n");
        assert_eq!(sink.current_path().unwrap(), tmp.0.join("app-20261201.log"));
    }

    #[test]
    fn retention_deletes_oldest() {
        let tmp = TempDir::new("keep");
        fs::write(tmp.0.join("app-20261101.log"), "old\n").unwrap();
        fs::write(tmp.0.join("app-20261102_001.log"), "old\n").unwrap();
        fs::write(tmp.0.join("other.log"), "not ours\n").unwrap();
        fs::write(tmp.0.join("app-2026110.log"), "not ours\n").unwrap();
        fs::write(tmp.0.join("app-20261103_01.log"), "not ours\n").unwrap();
        let sink = FileSink::builder(tmp.0.join("app.log"))
            .daily(true)
            .keep(3)
            .format(msg_only)
            .build()
            .unwrap();
        for day in 10..=14 {
            sink.write(&at(&format!("2026-11-{day}T09:00:00Z"), "x"))
                .unwrap();
        }
        assert_eq!(
            tmp.names(),
            [
                "app-2026110.log",
                "app-20261103_01.log",
                "app-20261112.log",
                "app-20261113.log",
                "app-20261114.log",
                "other.log"
            ]
        );
    }

    #[test]
    fn buffered_policy_flushes_on_level_and_explicitly() {
        let tmp = TempDir::new("buffered");
        let path = tmp.0.join("app.log");
        let sink = FileSink::builder(&path)
            .flush_policy(FlushPolicy::Buffered {
                flush_level: Level::Error,
                interval: Duration::from_secs(3600),
            })
            .format(msg_only)
            .build()
            .unwrap();
        sink.write(&at("2026-11-15T10:00:00Z", "buffered")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        let mut err = at("2026-11-15T10:00:00Z", "error");
        err.level = Level::Error;
        sink.write(&err).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "buffered\nerror\n");
        sink.write(&at("2026-11-15T10:00:00Z", "later")).unwrap();
        sink.flush().unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "buffered\nerror\nlater\n"
        );
        sink.write(&at("2026-11-15T10:00:00Z", "on drop")).unwrap();
        drop(sink);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .ends_with("later\non drop\n")
        );
    }

    #[test]
    fn names_round_trip() {
        let tmp = TempDir::new("names");
        let sink = FileSink::builder(tmp.0.join("app.log"))
            .daily(true)
            .build()
            .unwrap();
        for (period, seq) in [(20261115, 0), (20261115, 1), (99991231, 1234)] {
            let name = sink.file_name(period, seq);
            assert_eq!(sink.parse_name(&name), Some((period, seq)), "{name}");
        }
        assert_eq!(sink.file_name(20261115, 1), "app-20261115_001.log");
        for bad in [
            "app.log",
            "app-2026111.log",
            "app-20261115_1.log",
            "app-20261115-1.log",
            "app-2026111x.log",
            "app-20261115.txt",
        ] {
            assert_eq!(sink.parse_name(bad), None, "{bad}");
        }
        let plain = FileSink::builder(tmp.0.join("noext")).build().unwrap();
        assert_eq!(plain.file_name(0, 2), "noext_002");
        assert_eq!(plain.parse_name("noext_002"), Some((0, 2)));
        assert_eq!(plain.parse_name("noext"), Some((0, 0)));
    }

    #[test]
    fn bad_paths_are_errors_not_panics() {
        let tmp = TempDir::new("bad");
        // A directory where the file should be.
        fs::create_dir_all(tmp.0.join("app.log")).unwrap();
        let err = FileSink::new(tmp.0.join("app.log")).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "{err:?}");
        // A file where the directory should be.
        fs::write(tmp.0.join("file"), "x").unwrap();
        let err = FileSink::new(tmp.0.join("file").join("app.log")).unwrap_err();
        assert!(err.to_string().starts_with("create log directory"), "{err}");
        let err = FileSink::new("..").unwrap_err();
        assert!(err.to_string().contains("has no file name"), "{err}");
    }
}
