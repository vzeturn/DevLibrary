# dl-log

Structured logging for Rust services and tools: levels, `key=value` fields, per-target
filters in the `RUST_LOG` syntax, text / JSON / CLEF / logfmt output, rolling log files,
asynchronous writing and request-scoped context. No dependencies outside DevLibrary, no
`unsafe`, and logging never panics or stops the application when a disk fills up.

## Replaces

| Ecosystem | Packages |
|-----------|----------|
| .NET      | `Serilog`, `Serilog.Sinks.Console`, `Serilog.Sinks.File`, `Serilog.Sinks.Async`, `Serilog.Formatting.Compact`, `Microsoft.Extensions.Logging.Console` |
| Node.js   | `winston` (+ `winston-daily-rotate-file`), `pino`, `morgan` (access logs written from your middleware) |
| Go        | `log/slog` (`TextHandler`, `JSONHandler`), `github.com/sirupsen/logrus`, `gopkg.in/natefinch/lumberjack.v2` |
| Rust      | `log` + `env_logger`, `tracing` + `tracing-subscriber` (`fmt`, `EnvFilter`, `json`) + `tracing-appender` |

## Features

- **Six levels**: `Trace`, `Debug`, `Info`, `Warn`, `Error`, `Fatal`; names parse
  case-insensitively, including Serilog's `Verbose` / `Information` / `Warning`.
- **Macros with structured fields**:
  `info!("booking confirmed"; booking_id = id, guest = %name, channel = ?ch)`, plus
  `format!`-style messages. Arguments are not evaluated when the level is disabled
  (one atomic load, about 3 ns).
- **Filters**: a default level plus per-target overrides parsed from
  `"info,dl_http=debug,sqlx=warn"`; the longest matching target wins; the default level
  can change at runtime.
- **Formats**: human text (optional ANSI colors), JSON lines, CLEF (Serilog compact
  JSON for Seq), logfmt, or your own closure.
- **Sinks**: console (stdout / stderr split by level), files rolling by day and/or size
  with retention, in-memory (tests, UI), any `io::Write`, and an `AsyncSink` wrapper with
  a bounded queue, a drop policy and graceful shutdown.
- **Context**: logger-wide fields (`service`, `version`, `env`) and thread-local scoped
  fields (`request_id`) attached to every record.
- **Timestamps** in UTC or a fixed offset such as `Offset::VIETNAM`, from the system clock
  or an injected clock.
- **Failure-safe**: sink errors go to an error hook, panics in sinks are contained, log
  lines are never torn between threads, and control characters in messages are escaped
  (no log injection).
- **Secret scrubbing**: `scrub` / the `Redacted` format mask connection-string passwords
  (`Password=` / `Pwd=`, any case, ADO.NET quoting), `Bearer` / `Basic` values, URL
  credentials, JWT-looking strings and fields named like a password, secret, token or key.
- Usable without globals: a `Logger` is a plain value you can pass around and test.
- Builds for `wasm32-unknown-unknown` (in-memory and custom sinks).

## Installation

The crate lives in the DevLibrary workspace; cargo finds the package by name inside the
repository:

```toml
[dependencies]
dl-log = { git = "https://github.com/vzeturn/devlibrary" }
```

Pin a revision for reproducible builds: `{ git = "...", rev = "<commit>" }`.

With a local checkout, use a path dependency:

```toml
[dependencies]
dl-log = { path = "../DevLibrary/crates/dl-log" }
```

The minimum supported Rust version is 1.88. `dl-log` re-exports the `dl-json` types
(`Value`, `ToJson`) and the `dl-time` types (`Offset`, `OffsetDateTime`) its API uses.

## Quick start

```rust
use dl_log::{ConsoleSink, Filter, Level, Logger, Offset, error, info, warn};

fn main() -> Result<(), dl_log::Error> {
    let filter = Filter::from_env()?.unwrap_or(Filter::new(Level::Info)); // RUST_LOG
    dl_log::init(
        Logger::builder()
            .filter(filter)
            .offset(Offset::VIETNAM)
            .field("service", "booking-api")
            .sink(ConsoleSink::stdout())
            .build(),
    );

    let booking_id = 42;
    let guest = "Nguyễn Văn An";
    info!("booking confirmed"; booking_id, nights = 3, guest);
    warn!("room {} is not cleaned yet", 1205; floor = 12);
    error!("payment declined"; booking_id, code = "51");

    dl_log::shutdown(); // flush everything before exiting
    Ok(())
}
```

```text
2026-11-15T10:20:30.123+07:00 INFO  booking confirmed booking_id=42 nights=3 guest="Nguyễn Văn An" service=booking-api
2026-11-15T10:20:30.124+07:00 WARN  room 1205 is not cleaned yet floor=12 service=booking-api
2026-11-15T10:20:30.124+07:00 ERROR payment declined booking_id=42 code=51 service=booking-api
```

## Usage guide

### Structured fields

The macros take a `format!` message, then a `;` and comma-separated fields. Keys are
identifiers or string literals; values are converted three ways:

| Syntax | Stored as | Use for |
|---|---|---|
| `key = value` | `ToJson`: numbers, booleans, strings, `Option`, `Vec`, maps, your types | data you want to query (`booking_id = 42`) |
| `key = %value` | the `Display` output, as a string | IDs, IPs, errors, durations |
| `key = ?value` | the `Debug` output, as a string | enums and structs without `ToJson` |
| `key`, `%key`, `?key` | shorthand for `key = key` and so on | |

`logger:` sends to a specific `Logger` instead of the global one, and `target:`
overrides the target (by default the module path, which filters match on). Fields are
borrowed, never moved, and nothing after the level is evaluated when the level is off.

```rust
use dl_log::{Logger, MemorySink, TextFormat, ToJson, Value, info};

struct Room { number: u16, kind: &'static str }

impl ToJson for Room {
    fn to_json(&self) -> Value {
        dl_json::json!({"number": self.number, "kind": self.kind})
    }
}

#[derive(Debug)]
enum Channel { Agoda }

let mem = MemorySink::new().with_format(TextFormat::new().with_time(false));
let log = Logger::builder().sink(mem.clone()).build();

let room = Room { number: 1205, kind: "deluxe" };
let err = std::io::Error::other("timeout after 30s");
let channel = Channel::Agoda;
info!(logger: log, target: "booking", "imported {} bookings", 17;
    room, ?channel, error = %err, "http.status" = 504, retry = true);

assert_eq!(
    mem.lines()[0],
    r#"INFO  imported 17 bookings room={"number":1205,"kind":"deluxe"} channel=Agoda error="timeout after 30s" http.status=504 retry=true"#
);
```

Use `log!(level, …)` when the level is only known at runtime, and `log_enabled!` to skip
expensive preparation:

```rust
use dl_log::{Level, Logger, log, log_enabled};

let log = Logger::builder().level(Level::Info).build();
if log_enabled!(logger: log, Level::Debug) {
    let snapshot = expensive_snapshot(); // skipped: debug is off
    log!(logger: log, Level::Debug, "cache state"; snapshot);
}
# fn expensive_snapshot() -> u64 { unreachable!() }
```

### Filters and `RUST_LOG`-style configuration

A `Filter` is a default level plus per-target levels, written like `env_logger`'s
`RUST_LOG`: `level`, `target=level` and bare `target` (= `trace`), separated by commas.
A directive covers its target and everything below it (`dl_http` covers
`dl_http::server`; `Microsoft` covers `Microsoft.AspNetCore`), and the longest match
wins.

```rust
use dl_log::{Filter, Level, LevelFilter, Logger};

let filter = Filter::parse("info,dl_http=debug,dl_http::pool=warn,sqlx=warn")?;
let log = Logger::builder().filter(filter).build();
assert!(log.enabled(Level::Debug, "dl_http::server"));
assert!(!log.enabled(Level::Info, "dl_http::pool::conn"));
assert!(!log.enabled(Level::Info, "sqlx::query"));

// Change the default level at runtime (an admin endpoint, a signal handler).
log.set_level(LevelFilter::Warn);
assert!(!log.enabled(Level::Info, "booking"));
assert!(log.enabled(Level::Debug, "dl_http::server")); // directives are kept

// Errors point at the bad directive.
let err = Filter::parse("info,sqlx=loud").unwrap_err();
assert_eq!(err.to_string(), "invalid log filter at byte 10: unknown level");
# Ok::<(), dl_log::Error>(())
```

`Filter::from_env()` reads `RUST_LOG`. `Filter::from_env_with(lookup)` reads through a
closure, which lets you use another variable or inject values in tests (changing the
real environment needs `unsafe` since Rust 2024):

```rust
use dl_log::{Filter, Level};

let filter = Filter::from_env_with(|_| std::env::var("BOOKING_LOG").ok())?
    .unwrap_or(Filter::new(Level::Info));

let test_filter = Filter::from_env_with(|key| (key == "RUST_LOG").then(|| "debug".to_string()))?;
assert_eq!(test_filter.unwrap().default_level(), Level::Debug.into());
# let _ = filter;
# Ok::<(), dl_log::Error>(())
```

### Text, JSON, CLEF and logfmt

Every sink takes a `Format`. All of them write one line per record and escape
newlines, so a multi-line message cannot fake extra log entries.

```rust
use dl_log::{Format, JsonFormat, JsonKeys, Level, LogfmtFormat, Record, TextFormat};
use dl_time::OffsetDateTime;

let time = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.123+07:00")?;
let r = Record::new(time, Level::Warn, "booking", "late check-out")
    .with_template("late check-out")
    .with_field("room", 1205)
    .with_field("guest", "Lê Thị Hoa");

assert_eq!(TextFormat::new().format_to_string(&r),
    r#"2026-11-15T10:20:30.123+07:00 WARN  late check-out room=1205 guest="Lê Thị Hoa""#);
assert_eq!(JsonFormat::new().format_to_string(&r),
    r#"{"time":"2026-11-15T10:20:30.123+07:00","level":"WARN","target":"booking","message":"late check-out","room":1205,"guest":"Lê Thị Hoa"}"#);
assert_eq!(JsonFormat::clef().format_to_string(&r),
    r#"{"@t":"2026-11-15T10:20:30.123+07:00","@mt":"late check-out","@l":"Warning","SourceContext":"booking","room":1205,"guest":"Lê Thị Hoa"}"#);
assert_eq!(LogfmtFormat::new().format_to_string(&r),
    r#"time=2026-11-15T10:20:30.123+07:00 level=warn target=booking msg="late check-out" room=1205 guest="Lê Thị Hoa""#);

// Elastic Common Schema key names.
let ecs = JsonFormat::new().with_keys(JsonKeys {
    time: "@timestamp", level: "log.level", target: "log.logger", ..JsonKeys::default()
});
assert!(ecs.format_to_string(&r).starts_with(r#"{"@timestamp":"2026-11-15T10:20:30.123+07:00","log.level":"WARN""#));
# Ok::<(), dl_time::Error>(())
```

Which one to ship where:

- **Seq** and other Serilog tooling: `JsonFormat::clef()`, the
  [Compact Log Event Format](https://clef-json.org/). A message without format arguments
  is written as the message template `@mt`; a formatted message is written rendered as
  `@m`, with `@i`, a hash of the format string, so Seq still groups events by type. Ship
  the file with `seqcli ingest --json` or post the lines to Seq's
  `/api/events/raw?clef` endpoint.
- **Grafana Loki**, **Elasticsearch / OpenSearch**, **Vector**, **Fluent Bit**, cloud log
  services: `JsonFormat::new()` (NDJSON, one object per line), or `LogfmtFormat` for
  Loki's `| logfmt` parser.
- **People** reading a terminal or `journalctl`: `TextFormat`.

Prefer a fixed message with fields (`info!("payment declined"; code)`) over values baked
into the message: the fields stay queryable, and in CLEF the event type stays stable.

### Rolling files for a Linux service

`FileSink` writes `app.log`, rolls by day (`app-20261115.log`) and/or size
(`app-20261115_001.log`), and deletes the oldest files beyond `keep`. The day follows
the logger's offset, so with `Offset::VIETNAM` files change at local midnight.

```rust,no_run
use dl_log::{AsyncSink, FileSink, FlushPolicy, JsonFormat, Level, Logger, Offset};
use std::time::Duration;

let file = FileSink::builder("/var/log/booking/app.log")
    .daily(true)
    .max_size(100 * 1024 * 1024) // and roll at 100 MiB within a day
    .keep(30)                    // files, including today's
    .flush_policy(FlushPolicy::Buffered { flush_level: Level::Warn, interval: Duration::from_secs(1) })
    .format(JsonFormat::new())
    .build()?;
dl_log::init(
    Logger::builder()
        .offset(Offset::VIETNAM)
        .field("service", "booking-api")
        .field("version", env!("CARGO_PKG_VERSION"))
        .sink(AsyncSink::new(file)?)
        .build(),
);
# Ok::<(), dl_log::Error>(())
```

Under **systemd**, standard output already goes to the journal, which adds timestamps and
handles rotation; there, log to stdout with `TextFormat::new().with_time(false)` (or
JSON) instead of files. In **containers** (Docker, Kubernetes) always log to stdout and
let the runtime collect it: `ConsoleSink::stdout().with_format(JsonFormat::new())`.
Use files for services that run without a supervisor, on Windows, or when the operations
team wants self-contained log files.

A rolled file is never renamed; after a restart the sink appends to the newest file of
the day. The file handle is reopened after a write error (for example after the directory
was deleted), and one line is one `write` call, so concurrent writers never interleave.

### Asynchronous logging and shutdown

`AsyncSink` moves the write to a background thread behind a bounded queue. With
`Overflow::Block` (default) a full queue makes the caller wait; with
`Overflow::DropNewest` records are dropped and counted instead. `flush()` waits until
everything queued is written; `shutdown()` and drop write the rest and stop the thread.

```rust
use dl_log::{AsyncOptions, AsyncSink, Logger, MemorySink, Overflow, info};
use std::sync::Arc;

let mem = MemorySink::new();
let sink = Arc::new(AsyncSink::with_options(
    mem.clone(),
    AsyncOptions { capacity: 4096, overflow: Overflow::DropNewest },
)?);
let log = Logger::builder().sink(sink.clone()).build();

for i in 0..1000 {
    info!(logger: log, "tick {}", i);
}
log.shutdown(); // drains the queue, joins the thread; later records are discarded
assert_eq!(mem.len() as u64 + sink.dropped(), 1000);
# Ok::<(), dl_log::Error>(())
```

The global logger lives in a `static` and is never dropped, so call
`dl_log::shutdown()` (or at least `dl_log::flush()`) before `main` returns or before
`std::process::exit`. `fatal!` does not exit the process for you.

### Request-scoped context in a web handler

`with_context` and `push_context` add fields to every record logged on the current
thread until the scope ends. Scopes nest; an inner field overrides an outer one, and a
call-site field overrides both.

```rust
use dl_log::{Logger, MemorySink, TextFormat, fields, info, push_context, warn, with_context};

struct Request { id: String, path: &'static str, user: Option<u32> }

fn handle(log: &Logger, req: &Request) -> u16 {
    let _ctx = push_context(fields!(request_id = %req.id, path = req.path));
    info!(logger: log, "request started");
    let status = match req.user {
        Some(user) => with_context(fields!(user), || {
            info!(logger: log, "bookings loaded"; count = 3);
            200
        }),
        None => {
            warn!(logger: log, "missing session cookie");
            401
        }
    };
    info!(logger: log, "request finished"; status);
    status
}

let mem = MemorySink::new().with_format(TextFormat::new().with_time(false));
let log = Logger::builder().sink(mem.clone()).build();
handle(&log, &Request { id: "01JD3K8Q".into(), path: "/api/bookings", user: Some(7) });
handle(&log, &Request { id: "01JD3K8R".into(), path: "/api/bookings", user: None });

assert_eq!(mem.lines(), [
    "INFO  request started request_id=01JD3K8Q path=/api/bookings",
    "INFO  bookings loaded count=3 user=7 request_id=01JD3K8Q path=/api/bookings",
    "INFO  request finished status=200 request_id=01JD3K8Q path=/api/bookings",
    "INFO  request started request_id=01JD3K8R path=/api/bookings",
    "WARN  missing session cookie request_id=01JD3K8R path=/api/bookings",
    "INFO  request finished status=401 request_id=01JD3K8R path=/api/bookings",
]);
```

Context is per thread. To continue it on another thread (a worker pool, a spawned job),
capture it with `Context::current()` and run the work inside `ctx.in_scope(|| …)`.

### Testing with `MemorySink`

Give the code under test a `Logger` with a `MemorySink` and a fixed clock, then
assert on lines or records. Nothing global is touched, so tests can run in parallel.

```rust
use dl_log::{JsonFormat, Level, Logger, MemorySink, OffsetDateTime, error};

fn charge(log: &Logger, amount: u64) -> Result<(), &'static str> {
    if amount == 0 {
        error!(logger: log, target: "billing", "rejected charge"; amount);
        return Err("zero amount");
    }
    Ok(())
}

let mem = MemorySink::new().with_format(JsonFormat::new());
let log = Logger::builder()
    .clock(|| OffsetDateTime::from_unix_millis(1_794_712_830_000).unwrap())
    .sink(mem.clone())
    .build();

assert!(charge(&log, 0).is_err());
assert_eq!(mem.lines(), [
    r#"{"time":"2026-11-15T03:20:30.000Z","level":"ERROR","target":"billing","message":"rejected charge","amount":0}"#,
]);
let record = &mem.records()[0];
assert_eq!(record.level, Level::Error);
assert_eq!(record.fields[0].1, 0);
```

### Custom sinks and error handling

Implement `Sink` to send records anywhere: a message queue, an HTTP collector, the
browser console. Return errors rather than panicking; the logger passes them to its
error hook as `Error::Sink` and keeps serving the other sinks. Without a hook, the 1st,
2nd, 4th, 8th, … error is printed to standard error.

```rust
use std::io;
use std::sync::{Arc, Mutex};
use dl_log::{Error, Format, JsonFormat, Logger, MemorySink, Record, Sink, error, info};

/// Sends JSON lines to a collector that is currently down.
struct Collector { format: JsonFormat }

impl Sink for Collector {
    fn write(&self, record: &Record) -> io::Result<()> {
        let _line = self.format.format_to_string(record);
        Err(io::Error::new(io::ErrorKind::ConnectionRefused, "collector unreachable"))
    }
    fn name(&self) -> &str { "collector" }
}

let failures = Arc::new(Mutex::new(Vec::new()));
let local = MemorySink::new();
let log = Logger::builder()
    .sink(Collector { format: JsonFormat::new() })
    .sink(local.clone())
    .on_error({
        let failures = failures.clone();
        move |e: &Error| failures.lock().unwrap().push(e.to_string())
    })
    .build();

info!(logger: log, "order placed"; order_id = 991);
error!(logger: log, "order failed"; order_id = 992);

assert_eq!(local.len(), 2); // the other sink still got everything
assert_eq!(log.error_count(), 2);
assert_eq!(failures.lock().unwrap()[0], r#"log sink "collector" failed: collector unreachable"#);
```

`WriterSink` covers anything that implements `io::Write`, and a closure
`|record: &Record, out: &mut String| …` is a `Format`. Records logged from inside a
sink or the error hook are discarded, so a failing sink cannot loop.

### Migrating from Serilog

| Serilog | dl-log |
|---|---|
| `Log.Logger = new LoggerConfiguration()…CreateLogger()` | `dl_log::init(Logger::builder()….build())` |
| `MinimumLevel.Information()` | `.level(Level::Info)` |
| `MinimumLevel.Override("Microsoft", LogEventLevel.Warning)` | `.filter(Filter::parse("info,Microsoft=warn")?)` or `Filter::new(Level::Info).with_target("Microsoft", Level::Warn)` |
| `LoggingLevelSwitch.MinimumLevel = …` | `dl_log::set_level(Level::Debug)` |
| `WriteTo.Console()` | `.sink(ConsoleSink::stdout())` |
| `WriteTo.File("log.txt", rollingInterval: RollingInterval.Day, fileSizeLimitBytes: n, rollOnFileSizeLimit: true, retainedFileCountLimit: 31)` | `.sink(FileSink::builder("log.txt").daily(true).max_size(n).keep(31).build()?)` (same `log20261115_001.txt`-style names, with a `-` before the date) |
| `buffered: true`, `flushToDiskInterval` | `.flush_policy(FlushPolicy::Buffered { flush_level, interval })` |
| `restrictedToMinimumLevel: LogEventLevel.Warning` | `.sink_with_level(Level::Warn, sink)` |
| `WriteTo.Async(a => a.File(…))` | `.sink(AsyncSink::new(FileSink::new(…)?)?)` |
| `Enrich.WithProperty("Service", "booking-api")` | `.field("Service", "booking-api")` |
| `Enrich.FromLogContext()` + `LogContext.PushProperty("RequestId", id)` | `push_context(fields!(RequestId = %id))` (always enabled) |
| `new CompactJsonFormatter()` / `new RenderedCompactJsonFormatter()` | `JsonFormat::clef()` (`@mt` for plain messages, `@m` + `@i` for formatted ones) |
| `outputTemplate: "{Timestamp:o} [{Level:u3}] {Message}{NewLine}"` | `TextFormat` options, or a closure format |
| `Log.Information("Booking {BookingId} confirmed", id)` | `info!("booking confirmed"; BookingId = id)` |
| `Log.ForContext<BookingService>()` | `info!(target: "BookingService", …)` (or the default module path) |
| `Log.CloseAndFlush()` | `dl_log::shutdown()` |
| `SelfLog.Enable(Console.Error)` | `.on_error(\|e\| eprintln!("{e}"))` |
| `"Verbose"`, `"Information"`, `"Warning"` in `appsettings.json` | parse with `"Information".parse::<Level>()` |

The main difference is the message: Serilog extracts properties from `{Name}` holes,
while dl-log messages are Rust format strings and properties go after the `;`.

### Migrating from winston and pino

| winston / pino | dl-log |
|---|---|
| `winston.createLogger({ level: 'info', transports: [new transports.Console()] })` / `pino()` | `Logger::builder().level(Level::Info).sink(ConsoleSink::stdout()).build()` |
| `format.json()` / pino's default output | `ConsoleSink::stdout().with_format(JsonFormat::new())` |
| pino `messageKey: 'msg'`, `timestamp: pino.stdTimeFunctions.isoTime` | `JsonFormat::new().with_keys(JsonKeys { message: "msg", ..JsonKeys::default() })` (always ISO 8601) |
| `logger.info('booking confirmed', { bookingId })` / `logger.info({ bookingId }, 'booking confirmed')` | `info!("booking confirmed"; bookingId)` |
| `defaultMeta: { service }` / pino `base` | `.field("service", "booking-api")` |
| `logger.child({ requestId })` | `with_context(fields!(requestId), \|\| …)`, or a second `Logger` with `.field(…)` |
| `winston-daily-rotate-file` (`datePattern`, `maxSize`, `maxFiles`) / `pino-roll` | `FileSink::builder(…).daily(true).max_size(…).keep(…)` |
| `pino.destination({ sync: false })` | `AsyncSink::new(sink)?` |
| `process.env.LOG_LEVEL` | `Filter::from_env_with(\|_\| std::env::var("LOG_LEVEL").ok())` |
| `morgan('combined')` | `info!(target: "http", "request"; method, path, status, ms)` in your middleware |
| `logger.level = 'debug'` | `log.set_level(Level::Debug)` |

pino writes numeric levels (`30`); dl-log writes names (`"INFO"`). Configure your
collector's level mapping accordingly.

## API overview

| Item | Purpose |
|------|---------|
| `trace!`, `debug!`, `info!`, `warn!`, `error!`, `fatal!`, `log!(level, …)` | Log with message and fields; optional `logger:` and `target:` |
| `log_enabled!(level)`, `fields!(…)` | Level check; build a field list for context |
| `Level`, `LevelFilter` | Severities (`FromStr`, `Display`, `serilog_name`); minimum level or `Off` |
| `Filter` | `new`, `parse`, `from_env`, `from_env_with`, `with_target`, `with_default`, `level_for`, `enabled`, `max_level`, `Display` |
| `Logger`, `LoggerBuilder` | `builder().level / filter / sink / sink_with_level / field / offset / clock / on_error / build`; `enabled`, `log`, `set_level`, `level`, `filter`, `now`, `flush`, `shutdown`, `error_count`, `disabled` |
| `init`, `try_init`, `logger`, `set_level`, `flush`, `shutdown` | The global logger |
| `with_context`, `push_context` → `ContextGuard`, `Context::current`, `Context::in_scope` | Thread-local scoped fields |
| `Record`, `Field`, `Key`, `Record::all_fields` | A log event and its fields (call site, then context, then logger fields) |
| `Format`, `TextFormat`, `JsonFormat` (`new`, `clef`), `JsonKeys`, `LogfmtFormat` | Line formats; closures are formats too |
| `Sink`, `ConsoleSink`, `FileSink` / `FileSinkBuilder` / `FlushPolicy`, `MemorySink`, `WriterSink` | Destinations |
| `AsyncSink`, `AsyncOptions`, `Overflow` | Background writing with a bounded queue |
| `Error` | `ParseLevel`, `ParseFilter { position, reason }`, `AlreadyInitialized`, `Io`, `Sink` |

## Platform support

| Platform | Status |
|---|---|
| Linux, macOS, Windows | Supported, tested in CI |
| `wasm32-unknown-unknown` | Builds. `ConsoleSink`, `FileSink`, `AsyncSink` and `Filter::from_env` are compiled out; use `MemorySink`, `WriterSink` or your own `Sink` (for example one that calls `console.log` through your bindings). There is no system clock: give the logger `.clock(…)` reading `Date.now()`, otherwise records are stamped `1970-01-01T00:00:00Z`. |

The crate uses `std` (files, threads, stderr), has no `unsafe` code and no build script.
MSRV: Rust 1.88.

## Performance

Measured with `cargo bench -p dl-log` (in-house `dl-bench`, median of 30 samples, release
profile with fat LTO) on an Intel Core i7-9850H, Windows 11, Rust 1.100, while other
builds were running, so individual rows varied by up to 1.7x between runs; the table is the quietest run. Each record has
three fields plus one logger field; the null sink is `WriterSink::new(io::sink())`.

| Case | Time per record |
|---|---:|
| Disabled level (`debug!` with format args and fields) | 2.7 ns |
| Disabled by a target directive | 9.4 ns |
| Enabled, text → null sink | 0.59 µs |
| Enabled, text with format arguments → null sink | 0.51 µs |
| Enabled, JSON → null sink | 0.76 µs |
| Enabled, CLEF → null sink | 0.62 µs |
| Enabled, JSON with 3 scoped context fields → null sink | 0.88 µs |
| Formatting only (no I/O): text / JSON / CLEF | 0.23 / 0.32 / 0.26 µs |
| File, one `write` per record (`FlushPolicy::EveryRecord`) | 2.9 µs |
| File, `FlushPolicy::Buffered` | 0.60 µs |
| `AsyncSink` + file, `Overflow::Block` (sustained, bounded by the writer) | 3.1 µs |
| `AsyncSink` + buffered file, `Overflow::Block` | 0.77 µs |
| `AsyncSink`, `Overflow::DropNewest`, cost on the calling thread | 0.50 µs |

An enabled record costs a few allocations (message, field list, `%` values), one
system-clock read, formatting into a reused per-thread buffer and one write. On a
sustained flood the async sink cannot be faster than its destination; it helps with
bursts and keeps slow disks off the request path. A write per record is dominated by the
system call; buffering trades the last second of logs on a crash for a 4–5x speed-up.

Reproduce with:

```sh
cargo bench -p dl-log
cargo bench -p dl-log -- file   # only cases whose name contains "file"
```

## Standards and compatibility

- [CLEF](https://clef-json.org/), the Compact Log Event Format from Serilog: `@t`, `@m`,
  `@mt`, `@l` (absent for Information), `@i`, and `@@` escaping of user properties that
  start with `@`. `@i` is the 32-bit Jenkins one-at-a-time hash of the format string over
  its UTF-16 code units, written as 8 hex digits; it is tested against the published
  Jenkins hash vectors.
- [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339) timestamps with millisecond
  precision (`2026-11-15T10:20:30.123+07:00`, `…Z` at UTC).
- JSON output is valid [RFC 8259](https://www.rfc-editor.org/rfc/rfc8259) with the same
  string escaping as `dl-json` (tests compare both and parse every line back).
- [logfmt](https://brandur.org/logfmt) as parsed by Grafana Loki and Go's `logfmt`
  packages.
- Filter directives follow [`env_logger`](https://docs.rs/env_logger)'s `RUST_LOG`
  syntax, without its `/regex` suffix.
- Rolled file names follow Serilog.Sinks.File (`RollingInterval.Day`,
  `rollOnFileSizeLimit`), with a `-` between the name and the date.

## Limitations and roadmap

- **No OpenTelemetry export yet**: no OTLP sink and no trace or span ids on records
  (planned, together with `@tr` / `@sp` in CLEF). Add ids as context fields meanwhile.
- **No syslog or native journald protocol** (planned). Under systemd, log to stdout.
- No network sinks (HTTP, Seq ingestion, Kafka) out of the box; implement `Sink`, ideally
  behind an `AsyncSink`.
- Messages are Rust format strings, not Serilog message templates: `{Name}` holes do not
  become properties, so put properties after the `;`.
- Scoped context is thread-local. It does not follow `async` tasks that move between
  threads; capture `Context::current()` and enter it where the task runs.
- `target:` must be a `&'static str` in the macros; use `Logger::log` with a hand-built
  `Record` for dynamic targets. The macros split the message from the fields with a
  recursive macro, so a message plus arguments longer than about 100 tokens needs a
  higher `#![recursion_limit]`.
- Per-target directives are fixed when the logger is built; only the default level
  changes at runtime.
- Rolling is not coordinated between processes: give each process its own file set.
  Rolled files are not compressed. The buffered flush interval is checked when a record
  arrives, not by a timer.
- `AsyncSink` can block or drop the newest record; it cannot drop the oldest.
- Timestamps have millisecond precision in all built-in formats.

## License

Licensed under either of [MIT](https://opensource.org/licenses/MIT) or
[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0) at your option.
