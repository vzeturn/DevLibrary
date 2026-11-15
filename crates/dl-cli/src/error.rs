//! The error type.

use core::fmt;
use std::io::Write;

/// The category of an [`Error`].
///
/// The enum is `#[non_exhaustive]` so that new kinds can be added without a breaking
/// change; match it with a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// An option (`--name` or `-n`) that the command does not define.
    UnknownOption,
    /// A command name that matches no subcommand.
    UnknownSubcommand,
    /// An option that takes a value was the last argument.
    MissingValue,
    /// A required argument or subcommand was not provided by any source.
    MissingRequired,
    /// A value could not be converted to the requested type (see
    /// [`Matches::get`](crate::Matches::get)), or a flag's environment variable is not a
    /// boolean.
    InvalidValue,
    /// A value is not one of the allowed [`choices`](crate::Arg::choices).
    InvalidChoice,
    /// A flag was given a value, e.g. `--verbose=yes`.
    UnexpectedValue,
    /// More positional arguments than the command accepts.
    UnexpectedPositional,
    /// A flag or option that is not [`multiple`](crate::Arg::multiple) was given twice.
    DuplicateOption,
    /// A command-line argument or environment variable is not valid UTF-8.
    InvalidUtf8,
    /// `-h` / `--help` was given. The message is the help text.
    DisplayHelp,
    /// `-V` / `--version` was given. The message is the version line.
    DisplayVersion,
}

/// A parse error, or a request to display help or version text.
///
/// [`Error::exit`] prints it the conventional way and ends the process. `Display` gives the
/// one-line message (or the help/version text); [`Error::render`] gives the full output
/// including the usage line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    index: Option<usize>,
    suggestion: Option<String>,
    footer: Option<String>,
}

impl Error {
    /// Creates an error with a custom message, e.g. for validation done after parsing.
    ///
    /// ```
    /// use dl_cli::{Error, ErrorKind};
    ///
    /// let e = Error::new(ErrorKind::InvalidValue, "port must be at least 1024");
    /// assert_eq!(e.to_string(), "port must be at least 1024");
    /// assert_eq!(e.exit_code(), 2);
    /// ```
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            index: None,
            suggestion: None,
            footer: None,
        }
    }

    pub(crate) fn detailed(
        kind: ErrorKind,
        message: String,
        index: Option<usize>,
        suggestion: Option<String>,
    ) -> Self {
        Self {
            kind,
            message,
            index,
            suggestion,
            footer: None,
        }
    }

    pub(crate) fn with_footer(mut self, footer: Option<String>) -> Self {
        if self.footer.is_none() {
            self.footer = footer;
        }
        self
    }

    /// The category of the error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Position of the offending argument in the argument list, counting the program name
    /// as 0. `None` when the problem is not tied to one argument (missing arguments, values
    /// from the environment or a default).
    pub fn index(&self) -> Option<usize> {
        self.index
    }

    /// The closest valid spelling, e.g. `--port` for `--prot`, if one is within two edits.
    pub fn suggestion(&self) -> Option<&str> {
        self.suggestion.as_deref()
    }

    /// The process exit status: 0 for [`ErrorKind::DisplayHelp`] and
    /// [`ErrorKind::DisplayVersion`], 2 for everything else.
    pub fn exit_code(&self) -> i32 {
        match self.kind {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => 0,
            _ => 2,
        }
    }

    /// The full text that [`Error::exit`] prints.
    ///
    /// Help and version text as is; otherwise `error: <message>`, followed by the usage line
    /// and a `--help` hint when the error came from parsing.
    pub fn render(&self) -> String {
        if self.exit_code() == 0 {
            return self.message.clone();
        }
        let mut out = format!("error: {self}\n");
        if let Some(footer) = &self.footer {
            out.push('\n');
            out.push_str(footer);
            out.push('\n');
        }
        out
    }

    /// Prints [`Error::render`] (help and version to stdout, errors to stderr) and exits
    /// with [`Error::exit_code`].
    ///
    /// Write failures (e.g. a closed pipe) are ignored. On `wasm32-unknown-unknown` there is
    /// no process to exit; use [`Error::render`] there instead.
    pub fn exit(&self) -> ! {
        let text = self.render();
        let code = self.exit_code();
        if code == 0 {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
        } else {
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(text.as_bytes());
            let _ = err.flush();
        }
        std::process::exit(code)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(suggestion) = &self.suggestion {
            write!(f, "; did you mean '{suggestion}'?")?;
        }
        Ok(())
    }
}

impl core::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_render() {
        let e = Error::detailed(
            ErrorKind::UnknownOption,
            "unknown option '--prot'".into(),
            Some(1),
            Some("--port".into()),
        )
        .with_footer(Some("Usage: hotel [OPTIONS]".into()));
        assert_eq!(
            e.to_string(),
            "unknown option '--prot'; did you mean '--port'?"
        );
        assert_eq!(
            e.render(),
            "error: unknown option '--prot'; did you mean '--port'?\n\nUsage: hotel [OPTIONS]\n"
        );
        assert_eq!(e.kind(), ErrorKind::UnknownOption);
        assert_eq!(e.index(), Some(1));
        assert_eq!(e.suggestion(), Some("--port"));
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn help_renders_verbatim() {
        let e =
            Error::new(ErrorKind::DisplayHelp, "Usage: x\n").with_footer(Some("ignored".into()));
        assert_eq!(e.render(), "Usage: x\n");
        assert_eq!(e.exit_code(), 0);
        assert_eq!(
            Error::new(ErrorKind::DisplayVersion, "x 1\n").exit_code(),
            0
        );
    }

    #[test]
    fn exit_codes() {
        use ErrorKind::*;
        for kind in [
            UnknownOption,
            UnknownSubcommand,
            MissingValue,
            MissingRequired,
            InvalidValue,
            InvalidChoice,
            UnexpectedValue,
            UnexpectedPositional,
            DuplicateOption,
            InvalidUtf8,
        ] {
            assert_eq!(Error::new(kind, "x").exit_code(), 2, "{kind:?}");
        }
        for kind in [DisplayHelp, DisplayVersion] {
            assert_eq!(Error::new(kind, "x").exit_code(), 0, "{kind:?}");
        }
    }

    /// Child half of [`exit_writes_streams_and_code`]; does nothing unless spawned by it.
    #[test]
    fn exit_child() {
        match std::env::var("DL_CLI_EXIT_CHILD").as_deref() {
            Ok("help") => Error::new(ErrorKind::DisplayHelp, "HELP-TEXT\n").exit(),
            Ok("error") => Error::new(ErrorKind::UnknownOption, "ERROR-TEXT").exit(),
            _ => {}
        }
    }

    /// Runs this test binary again (the environment is set on the child only, so no
    /// `unsafe` `set_var`) and checks the stream and status that `exit` uses.
    #[test]
    fn exit_writes_streams_and_code() {
        let exe = std::env::current_exe().unwrap();
        let run = |mode: &str| {
            std::process::Command::new(&exe)
                .args(["--exact", "error::tests::exit_child", "--nocapture"])
                .env("DL_CLI_EXIT_CHILD", mode)
                .output()
                .unwrap()
        };
        let out = run("help");
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
        assert!(stdout.contains("HELP-TEXT\n") && !stderr.contains("HELP-TEXT"));

        let out = run("error");
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(out.status.code(), Some(2), "{stdout}{stderr}");
        assert!(stderr.contains("error: ERROR-TEXT\n") && !stdout.contains("ERROR-TEXT"));
    }

    #[test]
    fn custom_error_has_no_footer() {
        let e = Error::new(ErrorKind::InvalidValue, "bad");
        assert_eq!(e.render(), "error: bad\n");
        let boxed: Box<dyn core::error::Error> = Box::new(e);
        assert_eq!(boxed.to_string(), "bad");
    }
}
