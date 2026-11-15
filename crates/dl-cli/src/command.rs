//! Command definitions and parse entry points.

use crate::arg::Arg;
use crate::parser::{NotUnicode, Parser};
use crate::{Error, ErrorKind, Matches, help};
use std::ffi::OsString;

/// A command (or subcommand) definition: name, metadata, arguments and subcommands.
///
/// ```
/// use dl_cli::{Arg, Command};
///
/// let cmd = Command::new("hotel")
///     .version("1.2.0")
///     .about("Hotel PMS tools")
///     .arg(Arg::flag("verbose").short('v').count().global())
///     .subcommand(Command::new("serve").about("Run server").arg(Arg::option("port")));
///
/// let m = cmd.parse_from_with_env(["hotel", "serve", "-v", "--port", "80"], |_| None)?;
/// let (name, serve) = m.subcommand().unwrap();
/// assert_eq!(name, "serve");
/// assert_eq!(serve.value("port"), Some("80"));
/// assert_eq!(m.count("verbose"), 1); // global: visible at every level
/// # Ok::<(), dl_cli::Error>(())
/// ```
#[derive(Debug, Clone)]
pub struct Command {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) about: Option<String>,
    pub(crate) args: Vec<Arg>,
    pub(crate) subcommands: Vec<Command>,
    pub(crate) subcommand_required: bool,
}

impl Command {
    /// A command named `name`. For the top-level command this is the program name shown in
    /// usage lines; `argv[0]` is not used, so output does not depend on how the binary was
    /// invoked.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            about: None,
            args: Vec::new(),
            subcommands: Vec::new(),
            subcommand_required: false,
        }
    }

    /// Sets the version and enables `-V` / `--version`, which print `<name> <version>`.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Sets the description printed below the usage line of the help text. Its first line
    /// is the summary shown in the parent's `Commands:` list. A `\n` starts a new line.
    pub fn about(mut self, about: impl Into<String>) -> Self {
        self.about = Some(about.into());
        self
    }

    /// Adds an argument.
    pub fn arg(mut self, arg: Arg) -> Self {
        self.args.push(arg);
        self
    }

    /// Adds a subcommand. Subcommands can be nested to any depth.
    pub fn subcommand(mut self, command: Command) -> Self {
        self.subcommands.push(command);
        self
    }

    /// Makes it an [`ErrorKind::MissingRequired`] error to invoke this command without one
    /// of its subcommands.
    pub fn subcommand_required(mut self) -> Self {
        self.subcommand_required = true;
        self
    }

    /// The help text that `--help` prints for this command.
    pub fn render_help(&self) -> String {
        help::render(self, &self.name, &[])
    }

    /// Checks the definition and panics with a description of the first mistake.
    ///
    /// Mistakes are bugs in the program, not user errors: duplicate argument names, short
    /// or long names used twice (including by inherited global arguments), invalid names,
    /// a multiple positional that is not last, a required positional after an optional one,
    /// `count()` on a non-flag, a default or choices on a flag, a default outside the
    /// choices, a global positional, duplicate subcommand names.
    ///
    /// This runs at the start of every parse in debug builds and is skipped in release
    /// builds. Call it from a unit test to catch mistakes early:
    ///
    /// ```
    /// # fn cli() -> dl_cli::Command { dl_cli::Command::new("app") }
    /// cli().debug_assert();
    /// ```
    pub fn debug_assert(&self) {
        self.check(&self.name, &[]);
    }

    fn check(&self, path: &str, inherited: &[&Arg]) {
        assert!(
            !self.name.is_empty() && !self.name.starts_with('-'),
            "dl-cli: invalid command name '{}' in '{path}'",
            self.name
        );
        let all: Vec<&Arg> = self.args.iter().chain(inherited.iter().copied()).collect();
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().take(i) {
                assert!(
                    a.name != b.name,
                    "dl-cli: command '{path}': argument '{}' is defined twice",
                    a.name
                );
                if let Some(c) = a.short {
                    assert!(
                        !b.matches_short(c) || a.is_positional(),
                        "dl-cli: command '{path}': short option '-{c}' is used by '{}' and '{}'",
                        b.name,
                        a.name
                    );
                }
                for long in a.longs() {
                    assert!(
                        !b.matches_long(long) || a.is_positional(),
                        "dl-cli: command '{path}': long option '--{long}' is used by '{}' and '{}'",
                        b.name,
                        a.name
                    );
                }
            }
        }
        for arg in &self.args {
            arg.check(path);
        }
        let positionals: Vec<&Arg> = self.args.iter().filter(|a| a.is_positional()).collect();
        for (i, p) in positionals.iter().enumerate() {
            assert!(
                !p.multiple || i + 1 == positionals.len(),
                "dl-cli: command '{path}': only the last positional can be multiple ('{}' is not last)",
                p.name
            );
        }
        for pair in positionals.windows(2) {
            assert!(
                !pair[1].required || pair[0].required,
                "dl-cli: command '{path}': required positional '{}' follows optional '{}'",
                pair[1].name,
                pair[0].name
            );
        }
        for (i, sub) in self.subcommands.iter().enumerate() {
            assert!(
                self.subcommands.iter().take(i).all(|s| s.name != sub.name),
                "dl-cli: command '{path}': subcommand '{}' is defined twice",
                sub.name
            );
        }
        let mut globals: Vec<&Arg> = inherited.to_vec();
        globals.extend(self.args.iter().filter(|a| a.global));
        for sub in &self.subcommands {
            sub.check(&format!("{path} {}", sub.name), &globals);
        }
    }

    /// Parses an argument list whose first item is the program name (it is skipped and
    /// never needs to be valid UTF-8). Environment variables are read from the process
    /// environment.
    ///
    /// Help and version requests come back as [`ErrorKind::DisplayHelp`] /
    /// [`ErrorKind::DisplayVersion`] errors.
    pub fn parse_from<I, T>(&self, args: I) -> Result<Matches, Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        self.parse_with(args, &mut process_env)
    }

    /// Like [`Command::parse_from`], but environment variables are looked up with `env`
    /// instead of the process environment. Use it in tests, or to read settings from
    /// another source.
    pub fn parse_from_with_env<I, T, F>(&self, args: I, mut env: F) -> Result<Matches, Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
        F: FnMut(&str) -> Option<String>,
    {
        self.parse_with(args, &mut |name| Ok(env(name)))
    }

    /// Parses [`std::env::args_os`] with the process environment.
    pub fn parse_env(&self) -> Result<Matches, Error> {
        self.parse_from(std::env::args_os())
    }

    /// Parses [`std::env::args_os`]; on help, version or error, prints it and exits the
    /// process (see [`Error::exit`]).
    pub fn parse_env_or_exit(&self) -> Matches {
        self.parse_env().unwrap_or_else(|e| e.exit())
    }

    pub(crate) fn parse_with<I, T>(
        &self,
        args: I,
        env: &mut dyn FnMut(&str) -> Result<Option<String>, NotUnicode>,
    ) -> Result<Matches, Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        if cfg!(debug_assertions) {
            self.debug_assert();
        }
        let mut tokens = Vec::new();
        for (index, arg) in args.into_iter().enumerate().skip(1) {
            match arg.into().into_string() {
                Ok(token) => tokens.push((index, token)),
                Err(_) => {
                    return Err(Error::detailed(
                        ErrorKind::InvalidUtf8,
                        format!("argument {index} is not valid UTF-8"),
                        Some(index),
                        None,
                    ));
                }
            }
        }
        Parser::new(self).run(tokens, env)
    }
}

fn process_env(name: &str) -> Result<Option<String>, NotUnicode> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(NotUnicode),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The command from the crate specification.
    pub(crate) fn hotel() -> Command {
        Command::new("hotel")
            .version("1.2.0")
            .about("Hotel PMS tools")
            .arg(
                Arg::flag("verbose")
                    .short('v')
                    .long("verbose")
                    .help("More output")
                    .count(),
            )
            .arg(
                Arg::option("port")
                    .short('p')
                    .value_name("PORT")
                    .default("8080")
                    .env("HOTEL_PORT")
                    .help("Listen port"),
            )
            .arg(
                Arg::option("format")
                    .choices(["json", "text"])
                    .default("text"),
            )
            .arg(Arg::option("include").short('I').multiple())
            .arg(Arg::positional("input").required().help("Input file"))
            .arg(Arg::positional("rest").multiple())
            .subcommand(
                Command::new("serve")
                    .about("Run server")
                    .arg(Arg::option("bind").help("Bind address")),
            )
    }

    #[test]
    fn fixture_is_valid() {
        hotel().debug_assert();
    }

    fn check(cmd: Command) {
        cmd.debug_assert();
    }

    #[test]
    #[should_panic(expected = "argument 'port' is defined twice")]
    fn duplicate_name() {
        check(
            Command::new("x")
                .arg(Arg::option("port"))
                .arg(Arg::flag("port").long("p2")),
        );
    }

    #[test]
    #[should_panic(expected = "short option '-p' is used by 'port' and 'pid'")]
    fn duplicate_short() {
        check(
            Command::new("x")
                .arg(Arg::option("port").short('p'))
                .arg(Arg::option("pid").short('p')),
        );
    }

    #[test]
    #[should_panic(expected = "long option '--port' is used by 'port' and 'listen'")]
    fn duplicate_long_via_alias() {
        check(
            Command::new("x")
                .arg(Arg::option("port"))
                .arg(Arg::option("listen").alias("port")),
        );
    }

    #[test]
    #[should_panic(expected = "long option '--verbose' is used by 'loud' and 'verbose'")]
    fn global_conflicts_with_subcommand_arg() {
        check(
            Command::new("x")
                .arg(Arg::flag("verbose").global())
                .subcommand(Command::new("y").arg(Arg::flag("loud").long("verbose"))),
        );
    }

    #[test]
    #[should_panic(expected = "only the last positional can be multiple")]
    fn multiple_positional_not_last() {
        check(
            Command::new("x")
                .arg(Arg::positional("a").multiple())
                .arg(Arg::positional("b")),
        );
    }

    #[test]
    #[should_panic(expected = "required positional 'b' follows optional 'a'")]
    fn required_after_optional() {
        check(
            Command::new("x")
                .arg(Arg::positional("a"))
                .arg(Arg::positional("b").required()),
        );
    }

    #[test]
    #[should_panic(expected = "default 'xml' is not one of the choices")]
    fn default_outside_choices() {
        check(Command::new("x").arg(Arg::option("f").choices(["json"]).default("xml")));
    }

    #[test]
    #[should_panic(expected = "a flag cannot have a default")]
    fn flag_with_default() {
        check(Command::new("x").arg(Arg::flag("f").default("1")));
    }

    #[test]
    #[should_panic(expected = "only flags can count")]
    fn count_on_option() {
        check(Command::new("x").arg(Arg::option("f").count()));
    }

    #[test]
    #[should_panic(expected = "a positional cannot be global")]
    fn global_positional() {
        check(Command::new("x").arg(Arg::positional("f").global()));
    }

    #[test]
    #[should_panic(expected = "a positional cannot have short or long names")]
    fn positional_with_short() {
        check(Command::new("x").arg(Arg::positional("f").short('f')));
    }

    #[test]
    #[should_panic(expected = "invalid long name '--port'")]
    fn long_with_dashes() {
        check(Command::new("x").arg(Arg::option("port").long("--port")));
    }

    #[test]
    #[should_panic(expected = "subcommand 'a' is defined twice")]
    fn duplicate_subcommand() {
        check(
            Command::new("x")
                .subcommand(Command::new("a"))
                .subcommand(Command::new("a")),
        );
    }

    #[test]
    #[should_panic(expected = "invalid command name '-a'")]
    fn dashed_subcommand() {
        check(Command::new("x").subcommand(Command::new("-a")));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "defined twice")]
    fn parse_checks_definition_in_debug() {
        let cmd = Command::new("x")
            .arg(Arg::flag("a"))
            .arg(Arg::flag("a").long("b"));
        let _ = cmd.parse_from_with_env(["x"], |_| None);
    }
}
