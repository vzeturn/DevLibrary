//! Tests of the logger, the macros and sinks working together.

use std::fmt;
use std::fs;
use std::io;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use dl_json::{ToJson, Value, json};
use dl_time::{Offset, OffsetDateTime};

use crate::file::tests::TempDir;
use crate::{
    AsyncOptions, AsyncSink, Error, FileSink, Filter, JsonFormat, Level, LevelFilter, Logger,
    MemorySink, Overflow, Record, Sink, TextFormat, debug, error, fatal, fields, info, log,
    log_enabled, push_context, trace, warn, with_context,
};

/// 2026-11-15T03:20:30.123Z, shown as 10:20:30.123+07:00 in Vietnam.
const T0_MS: i64 = 1_794_712_830_123;

fn fixed_clock() -> OffsetDateTime {
    OffsetDateTime::from_unix_millis(T0_MS).unwrap()
}

fn text_logger(mem: &MemorySink) -> Logger {
    Logger::builder()
        .level(Level::Trace)
        .offset(Offset::VIETNAM)
        .clock(fixed_clock)
        .sink(mem.clone())
        .build()
}

fn bare() -> MemorySink {
    MemorySink::new().with_format(TextFormat::new().with_time(false))
}

/// Counts how often it is formatted.
struct Counted<'a>(&'a AtomicUsize);

impl fmt::Display for Counted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fetch_add(1, Ordering::SeqCst);
        f.write_str("counted")
    }
}

impl fmt::Debug for Counted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl ToJson for Counted<'_> {
    fn to_json(&self) -> Value {
        self.0.fetch_add(1, Ordering::SeqCst);
        Value::from("counted")
    }
}

#[test]
fn exact_text_line_with_vietnam_offset() {
    let mem = MemorySink::new();
    let log = text_logger(&mem);
    let id = 42;
    info!(logger: log, "booking confirmed"; booking_id = id, nights = 3);
    assert_eq!(
        mem.lines(),
        ["2026-11-15T10:20:30.123+07:00 INFO  booking confirmed booking_id=42 nights=3"]
    );
    let r = &mem.records()[0];
    assert_eq!(r.target, module_path!());
    assert_eq!(r.template, Some("booking confirmed"));
    assert_eq!(r.file, Some(file!()));
    assert!(r.line.is_some());
}

#[test]
fn exact_json_and_clef_lines() {
    let json = MemorySink::new().with_format(JsonFormat::new());
    let clef = MemorySink::new().with_format(JsonFormat::clef());
    let log = Logger::builder()
        .offset(Offset::VIETNAM)
        .clock(fixed_clock)
        .field("service", "booking-api")
        .sink(json.clone())
        .sink(clef.clone())
        .build();
    let user = "An";
    info!(logger: log, target: "booking", "booking confirmed"; booking_id = 42, nights = 3);
    warn!(logger: log, target: "auth", "user {} logged in from {}", user, "Đà Nẵng"; attempts = 3);
    assert_eq!(
        json.lines(),
        [
            r#"{"time":"2026-11-15T10:20:30.123+07:00","level":"INFO","target":"booking","message":"booking confirmed","booking_id":42,"nights":3,"service":"booking-api"}"#,
            r#"{"time":"2026-11-15T10:20:30.123+07:00","level":"WARN","target":"auth","message":"user An logged in from Đà Nẵng","attempts":3,"service":"booking-api"}"#,
        ]
    );
    let id = format!(
        "{:08x}",
        crate::format::event_id("user {} logged in from {}")
    );
    assert_eq!(
        clef.lines(),
        [
            r#"{"@t":"2026-11-15T10:20:30.123+07:00","@mt":"booking confirmed","SourceContext":"booking","booking_id":42,"nights":3,"service":"booking-api"}"#.to_string(),
            format!(
                r#"{{"@t":"2026-11-15T10:20:30.123+07:00","@m":"user An logged in from Đà Nẵng","@i":"{id}","@l":"Warning","SourceContext":"auth","attempts":3,"service":"booking-api"}}"#
            ),
        ]
    );
}

#[test]
fn field_kinds() {
    #[derive(Debug)]
    struct Room {
        number: u16,
    }
    let mem = bare();
    let log = text_logger(&mem);
    let guest = String::from("Trần Thị Bình");
    let room = Room { number: 1205 };
    let rate = 1_250_000.5;
    let tags = vec!["vip", "late"];
    let none: Option<u32> = None;
    info!(logger: log, "fields"; plain = 7, text = "a b", display = %guest, debug = ?room, rate, ?tags, %rate, none, "dotted.key" = true, "lit.display" = %1, "lit.debug" = ?'x');
    let r = &mem.records()[0];
    let got: Vec<(String, Value)> = r
        .fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let expected: Vec<(String, Value)> = vec![
        ("plain".into(), json!(7)),
        ("text".into(), json!("a b")),
        ("display".into(), json!("Trần Thị Bình")),
        ("debug".into(), json!("Room { number: 1205 }")),
        ("rate".into(), json!(1_250_000.5)),
        ("tags".into(), json!("[\"vip\", \"late\"]")),
        ("rate".into(), json!("1250000.5")),
        ("none".into(), Value::Null),
        ("dotted.key".into(), json!(true)),
        ("lit.display".into(), json!("1")),
        ("lit.debug".into(), json!("'x'")),
    ];
    assert_eq!(got, expected);
    // Values are borrowed, not moved.
    assert_eq!(guest, "Trần Thị Bình");
    assert_eq!(tags.len(), 2);
    let _ = room.number;
    // Text output skips the second `rate` (first occurrence wins).
    assert_eq!(
        mem.lines()[0],
        r#"INFO  fields plain=7 text="a b" display="Trần Thị Bình" debug="Room { number: 1205 }" rate=1250000.5 tags="[\"vip\", \"late\"]" none=null dotted.key=true lit.display=1 lit.debug='x'"#
    );
}

#[test]
fn message_templates() {
    let mem = bare();
    let log = text_logger(&mem);
    let user = "an";
    let n = 3;
    info!(logger: log, "user {} logged in", user);
    info!(logger: log, "user {user} has {n} sessions");
    info!(logger: log, "padded {:>5}|{:.2}", n, 1.0 / 3.0,);
    info!(logger: log, "named {who}", who = user; extra = 1);
    info!(logger: log, "braces {{literal}}");
    info!(logger: log, "semicolon in arg {}", { let x = 1; x + 1 }; after = "ok");
    info!(logger: log, concat!("built ", "message"));
    assert_eq!(
        mem.lines(),
        [
            "INFO  user an logged in",
            "INFO  user an has 3 sessions",
            "INFO  padded     3|0.33",
            "INFO  named an extra=1",
            "INFO  braces {literal}",
            "INFO  semicolon in arg 2 after=ok",
            "INFO  built message",
        ]
    );
    let templates: Vec<Option<&str>> = mem.records().iter().map(|r| r.template).collect();
    assert_eq!(templates[0], Some("user {} logged in"));
    assert_eq!(templates[4], Some("braces {{literal}}"));
    assert_eq!(templates[6], None);
}

#[test]
fn all_level_macros_and_targets() {
    let mem = MemorySink::new().with_format(TextFormat::new().with_time(false).with_target(true));
    let log = text_logger(&mem);
    trace!(logger: log, "t");
    debug!(logger: &log, "d");
    info!(logger: log, target: "custom", "i");
    warn!(logger: log, "w");
    error!(logger: log, "e");
    fatal!(logger: log, "f");
    log!(logger: log, target: "dyn", Level::Warn, "dynamic {}", 1; k = 2);
    log!(logger: log, Level::Debug, "dynamic plain");
    let m = module_path!();
    assert_eq!(
        mem.lines(),
        [
            format!("TRACE {m}: t"),
            format!("DEBUG {m}: d"),
            "INFO  custom: i".to_string(),
            format!("WARN  {m}: w"),
            format!("ERROR {m}: e"),
            format!("FATAL {m}: f"),
            "WARN  dyn: dynamic 1 k=2".to_string(),
            format!("DEBUG {m}: dynamic plain"),
        ]
    );
}

#[test]
fn disabled_levels_do_not_evaluate_arguments() {
    let calls = AtomicUsize::new(0);
    let mem = bare();
    let log = Logger::builder()
        .filter(Filter::parse("warn,noisy=off,chatty=trace").unwrap())
        .sink(mem.clone())
        .build();
    let c = Counted(&calls);
    let side_effect = || {
        calls.fetch_add(1, Ordering::SeqCst);
        5
    };
    info!(logger: log, "msg {}", c; a = %c, b = ?c, d = c, e = side_effect());
    debug!(logger: log, "msg {}", side_effect());
    trace!(logger: log, "{c}");
    error!(logger: log, target: "noisy", "msg {}", c; a = %c);
    log!(logger: log, Level::Info, "{}", c);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(mem.is_empty());
    // Enabled: everything is evaluated exactly once.
    warn!(logger: log, "msg {}", c; a = %c, b = ?c, d = c, e = side_effect());
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    trace!(logger: log, target: "chatty::x", "{c}");
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    assert_eq!(
        mem.lines(),
        [
            "WARN  msg counted a=counted b=counted d=counted e=5",
            "TRACE counted"
        ]
    );
}

#[test]
fn filters_and_runtime_level_changes() {
    let mem = bare();
    let log = Logger::builder()
        .filter(Filter::parse("info,dl_http=debug,dl_http::db=error,sqlx=warn").unwrap())
        .sink(mem.clone())
        .build();
    assert!(log_enabled!(logger: log, target: "dl_http::server", Level::Debug));
    assert!(!log_enabled!(logger: log, target: "dl_http::db::pool", Level::Warn));
    assert!(!log_enabled!(logger: log, target: "sqlx", Level::Info));
    assert!(log_enabled!(logger: log, Level::Info));
    assert!(!log_enabled!(logger: log, Level::Debug,));
    debug!(logger: log, target: "dl_http::server", "shown");
    info!(logger: log, target: "sqlx::query", "hidden");
    assert_eq!(mem.len(), 1);

    log.set_level(LevelFilter::Trace);
    assert_eq!(log.level(), LevelFilter::Trace);
    assert!(log.enabled(Level::Trace, "anything"));
    assert!(!log.enabled(Level::Info, "sqlx")); // directives stay
    log.set_level(Level::Error);
    assert!(!log.enabled(Level::Warn, "my_app"));
    assert!(log.enabled(Level::Debug, "dl_http"));
    assert_eq!(
        log.filter().to_string(),
        "error,dl_http::db=error,dl_http=debug,sqlx=warn"
    );

    // No directives: the fast path alone decides.
    let simple = Logger::builder().level(Level::Warn).build();
    assert!(!simple.enabled(Level::Info, "x") && simple.enabled(Level::Warn, "x"));
    simple.set_level(LevelFilter::Off);
    assert!(!simple.enabled(Level::Fatal, "x"));
    assert!(!Logger::disabled().enabled(Level::Fatal, "x"));
}

#[test]
fn filter_from_injected_env() {
    let env = |key: &str| (key == "RUST_LOG").then(|| "warn,booking=debug".to_string());
    let filter = Filter::from_env_with(env).unwrap().unwrap_or_default();
    let log = Logger::builder().filter(filter).build();
    assert!(log.enabled(Level::Debug, "booking::api"));
    assert!(!log.enabled(Level::Info, "other"));
}

#[test]
fn context_static_and_call_site_fields() {
    let mem = bare();
    let log = Logger::builder()
        .field("service", "booking-api")
        .field("env", "prod")
        .clock(fixed_clock)
        .sink(mem.clone())
        .build();
    with_context(fields!(request_id = "r-1", user = 7), || {
        info!(logger: log, "outer");
        with_context(fields!(step = "pay", request_id = "r-1b"), || {
            info!(logger: log, "inner"; user = 8, env = "override");
        });
        let guard = push_context(fields!(job = "email"));
        info!(logger: log, "guard");
        drop(guard);
        info!(logger: log, "after");
    });
    info!(logger: log, "none");
    assert_eq!(
        mem.lines(),
        [
            "INFO  outer request_id=r-1 user=7 service=booking-api env=prod",
            "INFO  inner user=8 env=override step=pay request_id=r-1b service=booking-api",
            "INFO  guard job=email request_id=r-1 user=7 service=booking-api env=prod",
            "INFO  after request_id=r-1 user=7 service=booking-api env=prod",
            "INFO  none service=booking-api env=prod",
        ]
    );
}

#[test]
fn manual_records_and_per_sink_levels() {
    let all = bare();
    let errors = bare();
    let log = Logger::builder()
        .field("svc", "x")
        .sink(all.clone())
        .sink_with_level(Level::Error, errors.clone())
        .build();
    let t = OffsetDateTime::UNIX_EPOCH;
    log.log(Record::new(t, Level::Warn, "bridge", "from another system").with_field("k", 1));
    log.log(Record::new(t, Level::Error, "bridge", "bad"));
    log.log(Record::new(t, Level::Debug, "bridge", "filtered"));
    assert_eq!(
        all.lines(),
        ["WARN  from another system k=1 svc=x", "ERROR bad svc=x"]
    );
    assert_eq!(errors.lines(), ["ERROR bad svc=x"]);
    assert_eq!(all.records()[0].time, t);
}

#[test]
fn many_threads_into_memory_sink() {
    let mem = MemorySink::new().with_format(JsonFormat::new());
    let log = Logger::builder().sink(mem.clone()).build();
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let log = log.clone();
            thread::spawn(move || {
                with_context(fields!(thread = t), || {
                    for i in 0..250 {
                        info!(logger: log, "event {} from {}", i, t; i, payload = "x".repeat(50));
                    }
                })
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let lines = mem.lines();
    assert_eq!(lines.len(), 2000);
    let mut next = [0u64; 8];
    for line in &lines {
        let v = dl_json::parse(line).unwrap();
        let t = v["thread"].as_u64().unwrap() as usize;
        assert_eq!(v["i"].as_u64().unwrap(), next[t]);
        assert_eq!(
            v["message"],
            format!("event {} from {}", next[t], t).as_str()
        );
        next[t] += 1;
    }
}

#[test]
fn many_threads_into_file_sink_have_no_torn_lines() {
    let tmp = TempDir::new("threads");
    let path = tmp.0.join("app.log");
    let file = FileSink::builder(&path)
        .format(JsonFormat::new())
        .build()
        .unwrap();
    let log = Logger::builder().sink(file).build();
    let threads: Vec<_> = (0..8)
        .map(|t| {
            let log = log.clone();
            thread::spawn(move || {
                for i in 0..500 {
                    // Long, varied lines make torn writes visible.
                    info!(logger: log, "line"; t, i, pad = "Đ".repeat(i % 200));
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    log.shutdown();
    let content = fs::read_to_string(&path).unwrap();
    let mut seen = [0u64; 8];
    let mut count = 0;
    for line in content.lines() {
        let v = dl_json::parse(line).unwrap_or_else(|e| panic!("torn line {line:?}: {e}"));
        let t = v["t"].as_u64().unwrap() as usize;
        assert_eq!(v["i"].as_u64().unwrap(), seen[t], "per-thread order");
        seen[t] += 1;
        count += 1;
    }
    assert_eq!(count, 4000);
    assert_eq!(log.error_count(), 0);
}

#[test]
fn daily_rolling_with_injected_clock_and_retention() {
    let tmp = TempDir::new("clock");
    let now = Arc::new(AtomicI64::new(T0_MS));
    let clock = {
        let now = now.clone();
        move || OffsetDateTime::from_unix_millis(now.load(Ordering::SeqCst)).unwrap()
    };
    let file = FileSink::builder(tmp.0.join("app.log"))
        .daily(true)
        .keep(2)
        .build()
        .unwrap();
    let log = Logger::builder()
        .offset(Offset::VIETNAM)
        .clock(clock)
        .sink(file)
        .build();
    const DAY: i64 = 86_400_000;
    info!(logger: log, "day one");
    // 2026-11-15 23:59:59.999 local is still day one.
    now.store(
        T0_MS + (13 * 3600 + 39 * 60 + 29) * 1000 + 876,
        Ordering::SeqCst,
    );
    assert_eq!(log.now().to_rfc3339(), "2026-11-15T23:59:59.999+07:00");
    info!(logger: log, "end of day one");
    now.fetch_add(1, Ordering::SeqCst);
    info!(logger: log, "day two");
    now.fetch_add(DAY, Ordering::SeqCst);
    info!(logger: log, "day three");
    assert_eq!(tmp.names(), ["app-20261116.log", "app-20261117.log"]);
    assert_eq!(
        tmp.read("app-20261116.log"),
        "2026-11-16T00:00:00.000+07:00 INFO  day two\n"
    );
    assert!(tmp.read("app-20261117.log").ends_with("INFO  day three\n"));
}

#[test]
fn async_file_logging_and_shutdown() {
    let tmp = TempDir::new("async");
    let path = tmp.0.join("app.log");
    let file = FileSink::builder(&path)
        .format(|r: &Record, out: &mut String| out.push_str(&r.message))
        .build()
        .unwrap();
    let sink = Arc::new(
        AsyncSink::with_options(
            file,
            AsyncOptions {
                capacity: 64,
                overflow: Overflow::Block,
            },
        )
        .unwrap(),
    );
    let log = Logger::builder().sink(sink.clone()).build();
    for i in 0..1000 {
        info!(logger: log, "{}", i);
    }
    log.flush();
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1000);
    for i in 1000..1500 {
        info!(logger: log, "{}", i);
    }
    log.shutdown();
    let content = fs::read_to_string(&path).unwrap();
    let expected: String = (0..1500).map(|i| format!("{i}\n")).collect();
    assert_eq!(content, expected);
    info!(logger: log, "after shutdown is discarded");
    log.set_level(Level::Trace); // no effect after shutdown
    assert!(!log.enabled(Level::Fatal, "x"));
    assert_eq!(sink.dropped(), 0);
    log.shutdown(); // idempotent
}

#[test]
fn sink_failures_go_to_the_error_hook() {
    struct Broken;
    impl Sink for Broken {
        fn write(&self, _: &Record) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "No space left on device",
            ))
        }
        fn flush(&self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
        fn name(&self) -> &str {
            "broken"
        }
    }
    let reports = Arc::new(Mutex::new(Vec::new()));
    let mem = bare();
    let log = Logger::builder()
        .sink(Broken)
        .sink(mem.clone())
        .on_error({
            let reports = reports.clone();
            move |e: &Error| reports.lock().unwrap().push(e.to_string())
        })
        .build();
    error!(logger: log, "payment failed"; code = 51);
    info!(logger: log, "still logging");
    log.flush();
    assert_eq!(mem.len(), 2, "other sinks keep working");
    assert_eq!(log.error_count(), 3);
    assert_eq!(
        *reports.lock().unwrap(),
        [
            "log sink \"broken\" failed: No space left on device",
            "log sink \"broken\" failed: No space left on device",
            "log sink \"broken\" failed: flush failed",
        ]
    );
}

#[test]
fn file_errors_are_reported_and_recovered() {
    let tmp = TempDir::new("recover");
    let dir = tmp.0.join("logs");
    let file = FileSink::builder(dir.join("app.log"))
        .daily(true)
        .build()
        .unwrap();
    let errors = Arc::new(AtomicUsize::new(0));
    let log = Logger::builder()
        .clock(fixed_clock)
        .sink(file)
        .on_error({
            let errors = errors.clone();
            move |_: &Error| {
                errors.fetch_add(1, Ordering::SeqCst);
            }
        })
        .build();
    // Daily files are opened lazily; remove the directory first.
    fs::remove_dir_all(&dir).unwrap();
    info!(logger: log, "lost");
    assert_eq!(errors.load(Ordering::SeqCst), 1);
    fs::create_dir_all(&dir).unwrap();
    info!(logger: log, "recovered");
    assert_eq!(errors.load(Ordering::SeqCst), 1);
    assert!(
        fs::read_to_string(dir.join("app-20261115.log"))
            .unwrap()
            .ends_with("INFO  recovered\n")
    );
}

#[test]
fn logging_from_inside_a_sink_does_not_recurse() {
    struct Loopy {
        log: Mutex<Option<Logger>>,
        writes: AtomicUsize,
    }
    impl Sink for Loopy {
        fn write(&self, _: &Record) -> io::Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            if let Some(log) = self.log.lock().unwrap().as_ref() {
                info!(logger: log, "from inside the sink");
            }
            Err(io::Error::other("and failing"))
        }
    }
    let loopy = Arc::new(Loopy {
        log: Mutex::new(None),
        writes: AtomicUsize::new(0),
    });
    let log = Logger::builder()
        .sink(loopy.clone())
        .on_error({
            let loopy = loopy.clone();
            move |_: &Error| {
                if let Some(log) = loopy.log.lock().unwrap().as_ref() {
                    error!(logger: log, "from the hook");
                }
            }
        })
        .build();
    *loopy.log.lock().unwrap() = Some(log.clone());
    info!(logger: log, "outer");
    assert_eq!(loopy.writes.load(Ordering::SeqCst), 1);
    assert_eq!(log.error_count(), 1);
    *loopy.log.lock().unwrap() = None; // break the Arc cycle
}

#[test]
fn global_logger() {
    // The only test that touches the process-wide logger.
    assert!(!log_enabled!(Level::Fatal));
    info!("before init is discarded");
    let mem = bare();
    let rejected = Arc::new(AtomicUsize::new(0));
    crate::init(
        Logger::builder()
            .level(Level::Debug)
            .sink(mem.clone())
            .build(),
    );
    crate::init(
        Logger::builder()
            .on_error({
                let rejected = rejected.clone();
                move |e: &Error| {
                    assert!(matches!(e, Error::AlreadyInitialized));
                    rejected.fetch_add(1, Ordering::SeqCst);
                }
            })
            .build(),
    );
    assert_eq!(rejected.load(Ordering::SeqCst), 1);
    assert!(matches!(
        crate::try_init(Logger::disabled()),
        Err(Error::AlreadyInitialized)
    ));

    info!("global {}", 1; k = "v");
    debug!(target: "t", "debug");
    trace!("hidden");
    assert!(log_enabled!(Level::Debug));
    assert!(log_enabled!(target: "t", Level::Debug));
    crate::set_level(LevelFilter::Warn);
    info!("hidden after set_level");
    log!(Level::Error, "dynamic");
    log!(target: "x", Level::Warn, "dynamic target");
    crate::flush();
    assert_eq!(
        mem.lines(),
        [
            "INFO  global 1 k=v",
            "DEBUG debug",
            "ERROR dynamic",
            "WARN  dynamic target"
        ]
    );
    assert_eq!(crate::logger().level(), LevelFilter::Warn);
    crate::shutdown();
    error!("after shutdown");
    assert_eq!(mem.len(), 4);
}

#[test]
fn logger_debug_output() {
    let log = Logger::builder()
        .filter(Filter::parse("warn,a=debug").unwrap())
        .sink(MemorySink::new())
        .build();
    let s = format!("{log:?}");
    assert!(s.contains("warn,a=debug") && s.contains("memory"), "{s}");
    assert!(format!("{:?}", Logger::builder()).contains("LoggerBuilder"));
}
