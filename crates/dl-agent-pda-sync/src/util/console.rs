//! Where a verb writes its console lines (`TextWriter` in the C# code): standard output,
//! standard error, or a buffer that tests read back.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

/// Receives whole lines (`TextWriter.WriteLine`) and partial text (`Write`).
pub trait LineWriter: Send + Sync {
    /// Writes `text` without a newline.
    fn write(&self, text: &str);

    /// Writes `text` and a newline (`\n`; the C# `Console.Out.NewLine` is the platform
    /// newline, which the Windows console shows the same).
    fn write_line(&self, text: &str) {
        self.write(text);
        self.write(crate::util::stj::NEWLINE);
    }
}

/// A shared line writer.
pub type SharedLines = Arc<dyn LineWriter>;

/// Standard output.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stdout;

impl LineWriter for Stdout {
    fn write(&self, text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }
}

/// Standard error.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stderr;

impl LineWriter for Stderr {
    fn write(&self, text: &str) {
        let mut out = std::io::stderr().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }
}

/// Keeps everything written (`StringWriter`).
#[derive(Debug, Default)]
pub struct MemoryLines(Mutex<String>);

impl MemoryLines {
    /// A new empty buffer, shared.
    pub fn shared() -> Arc<MemoryLines> {
        Arc::new(MemoryLines::default())
    }

    /// Everything written so far.
    pub fn text(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LineWriter for MemoryLines {
    fn write(&self, text: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_str(text);
    }
}
