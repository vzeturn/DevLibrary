//! Standard output and standard error.

use std::fmt;
use std::io::{self, Write};

use crate::sink::with_line;
use crate::{Format, Level, Record, Sink, TextFormat};

/// Writes lines to standard output and/or standard error.
///
/// Each line is written with a single `write_all` on the locked stream, so lines from
/// different threads do not interleave. By default every level goes to the chosen
/// stream; [`stderr_from`](Self::stderr_from) splits by level, like
/// Microsoft.Extensions.Logging's `LogToStandardErrorThreshold`.
///
/// ```no_run
/// use dl_log::{ConsoleSink, JsonFormat, Level, Logger, TextFormat};
/// use std::io::IsTerminal;
///
/// // Colors only when stdout is a terminal; warnings and errors on stderr.
/// let console = ConsoleSink::stdout()
///     .with_format(TextFormat::new().with_ansi(std::io::stdout().is_terminal()))
///     .stderr_from(Level::Warn);
/// // In a container: JSON lines on stdout for the log collector.
/// let container = ConsoleSink::stdout().with_format(JsonFormat::new());
/// # let _ = (console, container);
/// ```
///
/// Not available on `wasm32-unknown-unknown`.
pub struct ConsoleSink {
    format: Box<dyn Format>,
    /// Levels at or above this go to stderr; `None` sends everything to stdout.
    stderr_from: Option<Level>,
}

impl ConsoleSink {
    /// Everything to standard output, with [`TextFormat::new`].
    pub fn stdout() -> ConsoleSink {
        ConsoleSink {
            format: Box::new(TextFormat::new()),
            stderr_from: None,
        }
    }

    /// Everything to standard error (the `env_logger` default), with [`TextFormat::new`].
    pub fn stderr() -> ConsoleSink {
        ConsoleSink {
            format: Box::new(TextFormat::new()),
            stderr_from: Some(Level::Trace),
        }
    }

    /// Sends records at `level` and above to standard error and the rest to standard
    /// output.
    pub fn stderr_from(mut self, level: Level) -> ConsoleSink {
        self.stderr_from = Some(level);
        self
    }

    /// Uses `format` for lines.
    pub fn with_format(mut self, format: impl Format + 'static) -> ConsoleSink {
        self.format = Box::new(format);
        self
    }

    /// Returns `true` when records at `level` go to standard error.
    pub fn uses_stderr(&self, level: Level) -> bool {
        self.stderr_from.is_some_and(|from| level >= from)
    }
}

impl Sink for ConsoleSink {
    fn write(&self, record: &Record) -> io::Result<()> {
        let to_stderr = self.uses_stderr(record.level);
        with_line(&*self.format, record, |line| {
            if to_stderr {
                io::stderr().lock().write_all(line)
            } else {
                io::stdout().lock().write_all(line)
            }
        })
    }

    fn flush(&self) -> io::Result<()> {
        let out = io::stdout().lock().flush();
        let err = io::stderr().lock().flush();
        out.and(err)
    }

    fn name(&self) -> &str {
        "console"
    }
}

impl fmt::Debug for ConsoleSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsoleSink")
            .field("stderr_from", &self.stderr_from)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_selection() {
        let out = ConsoleSink::stdout();
        assert!(Level::ALL.iter().all(|&l| !out.uses_stderr(l)));
        let err = ConsoleSink::stderr();
        assert!(Level::ALL.iter().all(|&l| err.uses_stderr(l)));
        let split = ConsoleSink::stdout().stderr_from(Level::Warn);
        assert!(!split.uses_stderr(Level::Info));
        assert!(split.uses_stderr(Level::Warn) && split.uses_stderr(Level::Fatal));
        assert_eq!(split.name(), "console");
        assert!(split.flush().is_ok());
        assert!(format!("{split:?}").contains("Warn"));
    }
}
