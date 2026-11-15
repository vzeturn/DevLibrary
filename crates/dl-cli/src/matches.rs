//! Parse results.

use crate::{Error, ErrorKind};
use core::fmt;
use core::str::FromStr;

/// Where the value of an argument came from.
///
/// `#[non_exhaustive]`: new sources (e.g. a config file) may be added later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValueSource {
    /// Given on the command line.
    CommandLine,
    /// Read from the argument's environment variable.
    Env,
    /// The argument's default value.
    Default,
}

/// One argument in a [`Matches`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedArg {
    pub(crate) name: String,
    pub(crate) display: String,
    pub(crate) env: Option<String>,
    pub(crate) values: Vec<String>,
    /// Argument-list positions of `values`; empty for env and default values.
    pub(crate) indices: Vec<usize>,
    pub(crate) count: u32,
    pub(crate) source: Option<ValueSource>,
}

impl MatchedArg {
    fn origin(&self) -> String {
        match (self.source, &self.env) {
            (Some(ValueSource::Env), Some(var)) => format!(" (from environment variable {var})"),
            (Some(ValueSource::Default), _) => " (default)".into(),
            _ => String::new(),
        }
    }
}

/// The parsed arguments of one command level.
///
/// Every accessor takes the argument *name* given to [`Arg::flag`](crate::Arg::flag),
/// [`Arg::option`](crate::Arg::option) or [`Arg::positional`](crate::Arg::positional).
/// Asking for a name the command does not define is a bug in the program: it panics in
/// debug builds and behaves like an absent argument in release builds.
///
/// [`global`](crate::Arg::global) arguments appear in the matches of the command that
/// defines them and of every subcommand below it, with the same values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Matches {
    pub(crate) args: Vec<MatchedArg>,
    pub(crate) subcommand: Option<Box<(String, Matches)>>,
    /// Usage line and help hint attached to errors from [`Matches::get`].
    pub(crate) footer: Option<String>,
}

impl Matches {
    fn entry(&self, name: &str) -> Option<&MatchedArg> {
        let entry = self.args.iter().find(|a| a.name == name);
        debug_assert!(
            entry.is_some(),
            "dl-cli: no argument named '{name}' in this command"
        );
        entry
    }

    /// True if the argument is present from any source. For a flag: it was given on the
    /// command line or enabled by its environment variable.
    pub fn flag(&self, name: &str) -> bool {
        self.source(name).is_some()
    }

    /// Number of explicit occurrences: how many times the argument was given on the
    /// command line (`-vvv` is 3), or 1 when it came from its environment variable.
    /// Defaults do not count.
    pub fn count(&self, name: &str) -> u32 {
        self.entry(name).map_or(0, |a| a.count)
    }

    /// The first value of an option or positional, from whichever source provided it.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values(name).first().map(String::as_str)
    }

    /// All values in command-line order. An environment variable or default provides a
    /// single value. Empty if the argument is absent or is a flag.
    pub fn values(&self, name: &str) -> &[String] {
        self.entry(name).map_or(&[], |a| a.values.as_slice())
    }

    /// Where the value came from, or `None` if the argument is absent.
    pub fn source(&self, name: &str) -> Option<ValueSource> {
        self.entry(name).and_then(|a| a.source)
    }

    /// The first value converted with [`FromStr`], or `None` if absent.
    ///
    /// A conversion failure is an [`ErrorKind::InvalidValue`] error that names the argument
    /// and includes the `FromStr` error message.
    ///
    /// ```
    /// use dl_cli::{Arg, Command, ErrorKind};
    ///
    /// let cmd = Command::new("app").arg(Arg::option("port"));
    /// let m = cmd.parse_from_with_env(["app", "--port", "80x"], |_| None).unwrap();
    /// let err = m.get::<u16>("port").unwrap_err();
    /// assert_eq!(err.kind(), ErrorKind::InvalidValue);
    /// assert_eq!(err.to_string(), "invalid value '80x' for '--port <PORT>': invalid digit found in string");
    /// ```
    pub fn get<T>(&self, name: &str) -> Result<Option<T>, Error>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        match self.entry(name) {
            Some(arg) if !arg.values.is_empty() => self.convert(arg, 0).map(Some),
            _ => Ok(None),
        }
    }

    /// Every value converted with [`FromStr`]; empty if absent. See [`Matches::get`].
    pub fn get_all<T>(&self, name: &str) -> Result<Vec<T>, Error>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        match self.entry(name) {
            Some(arg) => (0..arg.values.len())
                .map(|i| self.convert(arg, i))
                .collect(),
            None => Ok(Vec::new()),
        }
    }

    /// Like [`Matches::get`], but an absent value is an [`ErrorKind::MissingRequired`] error.
    pub fn required<T>(&self, name: &str) -> Result<T, Error>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        self.get(name)?.ok_or_else(|| {
            let display = self.entry(name).map_or(name, |a| a.display.as_str());
            Error::detailed(
                ErrorKind::MissingRequired,
                format!("missing required argument '{display}'"),
                None,
                None,
            )
            .with_footer(self.footer.clone())
        })
    }

    /// The selected subcommand's name and matches, if one was given.
    pub fn subcommand(&self) -> Option<(&str, &Matches)> {
        self.subcommand
            .as_deref()
            .map(|(name, m)| (name.as_str(), m))
    }

    fn convert<T>(&self, arg: &MatchedArg, i: usize) -> Result<T, Error>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        let raw = arg.values.get(i).map_or("", String::as_str);
        raw.parse().map_err(|e: T::Err| {
            Error::detailed(
                ErrorKind::InvalidValue,
                format!(
                    "invalid value '{raw}' for '{}'{}: {e}",
                    arg.display,
                    arg.origin()
                ),
                arg.indices.get(i).copied(),
                None,
            )
            .with_footer(self.footer.clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{Arg, Command, ErrorKind, ValueSource};

    fn cmd() -> Command {
        Command::new("app")
            .arg(
                Arg::option("port")
                    .short('p')
                    .env("APP_PORT")
                    .default("8080"),
            )
            .arg(Arg::option("level").env("APP_LEVEL"))
            .arg(Arg::option("id").multiple())
            .arg(Arg::flag("quiet"))
    }

    #[test]
    fn typed_access() {
        let m = cmd()
            .parse_from_with_env(["app", "-p", "9000", "--id", "1", "--id=2"], |_| None)
            .unwrap();
        assert_eq!(m.get::<u16>("port").unwrap(), Some(9000));
        assert_eq!(m.required::<u16>("port").unwrap(), 9000);
        assert_eq!(m.get_all::<u32>("id").unwrap(), [1, 2]);
        assert_eq!(m.get::<u8>("level").unwrap(), None);
        assert_eq!(m.get_all::<u8>("level").unwrap(), Vec::<u8>::new());
        assert_eq!(m.value("quiet"), None);
        assert!(m.values("quiet").is_empty());
        assert!(!m.flag("quiet"));
        assert_eq!(m.count("id"), 2);
    }

    #[test]
    fn invalid_value_carries_index_and_origin() {
        let m = cmd()
            .parse_from_with_env(["app", "--id", "1", "--id", "x"], |_| None)
            .unwrap();
        let e = m.get_all::<u32>("id").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidValue);
        assert_eq!(e.index(), Some(4));
        assert_eq!(
            e.to_string(),
            "invalid value 'x' for '--id <ID>': invalid digit found in string"
        );
        assert_eq!(
            e.render(),
            "error: invalid value 'x' for '--id <ID>': invalid digit found in string\n\n\
             Usage: app [OPTIONS]\n\nFor more information, try 'app --help'.\n"
        );

        let env = |name: &str| (name == "APP_PORT").then(|| "http".to_string());
        let m = cmd().parse_from_with_env(["app"], env).unwrap();
        assert_eq!(m.source("port"), Some(ValueSource::Env));
        let e = m.get::<u16>("port").unwrap_err();
        assert_eq!(e.index(), None);
        assert_eq!(
            e.to_string(),
            "invalid value 'http' for '--port <PORT>' (from environment variable APP_PORT): \
             invalid digit found in string"
        );

        let m = Command::new("app")
            .arg(Arg::option("port").default("x"))
            .parse_from_with_env(["app"], |_| None)
            .unwrap();
        let e = m.get::<u16>("port").unwrap_err();
        assert_eq!(
            e.to_string(),
            "invalid value 'x' for '--port <PORT>' (default): invalid digit found in string"
        );
    }

    #[test]
    fn required_missing() {
        let m = cmd().parse_from_with_env(["app"], |_| None).unwrap();
        let e = m.required::<String>("level").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::MissingRequired);
        assert_eq!(e.to_string(), "missing required argument '--level <LEVEL>'");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "no argument named 'prot'")]
    fn unknown_name_panics_in_debug() {
        let m = cmd().parse_from_with_env(["app"], |_| None).unwrap();
        let _ = m.value("prot");
    }
}
