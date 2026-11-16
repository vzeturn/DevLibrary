//! Console input echo, for reading a password typed at a Windows console.
//!
//! ```no_run
//! use std::io::BufRead;
//!
//! eprint!("Password: ");
//! // Not a console (input redirected) or not Windows: read it visibly, or refuse.
//! let echo = dl_winos::console::hide_input().ok();
//! let mut line = String::new();
//! std::io::stdin().lock().read_line(&mut line)?;
//! drop(echo); // echo is back on
//! eprintln!();
//! # Ok::<(), std::io::Error>(())
//! ```

use crate::{Error, sys};

/// Echo is off on standard input while this lives; dropping it restores the previous
/// console mode.
pub struct EchoOff {
    console: sys::ConsoleIn,
    previous: u32,
}

impl core::fmt::Debug for EchoOff {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EchoOff")
            .field("previous_mode", &self.previous)
            .finish()
    }
}

/// Turns echo off on the console attached to standard input (`SetConsoleMode` without
/// `ENABLE_ECHO_INPUT`), so typed characters are not shown. Line input still works:
/// reading stops at Enter as usual.
///
/// # Errors
/// [`Error::Win32`] with operation `GetConsoleMode` when standard input is not a console
/// (redirected from a file or pipe), [`Error::Unsupported`] off Windows. Callers usually
/// continue without hiding in both cases, as there is nothing to hide from.
pub fn hide_input() -> Result<EchoOff, Error> {
    let (console, previous) = sys::console_echo_off()?;
    Ok(EchoOff { console, previous })
}

impl EchoOff {
    /// Restores the previous mode now and reports a failure, which `drop` cannot.
    pub fn restore(self) -> Result<(), Error> {
        let r = sys::console_set_mode(&self.console, self.previous);
        core::mem::forget(self);
        r
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        let _ = sys::console_set_mode(&self.console, self.previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(windows))]
    fn unsupported_off_windows() {
        assert!(matches!(hide_input(), Err(Error::Unsupported)));
    }

    #[test]
    #[cfg(windows)]
    fn redirected_input_is_not_a_console() {
        // Under `cargo test` standard input is either a console (then echo goes off and
        // comes back) or redirected (then GetConsoleMode fails); both must be clean.
        match hide_input() {
            Ok(g) => g.restore().unwrap(),
            Err(e) => assert_eq!(e.operation(), Some("GetConsoleMode"), "{e}"),
        }
    }
}
