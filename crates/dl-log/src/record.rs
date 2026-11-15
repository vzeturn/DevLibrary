//! Log records, fields and scoped context.

use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use dl_json::Value;
use dl_time::OffsetDateTime;

use crate::Level;

/// A field name. Names written in macros are `&'static str` and are not copied.
pub type Key = Cow<'static, str>;

/// A structured field: a name and a JSON value.
pub type Field = (Key, Value);

/// One log event, as handed to [`Sink`](crate::Sink)s and [`Format`](crate::Format)s.
///
/// The logging macros build records for you. Build one by hand to bridge events from
/// another source into [`Logger::log`](crate::Logger::log), or to test a custom format:
///
/// ```
/// use dl_log::{Format, Level, Record, TextFormat};
/// use dl_time::OffsetDateTime;
///
/// let time = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.123+07:00")?;
/// let record = Record::new(time, Level::Info, "booking", "booking confirmed")
///     .with_field("booking_id", 42)
///     .with_field("guest", "Nguyễn Văn An");
/// assert_eq!(
///     TextFormat::new().format_to_string(&record),
///     r#"2026-11-15T10:20:30.123+07:00 INFO  booking confirmed booking_id=42 guest="Nguyễn Văn An""#
/// );
/// # Ok::<(), dl_time::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Record {
    /// When the event happened, at the logger's offset.
    pub time: OffsetDateTime,
    /// Severity.
    pub level: Level,
    /// Where the event comes from. The macros use `module_path!()` (`my_app::booking`)
    /// unless `target:` is given; filters match on it.
    pub target: Cow<'static, str>,
    /// The rendered message.
    pub message: String,
    /// The format string literal the message was rendered from, when known. Used for the
    /// CLEF `@mt` / `@i` properties.
    pub template: Option<&'static str>,
    /// Fields given at the call site, in order.
    pub fields: Vec<Field>,
    /// Scoped context and logger-wide fields attached by the logger.
    pub context: Context,
    /// Source file of the call site.
    pub file: Option<&'static str>,
    /// Source line of the call site.
    pub line: Option<u32>,
}

impl Record {
    /// A record without fields, context, template or location.
    pub fn new(
        time: OffsetDateTime,
        level: Level,
        target: impl Into<Cow<'static, str>>,
        message: impl Into<String>,
    ) -> Record {
        Record {
            time,
            level,
            target: target.into(),
            message: message.into(),
            template: None,
            fields: Vec::new(),
            context: Context::default(),
            file: None,
            line: None,
        }
    }

    /// Appends a call-site field.
    pub fn with_field(mut self, key: impl Into<Key>, value: impl Into<Value>) -> Record {
        self.fields.push((key.into(), value.into()));
        self
    }

    /// Sets the message template (see [`template`](Self::template)).
    pub fn with_template(mut self, template: &'static str) -> Record {
        self.template = Some(template);
        self
    }

    /// Sets the source location.
    pub fn with_location(mut self, file: &'static str, line: u32) -> Record {
        self.file = Some(file);
        self.line = Some(line);
        self
    }

    /// All fields to write: call-site fields first, then scoped context from the
    /// innermost scope outwards, then logger-wide fields. When a name appears more than
    /// once, only the first occurrence is yielded, so a call-site field overrides a
    /// context field of the same name, and an inner scope overrides an outer one.
    pub fn all_fields(&self) -> Fields<'_> {
        let raw = Raw {
            cur: self.fields.iter(),
            next_frame: self.context.scoped.as_deref(),
            base: self.context.base.as_deref(),
        };
        Fields {
            start: raw.clone(),
            raw,
            seen: 0,
        }
    }
}

/// A stack of scoped fields (and logger-wide fields once attached to a [`Record`]).
///
/// Cloning is cheap (a reference count). Capture the current context with
/// [`Context::current`] to carry it to another thread:
///
/// ```
/// use dl_log::{Context, fields, with_context};
///
/// with_context(fields!(request_id = "r-1"), || {
///     let ctx = Context::current();
///     std::thread::spawn(move || {
///         ctx.in_scope(|| {
///             let current = Context::current();
///             let keys: Vec<&str> = current.iter().map(|(k, _)| k).collect();
///             assert_eq!(keys, ["request_id"]);
///         })
///     })
///     .join()
///     .unwrap();
/// });
/// ```
#[derive(Clone, Default)]
pub struct Context {
    scoped: Option<Arc<Frame>>,
    base: Option<Arc<[Field]>>,
}

struct Frame {
    fields: Vec<Field>,
    parent: Option<Arc<Frame>>,
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Frame>>> = const { RefCell::new(None) };
}

fn current_frame() -> Option<Arc<Frame>> {
    CURRENT
        .try_with(|c| c.try_borrow().ok().and_then(|f| f.clone()))
        .ok()
        .flatten()
}

/// Replaces the current frame and returns the previous one (`None` when the thread-local
/// is being destroyed).
fn swap_frame(frame: Option<Arc<Frame>>) -> Option<Option<Arc<Frame>>> {
    CURRENT
        .try_with(|c| {
            c.try_borrow_mut()
                .ok()
                .map(|mut cur| std::mem::replace(&mut *cur, frame))
        })
        .ok()
        .flatten()
}

impl Context {
    /// The scoped context of the calling thread (without logger-wide fields).
    pub fn current() -> Context {
        Context {
            scoped: current_frame(),
            base: None,
        }
    }

    pub(crate) fn current_with_base(base: Option<Arc<[Field]>>) -> Context {
        Context {
            scoped: current_frame(),
            base,
        }
    }

    /// Returns `true` when there are no fields.
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    /// All fields, innermost scope first, then logger-wide fields. Shadowed names are
    /// included; [`Record::all_fields`] removes them.
    pub fn iter(&self) -> ContextIter<'_> {
        ContextIter {
            raw: Raw {
                cur: [].iter(),
                next_frame: self.scoped.as_deref(),
                base: self.base.as_deref(),
            },
        }
    }

    /// Runs `f` with this context as the calling thread's scoped context, then restores
    /// the previous one (also when `f` panics). Logger-wide fields are not part of the
    /// scoped context.
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        let _guard = ContextGuard {
            prev: swap_frame(self.scoped.clone()),
            _not_send: PhantomData,
        };
        f()
    }
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// Runs `f` with `fields` added to the calling thread's scoped context. Every record
/// logged inside `f` on this thread carries them. Scopes nest; an inner field overrides
/// an outer one of the same name.
///
/// ```
/// use dl_log::{Logger, MemorySink, TextFormat, fields, info, with_context};
///
/// let mem = MemorySink::new().with_format(TextFormat::new().with_time(false));
/// let log = Logger::builder().sink(mem.clone()).build();
/// with_context(fields!(request_id = "r-42"), || {
///     with_context(fields!(step = "payment"), || {
///         info!(logger: log, "charged"; amount = 1_250_000);
///     });
/// });
/// assert_eq!(mem.lines(), ["INFO  charged amount=1250000 step=payment request_id=r-42"]);
/// ```
pub fn with_context<R>(fields: Vec<Field>, f: impl FnOnce() -> R) -> R {
    let _guard = push_context(fields);
    f()
}

/// Adds `fields` to the calling thread's scoped context until the returned guard is
/// dropped. Use it where a closure is awkward, for example at the top of a request
/// handler. The guard cannot be sent to another thread.
///
/// ```
/// use dl_log::{Context, fields, push_context};
///
/// {
///     let _ctx = push_context(fields!(request_id = "r-7"));
///     assert!(!Context::current().is_empty());
/// }
/// assert!(Context::current().is_empty());
/// ```
#[must_use = "the context is removed when the guard is dropped"]
pub fn push_context(fields: Vec<Field>) -> ContextGuard {
    let frame = Arc::new(Frame {
        fields,
        parent: current_frame(),
    });
    ContextGuard {
        prev: swap_frame(Some(frame)),
        _not_send: PhantomData,
    }
}

/// Restores the previous scoped context when dropped. Returned by [`push_context`].
pub struct ContextGuard {
    prev: Option<Option<Arc<Frame>>>,
    // Thread-local state: the guard must be dropped on the thread that created it.
    _not_send: PhantomData<*const ()>,
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            swap_frame(prev);
        }
    }
}

impl fmt::Debug for ContextGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextGuard")
    }
}

#[derive(Clone)]
struct Raw<'a> {
    cur: std::slice::Iter<'a, Field>,
    next_frame: Option<&'a Frame>,
    base: Option<&'a [Field]>,
}

impl<'a> Iterator for Raw<'a> {
    type Item = &'a Field;

    fn next(&mut self) -> Option<&'a Field> {
        loop {
            if let Some(f) = self.cur.next() {
                return Some(f);
            }
            if let Some(frame) = self.next_frame {
                self.cur = frame.fields.iter();
                self.next_frame = frame.parent.as_deref();
                continue;
            }
            self.cur = self.base.take()?.iter();
        }
    }
}

/// Iterator over the fields of a [`Context`], returned by [`Context::iter`].
#[derive(Clone)]
pub struct ContextIter<'a> {
    raw: Raw<'a>,
}

impl<'a> Iterator for ContextIter<'a> {
    type Item = (&'a str, &'a Value);

    fn next(&mut self) -> Option<(&'a str, &'a Value)> {
        self.raw.next().map(|(k, v)| (k.as_ref(), v))
    }
}

impl fmt::Debug for ContextIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContextIter")
    }
}

/// Iterator over the fields of a [`Record`] without shadowed names, returned by
/// [`Record::all_fields`].
#[derive(Clone)]
pub struct Fields<'a> {
    start: Raw<'a>,
    raw: Raw<'a>,
    seen: usize,
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a str, &'a Value);

    fn next(&mut self) -> Option<(&'a str, &'a Value)> {
        loop {
            let (key, value) = self.raw.next()?;
            let n = self.seen;
            self.seen += 1;
            // Records carry a handful of fields, so a quadratic scan beats hashing.
            if !self.start.clone().take(n).any(|(k, _)| k == key) {
                return Some((key.as_ref(), value));
            }
        }
    }
}

impl fmt::Debug for Fields<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Fields")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;

    fn keys(ctx: &Context) -> Vec<String> {
        ctx.iter().map(|(k, v)| format!("{k}={v}")).collect()
    }

    #[test]
    fn nesting_restores_outer_scopes() {
        assert!(Context::current().is_empty());
        with_context(fields!(request_id = "a"), || {
            assert_eq!(keys(&Context::current()), ["request_id=\"a\""]);
            with_context(fields!(user = 7, request_id = "b"), || {
                assert_eq!(
                    keys(&Context::current()),
                    ["user=7", "request_id=\"b\"", "request_id=\"a\""]
                );
            });
            assert_eq!(keys(&Context::current()), ["request_id=\"a\""]);
        });
        assert!(Context::current().is_empty());
    }

    #[test]
    fn restored_after_panic() {
        let result = std::panic::catch_unwind(|| {
            with_context(fields!(x = 1), || panic!("boom"));
        });
        assert!(result.is_err());
        assert!(Context::current().is_empty());
    }

    #[test]
    fn guards_dropped_out_of_order_restore_their_snapshot() {
        let a = push_context(fields!(a = 1));
        let b = push_context(fields!(b = 2));
        drop(a); // restores "nothing"
        assert!(Context::current().is_empty());
        drop(b); // restores the state seen when b was pushed: [a]
        assert_eq!(keys(&Context::current()), ["a=1"]);
        Context::default().in_scope(|| assert!(Context::current().is_empty()));
        assert_eq!(keys(&Context::current()), ["a=1"]);
        // Leave the thread clean for other tests on the same worker thread.
        let _ = swap_frame(None);
    }

    #[test]
    fn context_is_per_thread() {
        with_context(fields!(t = "main"), || {
            std::thread::spawn(|| assert!(Context::current().is_empty()))
                .join()
                .unwrap();
        });
    }

    #[test]
    fn record_fields_skip_shadowed_names() {
        let time = OffsetDateTime::UNIX_EPOCH;
        let base: Arc<[Field]> = Arc::from(vec![
            (Key::from("service"), Value::from("api")),
            (Key::from("request_id"), Value::from("base")),
        ]);
        let record = with_context(fields!(request_id = "ctx", step = 1), || {
            let mut r = Record::new(time, Level::Info, "t", "m")
                .with_field("step", 2)
                .with_field("a", true)
                .with_field("a", false);
            r.context = Context::current_with_base(Some(base.clone()));
            r
        });
        let got: Vec<String> = record
            .all_fields()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        assert_eq!(
            got,
            ["step=2", "a=true", "request_id=\"ctx\"", "service=\"api\""]
        );
        assert_eq!(record.context.iter().count(), 4);
        assert!(format!("{:?}", record.context).contains("\"service\""));
    }
}
