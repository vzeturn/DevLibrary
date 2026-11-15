//! `cargo bench -p dl-log`

use std::hint::black_box;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use dl_bench::Bench;
use dl_log::{
    AsyncOptions, AsyncSink, FileSink, FlushPolicy, Format, JsonFormat, Level, Logger, Overflow,
    Record, TextFormat, WriterSink, debug, fields, info, with_context,
};
use dl_time::{Offset, OffsetDateTime};

fn null_logger(format: impl Format + 'static) -> Logger {
    Logger::builder()
        .offset(Offset::VIETNAM)
        .field("service", "booking-api")
        .sink(WriterSink::new(io::sink()).with_format(format))
        .build()
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dl-log-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create bench directory");
    dir
}

fn main() {
    let mut b = Bench::from_args();
    let guest = String::from("Nguyễn Văn An");
    let booking_id = 118_204u64;

    let text = null_logger(TextFormat::new());
    let json = null_logger(JsonFormat::new());
    let clef = null_logger(JsonFormat::clef());

    b.run("disabled: debug! (args + fields)", || {
        debug!(logger: text, "rate for {} is {}", black_box(&guest), black_box(1_250_000); booking_id, guest = %guest);
    });
    let filtered = Logger::builder()
        .filter(dl_log::Filter::parse("info,sqlx=warn,dl_http=debug").unwrap())
        .sink(WriterSink::new(io::sink()))
        .build();
    b.run("disabled: info! by target directive", || {
        info!(logger: filtered, target: "sqlx::query", "query {}", black_box(1); rows = 3);
    });
    b.run("enabled: text -> null sink", || {
        info!(logger: text, "booking confirmed"; booking_id = black_box(booking_id), nights = 3, guest = %guest);
    });
    b.run("enabled: text, format args -> null", || {
        info!(logger: text, "user {} logged in from {}", black_box(&guest), "10.0.0.8"; attempts = 1);
    });
    b.run("enabled: json -> null sink", || {
        info!(logger: json, "booking confirmed"; booking_id = black_box(booking_id), nights = 3, guest = %guest);
    });
    b.run("enabled: clef -> null sink", || {
        info!(logger: clef, "booking confirmed"; booking_id = black_box(booking_id), nights = 3, guest = %guest);
    });
    with_context(
        fields!(
            request_id = "01JD3K8Q2N7",
            user_id = 7,
            path = "/api/bookings"
        ),
        || {
            b.run("enabled: json + 3 context -> null", || {
            info!(logger: json, "booking confirmed"; booking_id = black_box(booking_id), nights = 3);
        });
        },
    );

    let time = OffsetDateTime::parse_rfc3339("2026-11-15T10:20:30.123+07:00").unwrap();
    let record = Record::new(time, Level::Info, "booking", "booking confirmed")
        .with_template("booking confirmed")
        .with_field("booking_id", booking_id)
        .with_field("nights", 3)
        .with_field("guest", guest.as_str());
    let mut buf = String::with_capacity(256);
    for (name, format) in [
        ("format only: text", &TextFormat::new() as &dyn Format),
        ("format only: json", &JsonFormat::new()),
        ("format only: clef", &JsonFormat::clef()),
    ] {
        b.run(name, || {
            buf.clear();
            format.format(black_box(&record), &mut buf);
            buf.len()
        });
    }

    let dir = temp_dir();
    let file_logger = |name: &str, flush: FlushPolicy| {
        let file = FileSink::builder(dir.join(name))
            .flush_policy(flush)
            .format(JsonFormat::new())
            .build()
            .unwrap();
        Logger::builder().sink(file).build()
    };
    let sync_direct = file_logger("sync.log", FlushPolicy::EveryRecord);
    b.run("file: sync, write per record", || {
        info!(logger: sync_direct, "booking confirmed"; booking_id = black_box(booking_id), nights = 3);
    });
    let buffered = FlushPolicy::Buffered {
        flush_level: Level::Error,
        interval: std::time::Duration::from_secs(1),
    };
    let sync_buffered = file_logger("buffered.log", buffered);
    b.run("file: sync, buffered", || {
        info!(logger: sync_buffered, "booking confirmed"; booking_id = black_box(booking_id), nights = 3);
    });
    let async_logger = |name: &str, flush: FlushPolicy, overflow: Overflow| {
        let file = FileSink::builder(dir.join(name))
            .flush_policy(flush)
            .format(JsonFormat::new())
            .build()
            .unwrap();
        let sink = Arc::new(
            AsyncSink::with_options(
                file,
                AsyncOptions {
                    capacity: 8192,
                    overflow,
                },
            )
            .unwrap(),
        );
        (Logger::builder().sink(sink.clone()).build(), sink)
    };
    let (async_direct, _) = async_logger("async.log", FlushPolicy::EveryRecord, Overflow::Block);
    b.run("file: async (block), write per record", || {
        info!(logger: async_direct, "booking confirmed"; booking_id = black_box(booking_id), nights = 3);
    });
    async_direct.shutdown();
    let (async_buffered, _) = async_logger("async-buffered.log", buffered, Overflow::Block);
    b.run("file: async (block), buffered", || {
        info!(logger: async_buffered, "booking confirmed"; booking_id = black_box(booking_id), nights = 3);
    });
    async_buffered.shutdown();
    let (async_drop, sink) = async_logger(
        "async-drop.log",
        FlushPolicy::EveryRecord,
        Overflow::DropNewest,
    );
    b.run("file: async (drop newest), caller cost", || {
        info!(logger: async_drop, "booking confirmed"; booking_id = black_box(booking_id), nights = 3);
    });
    async_drop.shutdown();
    if sink.dropped() > 0 {
        println!("{:<40} {:>12}", "  (records dropped)", sink.dropped());
    }

    drop((
        sync_direct,
        sync_buffered,
        async_direct,
        async_buffered,
        async_drop,
        sink,
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
