//! The `Sink` trait and the portable sinks: memory and any `io::Write`.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{Format, Record, TextFormat};

/// A destination for records: the console, a file, a network service, a test buffer.
///
/// Sinks are shared between threads, so methods take `&self`; use interior mutability
/// (a `Mutex`) for state. Return errors instead of panicking: the [`Logger`](crate::Logger)
/// passes them to its error hook as [`Error::Sink`](crate::Error::Sink) and carries on.
///
/// A sink that only needs to write lines can format with a [`Format`]:
///
/// ```
/// use std::io;
/// use std::sync::Mutex;
/// use dl_log::{Format, JsonFormat, Logger, Record, Sink, info};
///
/// /// Collects JSON lines into a batch, as an HTTP shipper would.
/// struct Batch {
///     format: JsonFormat,
///     lines: Mutex<Vec<String>>,
/// }
///
/// impl Sink for Batch {
///     fn write(&self, record: &Record) -> io::Result<()> {
///         let line = self.format.format_to_string(record);
///         self.lines.lock().map_err(|_| io::Error::other("poisoned"))?.push(line);
///         Ok(())
///     }
///
///     fn name(&self) -> &str {
///         "batch"
///     }
/// }
///
/// let batch = std::sync::Arc::new(Batch { format: JsonFormat::new(), lines: Mutex::new(Vec::new()) });
/// let log = Logger::builder().sink(batch.clone()).build();
/// info!(logger: log, "shipped"; count = 3);
/// assert!(batch.lines.lock().unwrap()[0].ends_with(r#""message":"shipped","count":3}"#));
/// ```
pub trait Sink: Send + Sync {
    /// Writes one record. Called concurrently from any thread.
    fn write(&self, record: &Record) -> io::Result<()>;

    /// Pushes buffered records to their destination. The default does nothing.
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    /// Flushes and releases resources such as background threads; called by
    /// [`Logger::shutdown`](crate::Logger::shutdown). The default calls
    /// [`flush`](Self::flush). Records written after shutdown should still be handled
    /// (synchronously) or dropped, never panic.
    fn shutdown(&self) -> io::Result<()> {
        self.flush()
    }

    /// A short name used in error reports, such as `console` or `file:logs/app.log`.
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
}

impl<S: Sink + ?Sized> Sink for Arc<S> {
    fn write(&self, record: &Record) -> io::Result<()> {
        (**self).write(record)
    }
    fn flush(&self) -> io::Result<()> {
        (**self).flush()
    }
    fn shutdown(&self) -> io::Result<()> {
        (**self).shutdown()
    }
    fn name(&self) -> &str {
        (**self).name()
    }
}

impl<S: Sink + ?Sized> Sink for Box<S> {
    fn write(&self, record: &Record) -> io::Result<()> {
        (**self).write(record)
    }
    fn flush(&self) -> io::Result<()> {
        (**self).flush()
    }
    fn shutdown(&self) -> io::Result<()> {
        (**self).shutdown()
    }
    fn name(&self) -> &str {
        (**self).name()
    }
}

/// Locks a mutex, recovering the data if another thread panicked while holding it:
/// logging must keep working after a panic elsewhere.
pub(crate) fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Formats `record` plus `\n` into a per-thread buffer and passes the bytes to `write`.
pub(crate) fn with_line(
    format: &dyn Format,
    record: &Record,
    mut write: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    thread_local! {
        static BUF: RefCell<String> = const { RefCell::new(String::new()) };
    }
    const KEEP_CAPACITY: usize = 64 * 1024;
    let reused = BUF.try_with(|cell| {
        let mut buf = cell.try_borrow_mut().ok()?;
        buf.clear();
        format.format(record, &mut buf);
        buf.push('\n');
        let result = write(buf.as_bytes());
        if buf.capacity() > KEEP_CAPACITY {
            *buf = String::new();
        }
        Some(result)
    });
    match reused {
        Ok(Some(result)) => result,
        // Re-entered (a format that logs) or the thread is exiting: use a fresh buffer.
        _ => {
            let mut line = String::with_capacity(128);
            format.format(record, &mut line);
            line.push('\n');
            write(line.as_bytes())
        }
    }
}

/// Keeps records and their formatted lines in memory, for tests and for showing recent
/// logs in a UI (it works on every platform, including the browser).
///
/// Clones share the same buffer, so keep one clone and give another to the logger:
///
/// ```
/// use dl_log::{Level, Logger, MemorySink, TextFormat, info, warn};
///
/// let mem = MemorySink::new().with_format(TextFormat::new().with_time(false));
/// let log = Logger::builder().sink(mem.clone()).build();
/// info!(logger: log, "started"; port = 8080);
/// warn!(logger: log, "cache miss rate {}%", 37);
///
/// assert_eq!(mem.lines(), ["INFO  started port=8080", "WARN  cache miss rate 37%"]);
/// assert_eq!(mem.records()[1].level, Level::Warn);
/// mem.clear();
/// assert!(mem.is_empty());
/// ```
#[derive(Clone)]
pub struct MemorySink {
    shared: Arc<Mutex<MemoryState>>,
}

struct MemoryState {
    entries: VecDeque<(Record, String)>,
    limit: Option<usize>,
    format: Box<dyn Format>,
}

impl Default for MemorySink {
    fn default() -> Self {
        MemorySink::new()
    }
}

impl MemorySink {
    /// An unbounded sink that formats lines with [`TextFormat::new`].
    pub fn new() -> MemorySink {
        MemorySink {
            shared: Arc::new(Mutex::new(MemoryState {
                entries: VecDeque::new(),
                limit: None,
                format: Box::new(TextFormat::new()),
            })),
        }
    }

    /// Formats lines with `format` (for this sink and all its clones).
    pub fn with_format(self, format: impl Format + 'static) -> MemorySink {
        lock(&self.shared).format = Box::new(format);
        self
    }

    /// Keeps only the newest `limit` records (a ring buffer); `limit` 0 keeps nothing.
    pub fn with_limit(self, limit: usize) -> MemorySink {
        {
            let mut state = lock(&self.shared);
            state.limit = Some(limit);
            let excess = state.entries.len().saturating_sub(limit);
            state.entries.drain(..excess);
        }
        self
    }

    /// Copies of the stored records, oldest first.
    pub fn records(&self) -> Vec<Record> {
        lock(&self.shared)
            .entries
            .iter()
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// The formatted lines (without newlines), oldest first.
    pub fn lines(&self) -> Vec<String> {
        lock(&self.shared)
            .entries
            .iter()
            .map(|(_, l)| l.clone())
            .collect()
    }

    /// All lines, each followed by `\n`, as a file sink would write them.
    pub fn contents(&self) -> String {
        let state = lock(&self.shared);
        let mut out = String::new();
        for (_, line) in &state.entries {
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Number of stored records.
    pub fn len(&self) -> usize {
        lock(&self.shared).entries.len()
    }

    /// Returns `true` when nothing is stored.
    pub fn is_empty(&self) -> bool {
        lock(&self.shared).entries.is_empty()
    }

    /// Removes everything.
    pub fn clear(&self) {
        lock(&self.shared).entries.clear();
    }
}

impl Sink for MemorySink {
    fn write(&self, record: &Record) -> io::Result<()> {
        let mut state = lock(&self.shared);
        if state.limit == Some(0) {
            return Ok(());
        }
        let mut line = String::new();
        state.format.format(record, &mut line);
        if state
            .limit
            .is_some_and(|limit| state.entries.len() >= limit)
        {
            state.entries.pop_front();
        }
        state.entries.push_back((record.clone(), line));
        Ok(())
    }

    fn name(&self) -> &str {
        "memory"
    }
}

impl fmt::Debug for MemorySink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemorySink")
            .field("len", &self.len())
            .finish()
    }
}

/// Writes formatted lines to any [`io::Write`]: a `TcpStream`, a pipe, a `Vec<u8>`,
/// `io::sink()`. Each line is written with one `write_all` call under a lock.
///
/// ```
/// use dl_log::{JsonFormat, Logger, WriterSink, error};
/// use std::sync::Arc;
///
/// let sink = Arc::new(WriterSink::new(Vec::<u8>::new()).with_format(JsonFormat::new().with_target(false)));
/// let log = Logger::builder()
///     .clock(|| dl_time::OffsetDateTime::UNIX_EPOCH)
///     .sink(sink.clone())
///     .build();
/// error!(logger: log, "payment declined"; code = "51");
/// let written = sink.with_writer(|buf| String::from_utf8_lossy(buf).into_owned());
/// assert_eq!(
///     written,
///     "{\"time\":\"1970-01-01T00:00:00.000Z\",\"level\":\"ERROR\",\"message\":\"payment declined\",\"code\":\"51\"}\n"
/// );
/// ```
pub struct WriterSink<W> {
    writer: Mutex<W>,
    format: Box<dyn Format>,
    name: String,
}

impl<W: Write + Send> WriterSink<W> {
    /// Writes [`TextFormat::new`] lines to `writer`.
    pub fn new(writer: W) -> WriterSink<W> {
        WriterSink {
            writer: Mutex::new(writer),
            format: Box::new(TextFormat::new()),
            name: "writer".into(),
        }
    }

    /// Uses `format` for lines.
    pub fn with_format(mut self, format: impl Format + 'static) -> WriterSink<W> {
        self.format = Box::new(format);
        self
    }

    /// Sets the name used in error reports (default `writer`).
    pub fn with_name(mut self, name: impl Into<String>) -> WriterSink<W> {
        self.name = name.into();
        self
    }

    /// Runs `f` with the writer locked, for example to read a `Vec<u8>` back in tests.
    pub fn with_writer<R>(&self, f: impl FnOnce(&mut W) -> R) -> R {
        f(&mut lock(&self.writer))
    }

    /// Returns the writer.
    pub fn into_inner(self) -> W {
        self.writer.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

impl<W: Write + Send> Sink for WriterSink<W> {
    fn write(&self, record: &Record) -> io::Result<()> {
        with_line(&*self.format, record, |line| {
            lock(&self.writer).write_all(line)
        })
    }

    fn flush(&self) -> io::Result<()> {
        lock(&self.writer).flush()
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl<W> fmt::Debug for WriterSink<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriterSink")
            .field("name", &self.name)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Level;
    use dl_time::OffsetDateTime;

    fn rec(msg: &str) -> Record {
        Record::new(OffsetDateTime::UNIX_EPOCH, Level::Info, "t", msg)
    }

    #[test]
    fn memory_limit_keeps_newest() {
        let mem =
            MemorySink::new().with_format(|r: &Record, out: &mut String| out.push_str(&r.message));
        for i in 0..5 {
            mem.write(&rec(&i.to_string())).unwrap();
        }
        let mem = mem.with_limit(3);
        assert_eq!(mem.lines(), ["2", "3", "4"]);
        mem.write(&rec("5")).unwrap();
        assert_eq!(mem.lines(), ["3", "4", "5"]);
        assert_eq!(mem.contents(), "3\n4\n5\n");
        assert_eq!(mem.len(), 3);
        let none = MemorySink::new().with_limit(0);
        none.write(&rec("x")).unwrap();
        assert!(none.is_empty());
        assert_eq!(format!("{mem:?}"), "MemorySink { len: 3 }");
        assert_eq!(mem.name(), "memory");
    }

    #[test]
    fn writer_sink_writes_whole_lines() {
        let sink = WriterSink::new(Vec::new())
            .with_format(|r: &Record, out: &mut String| out.push_str(&r.message))
            .with_name("buf");
        sink.write(&rec("a")).unwrap();
        sink.write(&rec("b")).unwrap();
        sink.flush().unwrap();
        assert_eq!(sink.name(), "buf");
        assert_eq!(sink.into_inner(), b"a\nb\n");
    }

    #[test]
    fn line_buffer_survives_reentrancy() {
        // A format that formats another record through the same thread-local buffer.
        struct Nested;
        impl Format for Nested {
            fn format(&self, record: &Record, out: &mut String) {
                out.push_str(&record.message);
                if record.message == "outer" {
                    let inner = rec("inner");
                    let _ = with_line(
                        &|r: &Record, o: &mut String| o.push_str(&r.message),
                        &inner,
                        |l| {
                            out.push_str(&String::from_utf8_lossy(l));
                            Ok(())
                        },
                    );
                }
            }
        }
        let mut got = Vec::new();
        with_line(&Nested, &rec("outer"), |l| {
            got.extend_from_slice(l);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, b"outerinner\n\n");
    }

    #[test]
    fn default_name_is_the_type_name() {
        struct Custom;
        impl Sink for Custom {
            fn write(&self, _: &Record) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(Custom.name().ends_with("Custom"));
        assert!(Custom.flush().is_ok() && Custom.shutdown().is_ok());
        let boxed: Box<dyn Sink> = Box::new(Custom);
        assert!(boxed.name().ends_with("Custom"));
        assert!(
            boxed.write(&rec("x")).is_ok() && boxed.flush().is_ok() && boxed.shutdown().is_ok()
        );
    }
}
