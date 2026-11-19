#![doc = include_str!("../README.md")]

mod error;
mod filter;
mod format;
mod level;
mod logger;
mod macros;
mod record;
mod redact;
mod sink;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod async_sink;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod console;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod file;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use filter::Filter;
pub use format::{Format, JsonFormat, JsonKeys, LogfmtFormat, TextFormat};
pub use level::{Level, LevelFilter};
pub use logger::{Logger, LoggerBuilder, flush, init, logger, set_level, shutdown, try_init};
pub use record::{
    Context, ContextGuard, ContextIter, Field, Fields, Key, Record, push_context, with_context,
};
pub use redact::{REDACTED, Redacted, scrub};
pub use sink::{MemorySink, Sink, WriterSink};

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use async_sink::{AsyncOptions, AsyncSink, Overflow};
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use console::ConsoleSink;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use file::{FileSink, FileSinkBuilder, FlushPolicy};

pub use dl_json::{ToJson, Value};
pub use dl_time::{Offset, OffsetDateTime};

#[doc(hidden)]
pub use logger::__private;
