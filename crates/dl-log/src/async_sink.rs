//! A sink wrapper that writes on a background thread.

use std::fmt;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::sink::lock;
use crate::{Error, Record, Sink};

/// What [`AsyncSink`] does when its queue is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Overflow {
    /// The logging thread waits for room (the default). No record is lost, but a slow
    /// destination slows the application down.
    #[default]
    Block,
    /// The new record is discarded and counted in [`AsyncSink::dropped`]. Logging never
    /// waits; use it when latency matters more than completeness.
    DropNewest,
}

/// Options for [`AsyncSink::with_options`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AsyncOptions {
    /// Maximum number of queued records (at least 1). Default 8192.
    pub capacity: usize,
    /// Behaviour when the queue is full. Default [`Overflow::Block`].
    pub overflow: Overflow,
}

impl Default for AsyncOptions {
    fn default() -> Self {
        AsyncOptions {
            capacity: 8192,
            overflow: Overflow::Block,
        }
    }
}

enum Msg {
    Record(Record),
    Flush(SyncSender<io::Result<()>>),
    Shutdown(SyncSender<io::Result<()>>),
}

struct Shared {
    inner: Box<dyn Sink>,
    dropped: AtomicU64,
    /// First background error not yet reported, and how many more happened since.
    pending: Mutex<Option<(io::Error, u64)>>,
}

impl Shared {
    fn note_error(&self, e: io::Error) {
        let mut pending = lock(&self.pending);
        match pending.as_mut() {
            Some((_, more)) => *more += 1,
            None => *pending = Some((e, 0)),
        }
    }

    fn take_pending(&self) -> io::Result<()> {
        match lock(&self.pending).take() {
            None => Ok(()),
            Some((e, 0)) => Err(e),
            Some((e, more)) => Err(io::Error::new(
                e.kind(),
                format!("{e} (and {more} more errors)"),
            )),
        }
    }
}

fn guarded(f: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(io::Error::other("log sink panicked")))
}

/// Moves writing to a background thread: the logging call only clones the record into
/// a bounded queue, and a worker thread writes it to the wrapped sink.
///
/// - Records are written in the order they were queued.
/// - When the queue is full, [`Overflow`] decides between waiting and dropping (dropped
///   records are counted, see [`dropped`](Self::dropped)).
/// - [`flush`](Sink::flush) waits until everything queued before it has been written
///   and the wrapped sink has been flushed.
/// - [`shutdown`](Sink::shutdown) (called by [`Logger::shutdown`](crate::Logger::shutdown))
///   and drop write the remaining queue, shut the wrapped sink down and join the
///   thread. Records arriving afterwards are written synchronously, so none are lost.
/// - Errors from the wrapped sink happen on the worker; they are returned by the next
///   `write` or `flush` call and so reach the logger's error hook.
/// - A panic inside the wrapped sink is caught and reported as an error.
///
/// ```
/// use dl_log::{AsyncOptions, AsyncSink, Logger, MemorySink, Overflow, info};
/// use std::sync::Arc;
///
/// let mem = MemorySink::new();
/// let sink = Arc::new(AsyncSink::with_options(
///     mem.clone(),
///     AsyncOptions { capacity: 1024, overflow: Overflow::DropNewest },
/// )?);
/// let log = Logger::builder().sink(sink.clone()).build();
/// for i in 0..100 {
///     info!(logger: log, "event {}", i);
/// }
/// log.flush();
/// assert_eq!(mem.len() as u64 + sink.dropped(), 100);
/// log.shutdown();
/// # Ok::<(), dl_log::Error>(())
/// ```
///
/// Not available on `wasm32-unknown-unknown`, which has no threads.
pub struct AsyncSink {
    /// `None` after shutdown. Held while sending so shutdown cannot overtake a send.
    tx: Mutex<Option<SyncSender<Msg>>>,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
    overflow: Overflow,
    name: String,
}

impl AsyncSink {
    /// Wraps `inner` with the default [`AsyncOptions`] (8192 records, blocking).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the worker thread cannot be started.
    pub fn new(inner: impl Sink + 'static) -> Result<AsyncSink, Error> {
        AsyncSink::with_options(inner, AsyncOptions::default())
    }

    /// Wraps `inner` with the given queue capacity and overflow policy.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the worker thread cannot be started.
    pub fn with_options(
        inner: impl Sink + 'static,
        options: AsyncOptions,
    ) -> Result<AsyncSink, Error> {
        let name = format!("async:{}", inner.name());
        let shared = Arc::new(Shared {
            inner: Box::new(inner),
            dropped: AtomicU64::new(0),
            pending: Mutex::new(None),
        });
        let (tx, rx) = mpsc::sync_channel(options.capacity.max(1));
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("dl-log-async".into())
            .spawn(move || worker(&worker_shared, &rx))
            .map_err(|source| Error::Io {
                context: "start async log worker".into(),
                source,
            })?;
        Ok(AsyncSink {
            tx: Mutex::new(Some(tx)),
            shared,
            worker: Mutex::new(Some(handle)),
            overflow: options.overflow,
            name,
        })
    }

    /// Number of records discarded because the queue was full ([`Overflow::DropNewest`]).
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    fn stop(&self) -> io::Result<()> {
        // Holding the sender lock until the worker is gone makes concurrent writers wait
        // and then write synchronously, after everything queued before them.
        let mut tx = lock(&self.tx);
        let Some(sender) = tx.take() else {
            return Ok(());
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let result = match sender.send(Msg::Shutdown(ack_tx)) {
            Ok(()) => ack_rx
                .recv()
                .unwrap_or_else(|_| Err(io::Error::other("async log worker stopped"))),
            Err(_) => guarded(|| self.shared.inner.shutdown()),
        };
        drop(sender);
        if let Some(handle) = lock(&self.worker).take() {
            let _ = handle.join();
        }
        drop(tx);
        self.shared.take_pending().and(result)
    }
}

fn worker(shared: &Shared, rx: &Receiver<Msg>) {
    // Records logged from this thread (by the wrapped sink) would feed back into the
    // queue it is draining and could deadlock; they are discarded instead.
    crate::logger::mark_sink_thread();
    let handle = |msg: Msg| -> bool {
        match msg {
            Msg::Record(record) => {
                if let Err(e) = guarded(|| shared.inner.write(&record)) {
                    shared.note_error(e);
                }
                true
            }
            Msg::Flush(ack) => {
                let _ = ack.send(guarded(|| shared.inner.flush()));
                true
            }
            Msg::Shutdown(ack) => {
                let _ = ack.send(guarded(|| shared.inner.shutdown()));
                false
            }
        }
    };
    while let Ok(msg) = rx.recv() {
        if !handle(msg) {
            break;
        }
    }
}

impl Sink for AsyncSink {
    fn write(&self, record: &Record) -> io::Result<()> {
        let tx = lock(&self.tx);
        let Some(sender) = tx.as_ref() else {
            drop(tx);
            return guarded(|| self.shared.inner.write(record));
        };
        let msg = Msg::Record(record.clone());
        let unsent = match self.overflow {
            Overflow::Block => sender.send(msg).err().map(|e| e.0),
            Overflow::DropNewest => match sender.try_send(msg) {
                Ok(()) => None,
                Err(TrySendError::Full(_)) => {
                    self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    None
                }
                Err(TrySendError::Disconnected(msg)) => Some(msg),
            },
        };
        drop(tx);
        if let Some(Msg::Record(record)) = unsent {
            // The worker died: write on this thread rather than lose the record.
            guarded(|| self.shared.inner.write(&record))?;
        }
        self.shared.take_pending()
    }

    fn flush(&self) -> io::Result<()> {
        let tx = lock(&self.tx);
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let sent = tx
            .as_ref()
            .is_some_and(|s| s.send(Msg::Flush(ack_tx)).is_ok());
        drop(tx);
        let result = if sent {
            ack_rx
                .recv()
                .unwrap_or_else(|_| Err(io::Error::other("async log worker stopped")))
        } else {
            guarded(|| self.shared.inner.flush())
        };
        self.shared.take_pending().and(result)
    }

    fn shutdown(&self) -> io::Result<()> {
        self.stop()
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for AsyncSink {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl fmt::Debug for AsyncSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncSink")
            .field("name", &self.name)
            .field("overflow", &self.overflow)
            .field("dropped", &self.dropped())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, MemorySink};
    use dl_time::OffsetDateTime;
    use std::sync::Condvar;
    use std::sync::atomic::AtomicBool;

    fn rec(msg: String) -> Record {
        Record::new(OffsetDateTime::UNIX_EPOCH, Level::Info, "t", msg)
    }

    fn msg_only(r: &Record, out: &mut String) {
        out.push_str(&r.message);
    }

    /// A sink that blocks every write until released.
    struct Gate {
        open: Mutex<bool>,
        cv: Condvar,
        entered: AtomicBool,
        mem: MemorySink,
    }

    impl Gate {
        fn release(&self) {
            *lock(&self.open) = true;
            self.cv.notify_all();
        }
    }

    impl Sink for Gate {
        fn write(&self, record: &Record) -> io::Result<()> {
            self.entered.store(true, Ordering::SeqCst);
            let mut open = lock(&self.open);
            while !*open {
                open = self.cv.wait(open).unwrap_or_else(|e| e.into_inner());
            }
            self.mem.write(record)
        }
    }

    #[test]
    fn preserves_order_and_flushes() {
        let mem = MemorySink::new().with_format(msg_only);
        let sink = AsyncSink::new(mem.clone()).unwrap();
        for i in 0..1000 {
            sink.write(&rec(i.to_string())).unwrap();
        }
        sink.flush().unwrap();
        let expected: Vec<String> = (0..1000).map(|i| i.to_string()).collect();
        assert_eq!(mem.lines(), expected);
        assert_eq!(sink.dropped(), 0);
        assert_eq!(sink.name(), "async:memory");
    }

    #[test]
    fn drop_newest_counts_drops() {
        let gate = Arc::new(Gate {
            open: Mutex::new(false),
            cv: Condvar::new(),
            entered: AtomicBool::new(false),
            mem: MemorySink::new().with_format(msg_only),
        });
        let sink = AsyncSink::with_options(
            gate.clone(),
            AsyncOptions {
                capacity: 4,
                overflow: Overflow::DropNewest,
            },
        )
        .unwrap();
        // The worker takes the first record and blocks in the gate; 4 more fill the
        // queue; the remaining 5 are dropped.
        sink.write(&rec("0".into())).unwrap();
        let start = std::time::Instant::now();
        while !gate.entered.load(Ordering::SeqCst) {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "worker did not start"
            );
            thread::yield_now();
        }
        for i in 1..10 {
            sink.write(&rec(i.to_string())).unwrap();
        }
        assert_eq!(sink.dropped(), 5);
        gate.release();
        sink.flush().unwrap();
        assert_eq!(gate.mem.lines(), ["0", "1", "2", "3", "4"]);
        assert!(format!("{sink:?}").contains("dropped: 5"));
    }

    #[test]
    fn shutdown_drains_and_later_writes_are_synchronous() {
        let mem = MemorySink::new().with_format(msg_only);
        let sink = AsyncSink::with_options(
            mem.clone(),
            AsyncOptions {
                capacity: 2,
                overflow: Overflow::Block,
            },
        )
        .unwrap();
        for i in 0..50 {
            sink.write(&rec(i.to_string())).unwrap();
        }
        sink.shutdown().unwrap();
        assert_eq!(mem.len(), 50);
        sink.write(&rec("after".into())).unwrap();
        assert_eq!(mem.lines().last().map(String::as_str), Some("after"));
        sink.flush().unwrap();
        sink.shutdown().unwrap(); // idempotent
    }

    #[test]
    fn drop_flushes_queue() {
        let mem = MemorySink::new().with_format(msg_only);
        {
            let sink = AsyncSink::new(mem.clone()).unwrap();
            for i in 0..200 {
                sink.write(&rec(i.to_string())).unwrap();
            }
        }
        assert_eq!(mem.len(), 200);
    }

    #[test]
    fn worker_errors_and_panics_are_reported_later() {
        struct Failing {
            calls: AtomicU64,
            panicked: AtomicBool,
        }
        impl Sink for Failing {
            fn write(&self, record: &Record) -> io::Result<()> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if record.message == "panic" {
                    self.panicked.store(true, Ordering::Relaxed);
                    panic!("sink bug");
                }
                Err(io::Error::other("disk full"))
            }
        }
        let failing = Arc::new(Failing {
            calls: AtomicU64::new(0),
            panicked: AtomicBool::new(false),
        });
        let sink = AsyncSink::new(failing.clone()).unwrap();
        // Errors happen on the worker and come back from a later write or flush.
        let results = [
            sink.write(&rec("a".into())),
            sink.write(&rec("b".into())),
            sink.write(&rec("panic".into())),
            sink.flush(),
        ];
        let errors: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.iter().any(|e| e.starts_with("disk full")),
            "{errors:?}"
        );
        assert!(sink.flush().is_ok(), "errors are reported once");
        assert!(failing.panicked.load(Ordering::Relaxed));
        assert_eq!(failing.calls.load(Ordering::Relaxed), 3);
        // The worker survived the panic.
        let write = sink.write(&rec("c".into()));
        let flush = sink.flush();
        let err = write.err().or(flush.err()).expect("error from c");
        assert!(err.to_string().contains("disk full"));
    }

    #[test]
    fn many_threads_keep_per_thread_order() {
        let mem = MemorySink::new().with_format(msg_only);
        let sink = Arc::new(
            AsyncSink::with_options(
                mem.clone(),
                AsyncOptions {
                    capacity: 16,
                    overflow: Overflow::Block,
                },
            )
            .unwrap(),
        );
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let sink = sink.clone();
                thread::spawn(move || {
                    for i in 0..500 {
                        sink.write(&rec(format!("{t}:{i}"))).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        sink.flush().unwrap();
        let lines = mem.lines();
        assert_eq!(lines.len(), 4000);
        let mut next = [0u32; 8];
        for line in lines {
            let (t, i) = line.split_once(':').unwrap();
            let t: usize = t.parse().unwrap();
            assert_eq!(i.parse::<u32>().unwrap(), next[t]);
            next[t] += 1;
        }
    }
}
