//! Per-target level filters with `RUST_LOG`-style directives.

use std::fmt;
use std::str::FromStr;

use crate::{Error, Level, LevelFilter};

/// A default minimum level plus per-target overrides.
///
/// A target matches a directive when it equals the directive's name or continues it
/// after `::` or `.`: `dl_http` matches `dl_http` and `dl_http::server` but not
/// `dl_httpx`; `Microsoft` matches `Microsoft.AspNetCore`. When several directives
/// match, the longest name wins, so `dl_http::db=error` overrides `dl_http=debug` for
/// `dl_http::db::pool`.
///
/// ```
/// use dl_log::{Filter, Level, LevelFilter};
///
/// let filter = Filter::parse("info,dl_http=debug,dl_http::db=error,sqlx=warn")?;
/// assert!(filter.enabled(Level::Debug, "dl_http::server"));
/// assert!(!filter.enabled(Level::Warn, "dl_http::db::pool"));
/// assert!(!filter.enabled(Level::Info, "sqlx::query"));
/// assert!(!filter.enabled(Level::Debug, "dl_httpx"));       // falls back to `info`
/// assert_eq!(filter.level_for("booking"), LevelFilter::Info);
/// assert_eq!(filter.to_string(), "info,dl_http::db=error,dl_http=debug,sqlx=warn");
/// # Ok::<(), dl_log::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Filter {
    default: LevelFilter,
    /// Sorted by name length, longest first, names unique.
    directives: Vec<(String, LevelFilter)>,
}

impl Filter {
    /// The environment variable read by [`from_env`](Self::from_env): `RUST_LOG`.
    pub const ENV_VAR: &'static str = "RUST_LOG";

    /// A filter with only a default level.
    pub fn new(default: impl Into<LevelFilter>) -> Filter {
        Filter {
            default: default.into(),
            directives: Vec::new(),
        }
    }

    /// Parses a comma-separated directive string in the `env_logger` / `RUST_LOG` syntax.
    ///
    /// Each directive is one of:
    ///
    /// - `level`: sets the default level (`info`, `warn`, `off`, any [`Level`] name);
    /// - `target=level`: sets the level for a target and everything below it;
    /// - `target`: enables every level for that target (same as `target=trace`).
    ///
    /// Whitespace around directives, names and levels is ignored, empty directives are
    /// skipped, and a later directive for the same target (or a later default) wins.
    /// The default level is `info` when the string sets none. `env_logger`'s `/regex`
    /// suffix is not supported and is reported as an error.
    ///
    /// # Errors
    ///
    /// [`Error::ParseFilter`] with the byte offset of the bad directive, level or name.
    pub fn parse(directives: &str) -> Result<Filter, Error> {
        let mut filter = Filter::default();
        let mut offset = 0;
        for item in directives.split(',') {
            let item_start = offset;
            offset += item.len() + 1;
            let lead = item.len() - item.trim_start().len();
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let start = item_start + lead;
            let err =
                |position: usize, reason: &'static str| Error::ParseFilter { position, reason };
            if item.contains('/') {
                return Err(err(start, "regex filters ('/') are not supported"));
            }
            match item.split_once('=') {
                Some((name, level)) => {
                    let level_start =
                        start + name.len() + 1 + (level.len() - level.trim_start().len());
                    let name = name.trim();
                    let level = level.trim();
                    if name.is_empty() {
                        return Err(err(start, "missing target name before '='"));
                    }
                    check_name(name).map_err(|r| err(start, r))?;
                    if level.contains('=') {
                        return Err(err(level_start, "more than one '=' in a directive"));
                    }
                    if level.is_empty() {
                        return Err(err(level_start, "missing level after '='"));
                    }
                    let level: LevelFilter = level
                        .parse()
                        .map_err(|_| err(level_start, "unknown level"))?;
                    filter = filter.with_target(name, level);
                }
                None => match item.parse::<LevelFilter>() {
                    Ok(level) => filter.default = level,
                    Err(_) => {
                        check_name(item).map_err(|r| err(start, r))?;
                        filter = filter.with_target(item, LevelFilter::Trace);
                    }
                },
            }
        }
        Ok(filter)
    }

    /// Reads and parses the `RUST_LOG` environment variable. Returns `Ok(None)` when it
    /// is unset, empty or not valid Unicode.
    ///
    /// Not available on `wasm32-unknown-unknown`, which has no environment; use
    /// [`from_env_with`](Self::from_env_with) there.
    ///
    /// # Errors
    ///
    /// [`Error::ParseFilter`] when the variable is set but malformed.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub fn from_env() -> Result<Option<Filter>, Error> {
        Filter::from_env_with(|key| std::env::var(key).ok())
    }

    /// Like [`from_env`](Self::from_env), but reads variables through `lookup`, which
    /// receives the name `RUST_LOG`. Use it to read another variable, to read from a
    /// configuration file, or to inject values in tests (setting process environment
    /// variables is `unsafe` since Rust 2024).
    ///
    /// ```
    /// use dl_log::{Filter, Level};
    ///
    /// let filter = Filter::from_env_with(|key| {
    ///     assert_eq!(key, "RUST_LOG");
    ///     Some("warn,booking=debug".to_string())
    /// })?
    /// .unwrap_or_default();
    /// assert!(filter.enabled(Level::Debug, "booking::api"));
    ///
    /// // Unset: fall back to a default.
    /// let filter = Filter::from_env_with(|_| None)?.unwrap_or(Filter::new(Level::Info));
    /// assert!(!filter.enabled(Level::Debug, "booking"));
    /// # Ok::<(), dl_log::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::ParseFilter`] when the value is malformed.
    pub fn from_env_with(
        lookup: impl FnOnce(&str) -> Option<String>,
    ) -> Result<Option<Filter>, Error> {
        match lookup(Filter::ENV_VAR) {
            Some(value) if !value.trim().is_empty() => Filter::parse(&value).map(Some),
            _ => Ok(None),
        }
    }

    /// Sets the level for `target` (and the targets below it), replacing an existing
    /// directive for the same name.
    pub fn with_target(
        mut self,
        target: impl Into<String>,
        level: impl Into<LevelFilter>,
    ) -> Filter {
        let target = target.into();
        let level = level.into();
        if let Some(entry) = self.directives.iter_mut().find(|(name, _)| *name == target) {
            entry.1 = level;
        } else {
            let at = self
                .directives
                .iter()
                .position(|(name, _)| name.len() < target.len())
                .unwrap_or(self.directives.len());
            self.directives.insert(at, (target, level));
        }
        self
    }

    /// Sets the default level for targets without a directive.
    pub fn with_default(mut self, level: impl Into<LevelFilter>) -> Filter {
        self.default = level.into();
        self
    }

    /// The level for targets without a directive.
    pub fn default_level(&self) -> LevelFilter {
        self.default
    }

    /// The per-target directives, longest name first.
    pub fn directives(&self) -> impl Iterator<Item = (&str, LevelFilter)> + '_ {
        self.directives
            .iter()
            .map(|(name, level)| (name.as_str(), *level))
    }

    /// The minimum level that applies to `target`.
    pub fn level_for(&self, target: &str) -> LevelFilter {
        directive_for(&self.directives, target).unwrap_or(self.default)
    }

    /// Returns `true` when a record at `level` from `target` passes.
    pub fn enabled(&self, level: Level, target: &str) -> bool {
        self.level_for(target).allows(level)
    }

    /// The most verbose level any target can log at.
    pub fn max_level(&self) -> LevelFilter {
        self.directives
            .iter()
            .map(|(_, l)| *l)
            .fold(self.default, Ord::min)
    }

    pub(crate) fn into_parts(self) -> (LevelFilter, Vec<(String, LevelFilter)>) {
        (self.default, self.directives)
    }
}

/// The level of the longest directive matching `target`. `directives` must be sorted
/// longest name first.
pub(crate) fn directive_for(
    directives: &[(String, LevelFilter)],
    target: &str,
) -> Option<LevelFilter> {
    directives
        .iter()
        .find(|(name, _)| target_matches(target, name))
        .map(|(_, level)| *level)
}

fn target_matches(target: &str, name: &str) -> bool {
    match target.strip_prefix(name) {
        Some(rest) => rest.is_empty() || rest.starts_with("::") || rest.starts_with('.'),
        None => false,
    }
}

fn check_name(name: &str) -> Result<(), &'static str> {
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Err("target names cannot contain whitespace")
    } else {
        Ok(())
    }
}

impl FromStr for Filter {
    type Err = Error;

    fn from_str(s: &str) -> Result<Filter, Error> {
        Filter::parse(s)
    }
}

/// Writes the directive string, which [`Filter::parse`] reads back to an equal filter.
impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.default.as_str())?;
        for (name, level) in &self.directives {
            write!(f, ",{name}={level}")?;
        }
        Ok(())
    }
}

impl From<LevelFilter> for Filter {
    fn from(level: LevelFilter) -> Filter {
        Filter::new(level)
    }
}

impl From<Level> for Filter {
    fn from(level: Level) -> Filter {
        Filter::new(level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_logger_syntax() {
        let f = Filter::parse("info,dl_http=debug,sqlx=warn").unwrap();
        assert_eq!(f.default_level(), LevelFilter::Info);
        assert_eq!(f.level_for("dl_http"), LevelFilter::Debug);
        assert_eq!(f.level_for("dl_http::client::pool"), LevelFilter::Debug);
        assert_eq!(f.level_for("sqlx::postgres"), LevelFilter::Warn);
        assert_eq!(f.level_for("my_app"), LevelFilter::Info);
        assert_eq!(f.max_level(), LevelFilter::Debug);

        // Bare target = trace; whitespace and empty items ignored; case-insensitive levels.
        let f = Filter::parse(" WARN , ,my_app , hyper = Off ,").unwrap();
        assert_eq!(f.default_level(), LevelFilter::Warn);
        assert_eq!(f.level_for("my_app::x"), LevelFilter::Trace);
        assert_eq!(f.level_for("hyper"), LevelFilter::Off);
        assert_eq!(f.max_level(), LevelFilter::Trace);

        assert_eq!(Filter::parse("").unwrap(), Filter::new(LevelFilter::Info));
        assert_eq!(Filter::parse("off").unwrap().max_level(), LevelFilter::Off);
        // Later directives win.
        let f = Filter::parse("debug,a=warn,error,a=info").unwrap();
        assert_eq!(f.default_level(), LevelFilter::Error);
        assert_eq!(f.level_for("a"), LevelFilter::Info);
        assert_eq!(f.directives().count(), 1);
    }

    #[test]
    fn longest_prefix_wins_on_segment_boundaries() {
        let f = Filter::parse(
            "warn,dl_http=debug,dl_http::db=error,Microsoft=warning,Microsoft.Hosting=info",
        )
        .unwrap();
        assert_eq!(f.level_for("dl_http::db"), LevelFilter::Error);
        assert_eq!(f.level_for("dl_http::db::pool"), LevelFilter::Error);
        assert_eq!(f.level_for("dl_http::dbx"), LevelFilter::Debug);
        assert_eq!(f.level_for("dl_http::server"), LevelFilter::Debug);
        assert_eq!(f.level_for("dl_httpx"), LevelFilter::Warn);
        assert_eq!(f.level_for("dl_htt"), LevelFilter::Warn);
        assert_eq!(f.level_for("Microsoft.AspNetCore"), LevelFilter::Warn);
        assert_eq!(f.level_for("Microsoft.Hosting.Lifetime"), LevelFilter::Info);
        assert!(f.enabled(Level::Info, "Microsoft.Hosting.Lifetime"));
        assert!(!f.enabled(Level::Info, "Microsoft.AspNetCore.Routing"));
        // Order in the string does not matter.
        let g = Filter::parse(
            "dl_http::db=error,Microsoft.Hosting=info,dl_http=debug,warn,Microsoft=warn",
        )
        .unwrap();
        assert_eq!(f, g);
    }

    #[test]
    fn errors_carry_positions() {
        let cases = [
            ("info,=debug", 5, "missing target name before '='"),
            ("info, a=verbos", 8, "unknown level"),
            ("a=", 2, "missing level after '='"),
            ("a=b=c", 2, "more than one '=' in a directive"),
            ("info,my app", 5, "target names cannot contain whitespace"),
            (
                "my_app=debug/foo",
                0,
                "regex filters ('/') are not supported",
            ),
            ("warn,a=  bogus", 9, "unknown level"),
        ];
        for (input, position, reason) in cases {
            match Filter::parse(input) {
                Err(Error::ParseFilter {
                    position: p,
                    reason: r,
                }) => {
                    assert_eq!((p, r), (position, reason), "{input}");
                }
                other => panic!("{input}: {other:?}"),
            }
        }
    }

    #[test]
    fn display_round_trips() {
        for s in [
            "info",
            "off,a=trace",
            "warn,dl_http::db=error,dl_http=debug",
        ] {
            let f: Filter = s.parse().unwrap();
            assert_eq!(f.to_string(), s);
            assert_eq!(f.to_string().parse::<Filter>().unwrap(), f);
        }
    }

    #[test]
    fn env_lookup_is_injectable() {
        let f =
            Filter::from_env_with(|k| (k == "RUST_LOG").then(|| "debug,sqlx=warn".into())).unwrap();
        assert_eq!(f.unwrap().level_for("sqlx"), LevelFilter::Warn);
        assert_eq!(Filter::from_env_with(|_| None).unwrap(), None);
        assert_eq!(Filter::from_env_with(|_| Some("  ".into())).unwrap(), None);
        assert!(Filter::from_env_with(|_| Some("a=nope".into())).is_err());
    }

    #[test]
    fn builder_methods() {
        let f = Filter::new(Level::Warn)
            .with_target("a", Level::Debug)
            .with_target("a::b", LevelFilter::Off);
        assert_eq!(
            f.directives().collect::<Vec<_>>(),
            [("a::b", LevelFilter::Off), ("a", LevelFilter::Debug)]
        );
        assert_eq!(
            f.clone().with_default(LevelFilter::Trace).default_level(),
            LevelFilter::Trace
        );
        assert_eq!(
            Filter::from(Level::Error).default_level(),
            LevelFilter::Error
        );
    }
}
