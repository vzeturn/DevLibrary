//! Argument definitions.

/// The three kinds of argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgKind {
    /// A switch without a value: `--verbose`, `-v`.
    Flag,
    /// A named argument that takes a value: `--port 8080`.
    Option,
    /// An argument identified by its position.
    Positional,
}

/// Definition of one command-line argument: a flag, an option that takes a value, or a
/// positional argument.
///
/// Create one with [`Arg::flag`], [`Arg::option`] or [`Arg::positional`] and refine it with
/// the chained setters. The `name` is the key used to look the value up in
/// [`Matches`](crate::Matches).
///
/// ```
/// use dl_cli::Arg;
///
/// let port = Arg::option("port").short('p').value_name("PORT").default("8080").help("Listen port");
/// let verbose = Arg::flag("verbose").short('v').count();
/// let input = Arg::positional("input").required();
/// # let _ = (port, verbose, input);
/// ```
#[derive(Debug, Clone)]
pub struct Arg {
    pub(crate) name: String,
    pub(crate) kind: ArgKind,
    pub(crate) short: Option<char>,
    pub(crate) long: Option<String>,
    pub(crate) aliases: Vec<String>,
    pub(crate) help: Option<String>,
    pub(crate) value_name: Option<String>,
    pub(crate) default: Option<String>,
    pub(crate) env: Option<String>,
    pub(crate) choices: Vec<String>,
    pub(crate) required: bool,
    pub(crate) multiple: bool,
    pub(crate) count: bool,
    pub(crate) hidden: bool,
    pub(crate) global: bool,
}

impl Arg {
    fn new(name: String, kind: ArgKind) -> Self {
        let long = (kind != ArgKind::Positional).then(|| name.clone());
        Self {
            name,
            kind,
            short: None,
            long,
            aliases: Vec::new(),
            help: None,
            value_name: None,
            default: None,
            env: None,
            choices: Vec::new(),
            required: false,
            multiple: false,
            count: false,
            hidden: false,
            global: false,
        }
    }

    /// A flag: a switch that takes no value. Its long name defaults to `--<name>`.
    ///
    /// Read it with [`Matches::flag`](crate::Matches::flag) or
    /// [`Matches::count`](crate::Matches::count).
    pub fn flag(name: impl Into<String>) -> Self {
        Self::new(name.into(), ArgKind::Flag)
    }

    /// An option that takes a value. Its long name defaults to `--<name>`.
    ///
    /// The value can be given as `--name value`, `--name=value`, `-n value`, `-nvalue` or
    /// `-n=value`.
    pub fn option(name: impl Into<String>) -> Self {
        Self::new(name.into(), ArgKind::Option)
    }

    /// A positional argument. Positionals are filled in definition order.
    pub fn positional(name: impl Into<String>) -> Self {
        Self::new(name.into(), ArgKind::Positional)
    }

    /// Sets the short name, used as `-c`. Not valid on positionals.
    pub fn short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    /// Replaces the long name (default: the argument name). Pass it without the dashes.
    pub fn long(mut self, long: impl Into<String>) -> Self {
        self.long = Some(long.into());
        self
    }

    /// Adds another long name that is accepted on the command line. Pass it without the
    /// dashes. Aliases are listed in the help text.
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Sets the description shown in the help text. A `\n` starts a new line.
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Sets the placeholder shown in help and errors, e.g. `PORT` in `--port <PORT>`.
    ///
    /// Defaults to the upper-cased name (with `-` replaced by `_`) for options and to the
    /// name for positionals. Not valid on flags.
    pub fn value_name(mut self, value_name: impl Into<String>) -> Self {
        self.value_name = Some(value_name.into());
        self
    }

    /// Sets the value used when neither the command line nor the environment provides one.
    /// Not valid on flags.
    pub fn default(mut self, value: impl Into<String>) -> Self {
        self.default = Some(value.into());
        self
    }

    /// Reads the value from this environment variable when it is not given on the command
    /// line. Empty variables are ignored.
    ///
    /// For flags the variable must be one of `1`, `0`, `true`, `false`, `yes`, `no`, `on`,
    /// `off` (any case); a true value counts as one occurrence.
    pub fn env(mut self, name: impl Into<String>) -> Self {
        self.env = Some(name.into());
        self
    }

    /// Restricts the value to one of `values`. Others are rejected with
    /// [`ErrorKind::InvalidChoice`](crate::ErrorKind::InvalidChoice). Not valid on flags.
    pub fn choices<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = values.into_iter().map(Into::into).collect();
        self
    }

    /// Makes the argument mandatory. The check runs after environment variables and the
    /// default are applied, so a required argument with a default is always satisfied.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Allows the argument more than once and collects every value in order.
    ///
    /// Without this, a repeated flag or option is an
    /// [`ErrorKind::DuplicateOption`](crate::ErrorKind::DuplicateOption) error. On a
    /// positional it takes all remaining positional values; only the last positional can be
    /// multiple.
    pub fn multiple(mut self) -> Self {
        self.multiple = true;
        self
    }

    /// Makes a flag countable (`-vvv` gives 3). Implies [`Arg::multiple`]. Only valid on flags.
    pub fn count(mut self) -> Self {
        self.count = true;
        self.multiple = true;
        self
    }

    /// Hides the argument from help, usage and suggestions. It is still accepted.
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Makes a flag or option accepted by every subcommand below the command that defines
    /// it, and visible in their [`Matches`](crate::Matches). Not valid on positionals.
    pub fn global(mut self) -> Self {
        self.global = true;
        self
    }

    pub(crate) fn is_positional(&self) -> bool {
        self.kind == ArgKind::Positional
    }

    /// The long name and the aliases.
    pub(crate) fn longs(&self) -> impl Iterator<Item = &str> {
        self.long.iter().chain(&self.aliases).map(String::as_str)
    }

    pub(crate) fn matches_long(&self, name: &str) -> bool {
        !self.is_positional() && self.longs().any(|l| l == name)
    }

    pub(crate) fn matches_short(&self, c: char) -> bool {
        !self.is_positional() && self.short == Some(c)
    }

    /// The value placeholder without brackets.
    pub(crate) fn placeholder(&self) -> String {
        match (&self.value_name, self.kind) {
            (Some(v), _) => v.clone(),
            (None, ArgKind::Positional) => self.name.clone(),
            (None, _) => self.name.to_uppercase().replace('-', "_"),
        }
    }

    /// How the argument is named in error messages and in the usage line:
    /// `--verbose`, `--port <PORT>`, `<input>`.
    pub(crate) fn display(&self) -> String {
        let switch = match (&self.long, self.short) {
            (Some(long), _) => format!("--{long}"),
            (None, Some(short)) => format!("-{short}"),
            (None, None) => String::new(),
        };
        match self.kind {
            ArgKind::Flag => switch,
            ArgKind::Option => format!("{switch} <{}>", self.placeholder()),
            ArgKind::Positional => format!("<{}>", self.placeholder()),
        }
    }

    /// The positional as shown in the usage line: `<input>`, `[rest]...`.
    pub(crate) fn usage_token(&self) -> String {
        let (open, close) = if self.required {
            ('<', '>')
        } else {
            ('[', ']')
        };
        let dots = if self.multiple { "..." } else { "" };
        format!("{open}{}{close}{dots}", self.placeholder())
    }

    /// Panics if the definition is inconsistent. See [`Command::debug_assert`](crate::Command::debug_assert).
    pub(crate) fn check(&self, path: &str) {
        let name = &self.name;
        let fail = |what: &str| format!("dl-cli: command '{path}', argument '{name}': {what}");
        assert!(
            !name.is_empty(),
            "{}",
            fail("argument names must not be empty")
        );
        if let Some(c) = self.short {
            assert!(
                c != '-' && c != '=' && !c.is_whitespace(),
                "{}",
                fail("invalid short name")
            );
        }
        for long in self.longs() {
            assert!(
                !long.is_empty() && !long.starts_with('-') && !long.contains('='),
                "{}",
                fail(&format!(
                    "invalid long name '{long}' (give it without dashes)"
                ))
            );
        }
        if let Some(var) = &self.env {
            assert!(
                !var.is_empty() && !var.contains(['=', '\0']),
                "{}",
                fail("invalid environment variable name")
            );
        }
        match self.kind {
            ArgKind::Flag => {
                assert!(
                    self.default.is_none(),
                    "{}",
                    fail("a flag cannot have a default")
                );
                assert!(
                    self.choices.is_empty(),
                    "{}",
                    fail("a flag cannot have choices")
                );
                assert!(
                    self.value_name.is_none(),
                    "{}",
                    fail("a flag cannot have a value name")
                );
            }
            ArgKind::Option => {
                assert!(!self.count, "{}", fail("only flags can count"));
            }
            ArgKind::Positional => {
                assert!(
                    self.short.is_none() && self.long.is_none() && self.aliases.is_empty(),
                    "{}",
                    fail("a positional cannot have short or long names")
                );
                assert!(!self.global, "{}", fail("a positional cannot be global"));
                assert!(!self.count, "{}", fail("only flags can count"));
            }
        }
        if let Some(default) = &self.default {
            assert!(
                self.choices.is_empty() || self.choices.contains(default),
                "{}",
                fail(&format!("default '{default}' is not one of the choices"))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_defaults_to_name() {
        assert_eq!(Arg::flag("dry-run").long.as_deref(), Some("dry-run"));
        assert_eq!(
            Arg::option("port").long("listen").long.as_deref(),
            Some("listen")
        );
        assert_eq!(Arg::positional("input").long, None);
    }

    #[test]
    fn display_forms() {
        assert_eq!(Arg::flag("verbose").display(), "--verbose");
        assert_eq!(Arg::option("dry-run").display(), "--dry-run <DRY_RUN>");
        assert_eq!(Arg::option("port").value_name("N").display(), "--port <N>");
        assert_eq!(Arg::positional("input").display(), "<input>");
        assert_eq!(Arg::positional("input").required().usage_token(), "<input>");
        assert_eq!(
            Arg::positional("rest").multiple().usage_token(),
            "[rest]..."
        );
    }

    #[test]
    fn count_implies_multiple() {
        let a = Arg::flag("v").count();
        assert!(a.count && a.multiple);
    }
}
