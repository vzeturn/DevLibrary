//! The argument-list parser.
//!
//! Arguments of every command level get a numeric id (an index into `defs` and `states`).
//! A level's `args` lists its own ids followed by the ids of global arguments inherited
//! from its ancestors, so a global argument has a single state shared by every level.

use crate::arg::{Arg, ArgKind};
use crate::matches::{MatchedArg, ValueSource};
use crate::suggest::suggest;
use crate::{Command, Error, ErrorKind, Matches, help};
use core::ops::Range;

/// An environment variable exists but is not valid UTF-8.
pub(crate) struct NotUnicode;

type EnvLookup<'e> = dyn FnMut(&str) -> Result<Option<String>, NotUnicode> + 'e;

/// One command on the path from the root to the selected subcommand.
struct Level<'a> {
    cmd: &'a Command,
    /// Space-separated command names from the root, e.g. `hotel serve`.
    path: String,
    /// Ids of the command's own arguments.
    own: Range<usize>,
    /// Own ids followed by inherited global ids.
    args: Vec<usize>,
    /// Ids of the own positionals, in order.
    positionals: Vec<usize>,
    /// Index into `positionals` of the next slot to fill.
    next_positional: usize,
}

#[derive(Debug, Clone, Default)]
struct State {
    values: Vec<String>,
    indices: Vec<usize>,
    count: u32,
    source: Option<ValueSource>,
}

pub(crate) struct Parser<'a> {
    defs: Vec<&'a Arg>,
    states: Vec<State>,
    root: Level<'a>,
    /// Selected subcommands, outermost first.
    subs: Vec<Level<'a>>,
}

fn make_level<'a>(
    defs: &mut Vec<&'a Arg>,
    states: &mut Vec<State>,
    cmd: &'a Command,
    path: String,
    inherited: Vec<usize>,
) -> Level<'a> {
    let start = defs.len();
    for arg in &cmd.args {
        defs.push(arg);
        states.push(State::default());
    }
    let own = start..defs.len();
    let positionals = own.clone().filter(|&id| defs[id].is_positional()).collect();
    let mut args: Vec<usize> = own.clone().collect();
    args.extend(inherited);
    Level {
        cmd,
        path,
        own,
        args,
        positionals,
        next_positional: 0,
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn invalid_choice(def: &Arg, value: &str, index: Option<usize>, origin: &str) -> Error {
    Error::detailed(
        ErrorKind::InvalidChoice,
        format!(
            "invalid value '{value}' for '{}'{origin} [possible values: {}]",
            def.display(),
            def.choices.join(", ")
        ),
        index,
        suggest(value, def.choices.iter().map(String::as_str)).map(str::to_owned),
    )
}

impl<'a> Parser<'a> {
    pub(crate) fn new(cmd: &'a Command) -> Self {
        let mut defs = Vec::new();
        let mut states = Vec::new();
        let root = make_level(&mut defs, &mut states, cmd, cmd.name.clone(), Vec::new());
        Self {
            defs,
            states,
            root,
            subs: Vec::new(),
        }
    }

    /// Parses `tokens` (argument-list index, text) and applies env, defaults and checks.
    pub(crate) fn run(
        mut self,
        tokens: Vec<(usize, String)>,
        env: &mut EnvLookup<'_>,
    ) -> Result<Matches, Error> {
        if let Err(e) = self.consume(tokens) {
            return Err(self.attach(self.level(), e));
        }
        for depth in 0..=self.subs.len() {
            if let Err(e) = self.complete(depth, env) {
                return Err(self.attach(self.level_at(depth), e));
            }
        }
        let leaf = self.level();
        if leaf.cmd.subcommand_required && !leaf.cmd.subcommands.is_empty() {
            let e = Error::detailed(
                ErrorKind::MissingRequired,
                format!("'{}' requires a subcommand", leaf.path),
                None,
                None,
            );
            return Err(self.attach(leaf, e));
        }
        Ok(self.into_matches())
    }

    fn level(&self) -> &Level<'a> {
        self.subs.last().unwrap_or(&self.root)
    }

    fn level_mut(&mut self) -> &mut Level<'a> {
        match self.subs.last_mut() {
            Some(level) => level,
            None => &mut self.root,
        }
    }

    /// Level `depth` on the path; 0 is the root.
    fn level_at(&self, depth: usize) -> &Level<'a> {
        depth
            .checked_sub(1)
            .and_then(|i| self.subs.get(i))
            .unwrap_or(&self.root)
    }

    fn inherited(&self, level: &Level<'a>) -> Vec<&'a Arg> {
        level
            .args
            .iter()
            .skip(level.own.len())
            .map(|&id| self.defs[id])
            .collect()
    }

    /// Adds the usage line of `level` to a parse error.
    fn attach(&self, level: &Level<'a>, error: Error) -> Error {
        if error.exit_code() == 0 {
            return error;
        }
        error.with_footer(Some(help::footer(
            level.cmd,
            &level.path,
            &self.inherited(level),
        )))
    }

    /// The id of the current level's argument matching `pred`.
    fn find(&self, pred: impl Fn(&Arg) -> bool) -> Option<usize> {
        self.level()
            .args
            .iter()
            .copied()
            .find(|&id| pred(self.defs[id]))
    }

    /// Visible long names accepted by the current level, for suggestions.
    fn long_names(&self) -> Vec<&'a str> {
        let level = self.level();
        let mut names: Vec<&'a str> = Vec::new();
        for &id in &level.args {
            let def: &'a Arg = self.defs[id];
            if !def.hidden && !def.is_positional() {
                names.extend(def.longs());
            }
        }
        names.push("help");
        if level.cmd.version.is_some() {
            names.push("version");
        }
        names
    }

    fn help(&self) -> Error {
        let level = self.level();
        let text = help::render(level.cmd, &level.path, &self.inherited(level));
        Error::detailed(ErrorKind::DisplayHelp, text, None, None)
    }

    fn version(&self) -> Error {
        let level = self.level();
        Error::detailed(
            ErrorKind::DisplayVersion,
            help::version(level.cmd, &level.path),
            None,
            None,
        )
    }

    fn consume(&mut self, tokens: Vec<(usize, String)>) -> Result<(), Error> {
        let mut tokens = tokens.into_iter();
        let mut only_positional = false;
        while let Some((index, token)) = tokens.next() {
            if only_positional {
                self.positional(index, token, false)?;
            } else if token == "--" {
                only_positional = true;
            } else if let Some(body) = token.strip_prefix("--") {
                self.long(index, body, &mut tokens)?;
            } else if let Some(cluster) = token
                .strip_prefix('-')
                .filter(|c| !c.is_empty() && !self.is_negative_number(c))
            {
                self.shorts(index, cluster, &mut tokens)?;
            } else if let Some(sub) = self
                .level()
                .cmd
                .subcommands
                .iter()
                .find(|c| c.name == token)
            {
                self.enter(sub);
            } else {
                self.positional(index, token, true)?;
            }
        }
        Ok(())
    }

    /// `digits` is the token without its leading `-`.
    fn is_negative_number(&self, digits: &str) -> bool {
        digits.starts_with(|c: char| c.is_ascii_digit())
            && digits.parse::<f64>().is_ok()
            && self
                .find(|a| a.short.is_some_and(|c| c.is_ascii_digit()) && !a.is_positional())
                .is_none()
    }

    fn enter(&mut self, sub: &'a Command) {
        let parent = self.level();
        let path = format!("{} {}", parent.path, sub.name);
        let inherited = parent
            .args
            .iter()
            .copied()
            .filter(|&id| self.defs[id].global)
            .collect();
        let level = make_level(&mut self.defs, &mut self.states, sub, path, inherited);
        self.subs.push(level);
    }

    /// `--name`, `--name=value`, `--name value`. `body` is the token without `--`.
    fn long(
        &mut self,
        index: usize,
        body: &str,
        rest: &mut impl Iterator<Item = (usize, String)>,
    ) -> Result<(), Error> {
        let (name, inline) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (body, None),
        };
        let Some(id) = self.find(|a| a.matches_long(name)) else {
            if name == "help" {
                return Err(self.help());
            }
            if name == "version" && self.level().cmd.version.is_some() {
                return Err(self.version());
            }
            let suggestion = suggest(name, self.long_names()).map(|s| format!("--{s}"));
            return Err(Error::detailed(
                ErrorKind::UnknownOption,
                format!("unknown option '--{name}'"),
                Some(index),
                suggestion,
            ));
        };
        self.check_duplicate(id, index)?;
        if self.defs[id].kind == ArgKind::Flag {
            if let Some(value) = inline {
                return Err(self.unexpected_value(id, value, index));
            }
            self.add_flag(id);
            return Ok(());
        }
        let (value, value_index) = match inline {
            Some(value) => (value.to_owned(), index),
            None => match rest.next() {
                Some((i, value)) => (value, i),
                None => return Err(self.missing_value(id, index)),
            },
        };
        self.add_value(id, value, value_index)
    }

    /// `-a`, `-abc`, `-p value`, `-pvalue`, `-p=value`, `-vp8080`. `cluster` is the token
    /// without `-`.
    fn shorts(
        &mut self,
        index: usize,
        cluster: &str,
        rest: &mut impl Iterator<Item = (usize, String)>,
    ) -> Result<(), Error> {
        for (pos, c) in cluster.char_indices() {
            let Some(id) = self.find(|a| a.matches_short(c)) else {
                if c == 'h' {
                    return Err(self.help());
                }
                if c == 'V' && self.level().cmd.version.is_some() {
                    return Err(self.version());
                }
                // `-format` was probably meant as `--format`.
                let suggestion = (pos == 0 && self.long_names().contains(&cluster))
                    .then(|| format!("--{cluster}"));
                return Err(Error::detailed(
                    ErrorKind::UnknownOption,
                    format!("unknown option '-{c}'"),
                    Some(index),
                    suggestion,
                ));
            };
            self.check_duplicate(id, index)?;
            let after = &cluster[pos + c.len_utf8()..];
            if self.defs[id].kind == ArgKind::Flag {
                if let Some(value) = after.strip_prefix('=') {
                    return Err(self.unexpected_value(id, value, index));
                }
                self.add_flag(id);
                continue;
            }
            let (value, value_index) = if after.is_empty() {
                match rest.next() {
                    Some((i, value)) => (value, i),
                    None => return Err(self.missing_value(id, index)),
                }
            } else {
                (after.strip_prefix('=').unwrap_or(after).to_owned(), index)
            };
            return self.add_value(id, value, value_index);
        }
        Ok(())
    }

    /// A positional value, or an unknown subcommand when no slot is left.
    fn positional(
        &mut self,
        index: usize,
        token: String,
        may_be_command: bool,
    ) -> Result<(), Error> {
        let level = self.level();
        if let Some(&id) = level.positionals.get(level.next_positional) {
            if !self.defs[id].multiple {
                self.level_mut().next_positional += 1;
            }
            return self.add_value(id, token, index);
        }
        let cmd = self.level().cmd;
        if may_be_command && !cmd.subcommands.is_empty() {
            let suggestion =
                suggest(&token, cmd.subcommands.iter().map(|c| c.name.as_str())).map(str::to_owned);
            return Err(Error::detailed(
                ErrorKind::UnknownSubcommand,
                format!("unknown subcommand '{token}'"),
                Some(index),
                suggestion,
            ));
        }
        Err(Error::detailed(
            ErrorKind::UnexpectedPositional,
            format!("unexpected argument '{token}'"),
            Some(index),
            None,
        ))
    }

    fn check_duplicate(&self, id: usize, index: usize) -> Result<(), Error> {
        let def = self.defs[id];
        // Env and default values are applied after the command line, so a non-zero count
        // here always comes from an earlier command-line occurrence.
        if self.states[id].count > 0 && !def.multiple {
            return Err(Error::detailed(
                ErrorKind::DuplicateOption,
                format!("'{}' cannot be given more than once", def.display()),
                Some(index),
                None,
            ));
        }
        Ok(())
    }

    fn unexpected_value(&self, id: usize, value: &str, index: usize) -> Error {
        Error::detailed(
            ErrorKind::UnexpectedValue,
            format!(
                "flag '{}' does not take a value (got '{value}')",
                self.defs[id].display()
            ),
            Some(index),
            None,
        )
    }

    fn missing_value(&self, id: usize, index: usize) -> Error {
        Error::detailed(
            ErrorKind::MissingValue,
            format!("option '{}' requires a value", self.defs[id].display()),
            Some(index),
            None,
        )
    }

    fn add_flag(&mut self, id: usize) {
        let state = &mut self.states[id];
        state.count = state.count.saturating_add(1);
        state.source = Some(ValueSource::CommandLine);
    }

    fn add_value(&mut self, id: usize, value: String, index: usize) -> Result<(), Error> {
        let def = self.defs[id];
        if !def.choices.is_empty() && !def.choices.contains(&value) {
            return Err(invalid_choice(def, &value, Some(index), ""));
        }
        let state = &mut self.states[id];
        state.count = state.count.saturating_add(1);
        state.values.push(value);
        state.indices.push(index);
        state.source = Some(ValueSource::CommandLine);
        Ok(())
    }

    /// Applies env vars and defaults to the own arguments of level `depth`, then checks
    /// required arguments.
    fn complete(&mut self, depth: usize, env: &mut EnvLookup<'_>) -> Result<(), Error> {
        let own = self.level_at(depth).own.clone();
        for id in own.clone() {
            self.fill(id, env)?;
        }
        let missing: Vec<String> = own
            .filter(|&id| self.defs[id].required && self.states[id].source.is_none())
            .map(|id| format!("'{}'", self.defs[id].display()))
            .collect();
        let message = match missing.as_slice() {
            [] => return Ok(()),
            [one] => format!("missing required argument {one}"),
            many => format!("missing required arguments: {}", many.join(", ")),
        };
        Err(Error::detailed(
            ErrorKind::MissingRequired,
            message,
            None,
            None,
        ))
    }

    /// Environment variable, then default, for an argument absent from the command line.
    fn fill(&mut self, id: usize, env: &mut EnvLookup<'_>) -> Result<(), Error> {
        let def: &'a Arg = self.defs[id];
        if self.states[id].source.is_some() {
            return Ok(());
        }
        let state = &mut self.states[id];
        if let Some(var) = &def.env {
            let value = env(var).map_err(|NotUnicode| {
                Error::detailed(
                    ErrorKind::InvalidUtf8,
                    format!("environment variable {var} is not valid UTF-8"),
                    None,
                    None,
                )
            })?;
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                let origin = format!(" (from environment variable {var})");
                if def.kind == ArgKind::Flag {
                    match parse_bool(&value) {
                        Some(true) => {
                            state.count = 1;
                            state.source = Some(ValueSource::Env);
                        }
                        Some(false) => {}
                        None => {
                            return Err(Error::detailed(
                                ErrorKind::InvalidValue,
                                format!(
                                    "invalid value '{value}' for '{}'{origin}: \
                                     expected 1/0, true/false, yes/no or on/off",
                                    def.display()
                                ),
                                None,
                                None,
                            ));
                        }
                    }
                    return Ok(());
                }
                if !def.choices.is_empty() && !def.choices.contains(&value) {
                    return Err(invalid_choice(def, &value, None, &origin));
                }
                state.count = 1;
                state.values.push(value);
                state.source = Some(ValueSource::Env);
                return Ok(());
            }
        }
        if let (Some(default), false) = (&def.default, def.kind == ArgKind::Flag) {
            state.values.push(default.clone());
            state.source = Some(ValueSource::Default);
        }
        Ok(())
    }

    fn into_matches(self) -> Matches {
        let Parser {
            defs,
            states,
            root,
            subs,
        } = self;
        let mut child: Option<(String, Matches)> = None;
        for level in subs.iter().rev().chain(core::iter::once(&root)) {
            let inherited: Vec<&Arg> = level
                .args
                .iter()
                .skip(level.own.len())
                .map(|&id| defs[id])
                .collect();
            let args = level
                .args
                .iter()
                .map(|&id| {
                    let (def, state) = (defs[id], &states[id]);
                    MatchedArg {
                        name: def.name.clone(),
                        display: def.display(),
                        env: def.env.clone(),
                        values: state.values.clone(),
                        indices: state.indices.clone(),
                        count: state.count,
                        source: state.source,
                    }
                })
                .collect();
            let matches = Matches {
                args,
                subcommand: child.take().map(Box::new),
                footer: Some(help::footer(level.cmd, &level.path, &inherited)),
            };
            child = Some((level.cmd.name.clone(), matches));
        }
        // The loop always runs at least once (for the root).
        child.map(|(_, m)| m).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use crate::command::tests::hotel;
    use crate::{Arg, Command, Error, ErrorKind, Matches, ValueSource};

    fn parse(cmd: &Command, args: &[&str]) -> Result<Matches, Error> {
        parse_env(cmd, args, &[])
    }

    fn parse_env(cmd: &Command, args: &[&str], vars: &[(&str, &str)]) -> Result<Matches, Error> {
        let argv = core::iter::once(cmd.name.as_str()).chain(args.iter().copied());
        cmd.parse_from_with_env(argv, |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        })
    }

    fn ok(cmd: &Command, args: &[&str]) -> Matches {
        parse(cmd, args).unwrap_or_else(|e| panic!("{args:?}: {}", e.render()))
    }

    fn err(cmd: &Command, args: &[&str]) -> Error {
        parse(cmd, args).expect_err("expected an error")
    }

    fn offset_cmd() -> Command {
        Command::new("calc")
            .arg(Arg::option("offset").short('o'))
            .arg(Arg::flag("all").short('a'))
            .arg(Arg::positional("n"))
    }

    #[test]
    fn long_forms() {
        let cmd = hotel();
        for args in [
            &["--port", "9000", "f"][..],
            &["--port=9000", "f"],
            &["f", "--port", "9000"],
        ] {
            let m = ok(&cmd, args);
            assert_eq!(m.value("port"), Some("9000"), "{args:?}");
            assert_eq!(m.value("input"), Some("f"));
            assert_eq!(m.source("port"), Some(ValueSource::CommandLine));
        }
        assert_eq!(ok(&cmd, &["--port=", "f"]).value("port"), Some(""));
        assert_eq!(ok(&cmd, &["--port=a=b", "f"]).value("port"), Some("a=b"));
        assert!(ok(&cmd, &["--verbose", "f"]).flag("verbose"));
    }

    #[test]
    fn short_forms() {
        let cmd = hotel();
        for args in [
            &["-p", "9000", "f"][..],
            &["-p9000", "f"],
            &["-p=9000", "f"],
        ] {
            assert_eq!(ok(&cmd, args).value("port"), Some("9000"), "{args:?}");
        }
        assert_eq!(ok(&cmd, &["-p==", "f"]).value("port"), Some("="));
    }

    #[test]
    fn clusters() {
        let cmd = hotel();
        let m = ok(&cmd, &["-vvv", "f"]);
        assert_eq!(m.count("verbose"), 3);
        let m = ok(&cmd, &["-vp8080", "f"]);
        assert_eq!((m.count("verbose"), m.value("port")), (1, Some("8080")));
        let m = ok(&cmd, &["-vvp", "1", "f"]);
        assert_eq!((m.count("verbose"), m.value("port")), (2, Some("1")));
        let m = ok(&cmd, &["-vI", "a", "-vIb", "f"]);
        assert_eq!(m.count("verbose"), 2);
        assert_eq!(m.values("include"), ["a", "b"]);
        // The value swallows the rest of the cluster, including letters that are flags.
        assert_eq!(ok(&cmd, &["-pv", "f"]).value("port"), Some("v"));
    }

    #[test]
    fn double_dash_and_lone_dash() {
        let cmd = hotel();
        let m = ok(&cmd, &["-v", "--", "-v", "--port", "--", "serve"]);
        assert_eq!(m.count("verbose"), 1);
        assert_eq!(m.value("input"), Some("-v"));
        assert_eq!(m.values("rest"), ["--port", "--", "serve"]);
        assert!(m.subcommand().is_none());
        assert_eq!(m.source("port"), Some(ValueSource::Default));

        let m = ok(&cmd, &["-", "-"]);
        assert_eq!(m.value("input"), Some("-"));
        assert_eq!(m.values("rest"), ["-"]);
    }

    #[test]
    fn values_starting_with_dash() {
        let cmd = offset_cmd();
        for args in [
            &["--offset", "-5"][..],
            &["--offset=-5"],
            &["-o", "-5"],
            &["-o-5"],
        ] {
            assert_eq!(ok(&cmd, args).value("offset"), Some("-5"), "{args:?}");
        }
        assert_eq!(ok(&cmd, &["-o", "--all"]).value("offset"), Some("--all"));
        assert!(!ok(&cmd, &["-o", "--all"]).flag("all"));
        assert_eq!(ok(&cmd, &["-o", "--"]).value("offset"), Some("--"));
        let m = ok(&hotel(), &["--port", "-h", "f"]);
        assert_eq!(m.value("port"), Some("-h"));
    }

    #[test]
    fn negative_number_positionals() {
        let cmd = offset_cmd();
        assert_eq!(ok(&cmd, &["-5"]).value("n"), Some("-5"));
        assert_eq!(ok(&cmd, &["-1.5e3"]).value("n"), Some("-1.5e3"));
        assert_eq!(err(&cmd, &["-5x"]).kind(), ErrorKind::UnknownOption);

        let digits = Command::new("x")
            .arg(Arg::flag("one").short('1'))
            .arg(Arg::positional("n"));
        assert!(ok(&digits, &["-1"]).flag("one"));
        let e = err(&digits, &["-5"]);
        assert_eq!(
            (e.kind(), e.to_string()),
            (ErrorKind::UnknownOption, "unknown option '-5'".into())
        );
    }

    #[test]
    fn multiple_and_count() {
        let cmd = hotel();
        let m = ok(
            &cmd,
            &[
                "-I",
                "a",
                "--include",
                "b",
                "--include=c",
                "-Id",
                "f",
                "g",
                "h",
            ],
        );
        assert_eq!(m.values("include"), ["a", "b", "c", "d"]);
        assert_eq!(m.value("include"), Some("a"));
        assert_eq!(m.count("include"), 4);
        assert_eq!(m.values("rest"), ["g", "h"]);
        assert_eq!(m.count("rest"), 2);
        let m = ok(&cmd, &["-v", "f", "--verbose", "-vv"]);
        assert_eq!(m.count("verbose"), 4);
        let m = ok(&cmd, &["f"]);
        assert_eq!((m.count("verbose"), m.flag("verbose")), (0, false));
        assert!(m.values("include").is_empty());
        assert_eq!(m.source("include"), None);
    }

    #[test]
    fn duplicates() {
        let cmd = hotel();
        let e = err(&cmd, &["--port", "1", "-p", "2", "f"]);
        assert_eq!(e.kind(), ErrorKind::DuplicateOption);
        assert_eq!(e.index(), Some(3));
        assert_eq!(
            e.to_string(),
            "'--port <PORT>' cannot be given more than once"
        );

        let e = err(&offset_cmd(), &["-aa"]);
        assert_eq!((e.kind(), e.index()), (ErrorKind::DuplicateOption, Some(1)));
        assert_eq!(e.to_string(), "'--all' cannot be given more than once");
        assert_eq!(
            err(&offset_cmd(), &["--all", "-a"]).kind(),
            ErrorKind::DuplicateOption
        );
    }

    #[test]
    fn env_and_default_precedence() {
        let cmd = hotel();
        let env = [("HOTEL_PORT", "7000")];
        let m = parse_env(&cmd, &["f"], &env).unwrap();
        assert_eq!(
            (m.value("port"), m.source("port")),
            (Some("7000"), Some(ValueSource::Env))
        );
        assert_eq!((m.count("port"), m.flag("port")), (1, true));
        let m = parse_env(&cmd, &["-p", "9000", "f"], &env).unwrap();
        assert_eq!(
            (m.value("port"), m.source("port")),
            (Some("9000"), Some(ValueSource::CommandLine))
        );
        let m = parse_env(&cmd, &["f"], &[]).unwrap();
        assert_eq!(
            (m.value("port"), m.source("port")),
            (Some("8080"), Some(ValueSource::Default))
        );
        assert_eq!((m.count("port"), m.flag("port")), (0, true));
        let m = parse_env(&cmd, &["f"], &[("HOTEL_PORT", "")]).unwrap();
        assert_eq!(m.source("port"), Some(ValueSource::Default));
        assert_eq!(m.get::<u16>("port").unwrap(), Some(8080));
    }

    #[test]
    fn env_satisfies_required_and_positionals() {
        let cmd = Command::new("app")
            .arg(Arg::option("token").env("APP_TEST_TOKEN").required())
            .arg(
                Arg::positional("file")
                    .env("APP_TEST_FILE")
                    .default("in.csv"),
            );
        let e = parse_env(&cmd, &[], &[]).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::MissingRequired);
        let m = parse_env(&cmd, &[], &[("APP_TEST_TOKEN", "s3cret")]).unwrap();
        assert_eq!(m.value("token"), Some("s3cret"));
        assert_eq!(m.value("file"), Some("in.csv"));
        let m = parse_env(
            &cmd,
            &["x.csv"],
            &[("APP_TEST_TOKEN", "t"), ("APP_TEST_FILE", "e.csv")],
        )
        .unwrap();
        assert_eq!(m.value("file"), Some("x.csv"));
        let m = parse_env(
            &cmd,
            &[],
            &[("APP_TEST_TOKEN", "t"), ("APP_TEST_FILE", "e.csv")],
        )
        .unwrap();
        assert_eq!(
            (m.value("file"), m.source("file")),
            (Some("e.csv"), Some(ValueSource::Env))
        );
    }

    #[test]
    fn flag_env_values() {
        let cmd = Command::new("app").arg(Arg::flag("debug").env("APP_TEST_DEBUG"));
        for v in ["1", "true", "YES", "On"] {
            let m = parse_env(&cmd, &[], &[("APP_TEST_DEBUG", v)]).unwrap();
            assert!(m.flag("debug"), "{v}");
            assert_eq!(
                (m.count("debug"), m.source("debug")),
                (1, Some(ValueSource::Env))
            );
        }
        for v in ["0", "false", "No", "OFF", ""] {
            let m = parse_env(&cmd, &[], &[("APP_TEST_DEBUG", v)]).unwrap();
            assert!(!m.flag("debug"), "{v}");
            assert_eq!(m.source("debug"), None);
        }
        let e = parse_env(&cmd, &[], &[("APP_TEST_DEBUG", "maybe")]).unwrap_err();
        assert_eq!((e.kind(), e.index()), (ErrorKind::InvalidValue, None));
        assert_eq!(
            e.to_string(),
            "invalid value 'maybe' for '--debug' (from environment variable APP_TEST_DEBUG): \
             expected 1/0, true/false, yes/no or on/off"
        );
        // The command line wins without consulting the environment.
        assert!(
            parse_env(&cmd, &["--debug"], &[("APP_TEST_DEBUG", "maybe")])
                .unwrap()
                .flag("debug")
        );
    }

    #[test]
    fn choices() {
        let cmd = hotel();
        assert_eq!(
            ok(&cmd, &["--format", "json", "f"]).value("format"),
            Some("json")
        );
        assert_eq!(ok(&cmd, &["f"]).value("format"), Some("text"));
        let e = err(&cmd, &["--format=jsn", "f"]);
        assert_eq!(
            (e.kind(), e.index(), e.suggestion()),
            (ErrorKind::InvalidChoice, Some(1), Some("json"))
        );
        assert_eq!(
            e.to_string(),
            "invalid value 'jsn' for '--format <FORMAT>' [possible values: json, text]; did you mean 'json'?"
        );
        let e = err(&cmd, &["--format", "xml", "f"]);
        assert_eq!((e.index(), e.suggestion()), (Some(2), None));
        let e = parse_env(
            &Command::new("a").arg(Arg::option("mode").choices(["fast", "safe"]).env("A_MODE")),
            &[],
            &[("A_MODE", "slow")],
        )
        .unwrap_err();
        assert_eq!((e.kind(), e.index()), (ErrorKind::InvalidChoice, None));
        assert_eq!(
            e.to_string(),
            "invalid value 'slow' for '--mode <MODE>' (from environment variable A_MODE) \
             [possible values: fast, safe]"
        );
        let positional = Command::new("a").arg(Arg::positional("op").choices(["add", "rm"]));
        assert_eq!(err(&positional, &["ad"]).suggestion(), Some("add"));
    }

    #[test]
    fn required() {
        let cmd = hotel();
        let e = err(&cmd, &[]);
        assert_eq!((e.kind(), e.index()), (ErrorKind::MissingRequired, None));
        assert_eq!(e.to_string(), "missing required argument '<input>'");
        assert_eq!(
            e.render(),
            "error: missing required argument '<input>'\n\n\
             Usage: hotel [OPTIONS] <input> [rest]... [COMMAND]\n\n\
             For more information, try 'hotel --help'.\n"
        );
        let two = Command::new("a")
            .arg(Arg::option("user").required())
            .arg(Arg::positional("x").required());
        assert_eq!(
            err(&two, &[]).to_string(),
            "missing required arguments: '--user <USER>', '<x>'"
        );
    }

    #[test]
    fn unknown_options() {
        let cmd = hotel();
        let e = err(&cmd, &["f", "--prot", "1"]);
        assert_eq!(
            (e.kind(), e.index(), e.suggestion()),
            (ErrorKind::UnknownOption, Some(2), Some("--port"))
        );
        assert_eq!(
            e.to_string(),
            "unknown option '--prot'; did you mean '--port'?"
        );
        assert_eq!(
            e.render(),
            "error: unknown option '--prot'; did you mean '--port'?\n\n\
             Usage: hotel [OPTIONS] <input> [rest]... [COMMAND]\n\n\
             For more information, try 'hotel --help'.\n"
        );
        assert_eq!(err(&cmd, &["--prot=1"]).suggestion(), Some("--port"));
        assert_eq!(err(&cmd, &["--verison"]).suggestion(), Some("--version"));
        assert_eq!(err(&cmd, &["--hepl"]).suggestion(), Some("--help"));
        let e = err(&cmd, &["--xyz"]);
        assert_eq!(
            (e.to_string(), e.suggestion()),
            ("unknown option '--xyz'".into(), None)
        );
        let e = err(&cmd, &["-x"]);
        assert_eq!(
            (e.to_string(), e.index()),
            ("unknown option '-x'".into(), Some(1))
        );
        let e = err(&cmd, &["-vx"]);
        assert_eq!(e.to_string(), "unknown option '-x'");
        let e = err(&cmd, &["-format", "json"]);
        assert_eq!(
            e.to_string(),
            "unknown option '-f'; did you mean '--format'?"
        );
        // Exact match only: no abbreviations.
        assert_eq!(err(&cmd, &["--verb", "f"]).kind(), ErrorKind::UnknownOption);
        // Hidden arguments are accepted but never suggested.
        let hidden = Command::new("a").arg(Arg::flag("secret").hidden());
        assert!(ok(&hidden, &["--secret"]).flag("secret"));
        assert_eq!(err(&hidden, &["--secrt"]).suggestion(), None);
    }

    #[test]
    fn aliases() {
        let cmd =
            Command::new("a").arg(Arg::option("output").short('o').alias("out").alias("dest"));
        assert_eq!(ok(&cmd, &["--out", "x"]).value("output"), Some("x"));
        assert_eq!(ok(&cmd, &["--dest=y"]).value("output"), Some("y"));
        assert_eq!(
            err(&cmd, &["--out", "x", "-o", "y"]).kind(),
            ErrorKind::DuplicateOption
        );
        assert_eq!(err(&cmd, &["--dst"]).suggestion(), Some("--dest"));
    }

    #[test]
    fn unexpected_values() {
        let cmd = hotel();
        let e = err(&cmd, &["--verbose=x", "f"]);
        assert_eq!((e.kind(), e.index()), (ErrorKind::UnexpectedValue, Some(1)));
        assert_eq!(
            e.to_string(),
            "flag '--verbose' does not take a value (got 'x')"
        );
        assert_eq!(
            err(&cmd, &["--verbose=", "f"]).kind(),
            ErrorKind::UnexpectedValue
        );
        let e = err(&cmd, &["-v=x", "f"]);
        assert_eq!((e.kind(), e.index()), (ErrorKind::UnexpectedValue, Some(1)));
        assert_eq!(
            err(&cmd, &["-vv=x", "f"]).kind(),
            ErrorKind::UnexpectedValue
        );
    }

    #[test]
    fn missing_values() {
        let cmd = hotel();
        for args in [&["f", "--port"][..], &["f", "-p"], &["f", "-vp"]] {
            let e = err(&cmd, args);
            assert_eq!(
                (e.kind(), e.index()),
                (ErrorKind::MissingValue, Some(2)),
                "{args:?}"
            );
            assert_eq!(e.to_string(), "option '--port <PORT>' requires a value");
        }
    }

    #[test]
    fn unexpected_positionals() {
        let cmd = Command::new("a").arg(Arg::positional("x"));
        let e = err(&cmd, &["1", "2"]);
        assert_eq!(
            (e.kind(), e.index()),
            (ErrorKind::UnexpectedPositional, Some(2))
        );
        assert_eq!(e.to_string(), "unexpected argument '2'");
        assert_eq!(
            err(&Command::new("a"), &["x"]).kind(),
            ErrorKind::UnexpectedPositional
        );
        // After `--` a word is never a subcommand.
        let subs = Command::new("a").subcommand(Command::new("run"));
        assert_eq!(
            err(&subs, &["--", "run"]).kind(),
            ErrorKind::UnexpectedPositional
        );
    }

    fn tool() -> Command {
        Command::new("tool")
            .version("3.0")
            .arg(Arg::flag("verbose").short('v').count().global())
            .arg(
                Arg::option("config")
                    .short('c')
                    .env("TOOL_TEST_CONFIG")
                    .global(),
            )
            .arg(Arg::flag("local").short('l'))
            .subcommand(
                Command::new("db")
                    .about("Database tools")
                    .arg(Arg::flag("dry-run").short('n').global())
                    .subcommand(
                        Command::new("migrate")
                            .version("3.0-db")
                            .arg(Arg::option("to").required())
                            .arg(Arg::positional("name")),
                    )
                    .subcommand(Command::new("seed")),
            )
            .subcommand(Command::new("serve").arg(Arg::option("port").short('p')))
    }

    #[test]
    fn subcommands() {
        let cmd = tool();
        let m = ok(&cmd, &["serve", "-p", "80"]);
        let (name, serve) = m.subcommand().unwrap();
        assert_eq!((name, serve.value("port")), ("serve", Some("80")));
        assert!(serve.subcommand().is_none());

        let m = ok(&cmd, &["-l", "db", "migrate", "--to", "7", "init"]);
        assert!(m.flag("local"));
        let (_, db) = m.subcommand().unwrap();
        let (name, migrate) = db.subcommand().unwrap();
        assert_eq!(name, "migrate");
        assert_eq!(migrate.required::<u32>("to").unwrap(), 7);
        assert_eq!(migrate.value("name"), Some("init"));
        assert!(ok(&cmd, &[]).subcommand().is_none());

        // Parent-only options are not accepted after the subcommand.
        let e = err(&cmd, &["serve", "-l"]);
        assert_eq!(e.kind(), ErrorKind::UnknownOption);
        assert!(
            e.render().contains("Usage: tool serve [OPTIONS]"),
            "{}",
            e.render()
        );
        // Required arguments of the selected subcommand are checked.
        let e = err(&cmd, &["db", "migrate"]);
        assert_eq!(e.to_string(), "missing required argument '--to <TO>'");
        assert!(
            e.render().contains("try 'tool db migrate --help'"),
            "{}",
            e.render()
        );
    }

    #[test]
    fn global_args() {
        let cmd = tool();
        let m = ok(
            &cmd,
            &[
                "-v", "db", "-vn", "migrate", "--to", "1", "-v", "--config", "c.toml",
            ],
        );
        let (_, db) = m.subcommand().unwrap();
        let (_, migrate) = db.subcommand().unwrap();
        for level in [&m, db, migrate] {
            assert_eq!(level.count("verbose"), 3);
            assert_eq!(level.value("config"), Some("c.toml"));
        }
        assert!(db.flag("dry-run") && migrate.flag("dry-run"));
        // Globals share one state, so a non-multiple global given at two levels is a duplicate.
        let e = err(&cmd, &["-c", "a", "serve", "-c", "b"]);
        assert_eq!((e.kind(), e.index()), (ErrorKind::DuplicateOption, Some(4)));
        // Env applies to globals at every level.
        let m = parse_env(&cmd, &["serve"], &[("TOOL_TEST_CONFIG", "env.toml")]).unwrap();
        assert_eq!(m.subcommand().unwrap().1.value("config"), Some("env.toml"));
        assert_eq!(
            m.subcommand().unwrap().1.source("config"),
            Some(ValueSource::Env)
        );
        // A global defined in a subcommand is not accepted by its parent.
        assert_eq!(err(&cmd, &["-n"]).kind(), ErrorKind::UnknownOption);
    }

    #[test]
    fn unknown_subcommands() {
        let cmd = tool();
        let e = err(&cmd, &["srve"]);
        assert_eq!(
            (e.kind(), e.index(), e.suggestion()),
            (ErrorKind::UnknownSubcommand, Some(1), Some("serve"))
        );
        assert_eq!(
            e.to_string(),
            "unknown subcommand 'srve'; did you mean 'serve'?"
        );
        let e = err(&cmd, &["db", "migrat"]);
        assert_eq!(e.suggestion(), Some("migrate"));
        assert!(
            e.render().contains("Usage: tool db [OPTIONS] [COMMAND]"),
            "{}",
            e.render()
        );
        assert_eq!(err(&cmd, &["zzz"]).suggestion(), None);
    }

    #[test]
    fn subcommand_name_vs_positional() {
        // Before `--`, a word equal to a subcommand name selects it.
        let cmd = hotel();
        let m = ok(&cmd, &["in.txt", "a", "serve", "--bind", "0.0.0.0"]);
        assert_eq!(m.values("rest"), ["a"]);
        assert_eq!(m.subcommand().unwrap().1.value("bind"), Some("0.0.0.0"));
        // Parent requirements still apply when a subcommand is selected.
        assert_eq!(err(&cmd, &["serve"]).kind(), ErrorKind::MissingRequired);
        // With free positional slots, a near-miss is a value, not an error.
        assert_eq!(ok(&cmd, &["srve"]).value("input"), Some("srve"));
    }

    #[test]
    fn subcommand_required() {
        let cmd = Command::new("git")
            .subcommand_required()
            .subcommand(Command::new("push"));
        let e = err(&cmd, &[]);
        assert_eq!(e.kind(), ErrorKind::MissingRequired);
        assert_eq!(e.to_string(), "'git' requires a subcommand");
        assert!(e.render().contains("Usage: git [OPTIONS] <COMMAND>"));
        assert!(ok(&cmd, &["push"]).subcommand().is_some());
    }

    #[test]
    fn help_requests() {
        let cmd = hotel();
        let help = cmd.render_help();
        for args in [
            &["-h"][..],
            &["--help"],
            &["-vh"],
            &["-p", "1", "-h", "f"],
            &["f", "--help"],
        ] {
            let e = err(&cmd, args);
            assert_eq!(
                (e.kind(), e.exit_code()),
                (ErrorKind::DisplayHelp, 0),
                "{args:?}"
            );
            assert_eq!(e.to_string(), help);
        }
        // Errors earlier on the command line win, as arguments are processed in order.
        assert_eq!(
            err(&cmd, &["--prot", "--help"]).kind(),
            ErrorKind::UnknownOption
        );
        // After `--`, `-h` is a value.
        assert_eq!(ok(&cmd, &["--", "-h"]).value("input"), Some("-h"));
        let e = err(&tool(), &["db", "--help"]);
        assert!(
            e.to_string()
                .starts_with("Usage: tool db [OPTIONS] [COMMAND]\n\nDatabase tools\n")
        );
        assert!(
            e.to_string().contains("  -v, --verbose...")
                && e.to_string().contains("  -c, --config <CONFIG>")
        );
    }

    #[test]
    fn version_requests() {
        let cmd = hotel();
        for args in [&["-V"][..], &["--version"], &["-vV"]] {
            let e = err(&cmd, args);
            assert_eq!((e.kind(), e.exit_code()), (ErrorKind::DisplayVersion, 0));
            assert_eq!(e.to_string(), "hotel 1.2.0\n");
            assert_eq!(e.render(), "hotel 1.2.0\n");
        }
        assert_eq!(
            err(&tool(), &["db", "migrate", "-V"]).to_string(),
            "tool db migrate 3.0-db\n"
        );
        // Subcommands without a version have no -V.
        let e = err(&cmd, &["f", "serve", "--version"]);
        assert_eq!((e.kind(), e.suggestion()), (ErrorKind::UnknownOption, None));
        assert_eq!(
            err(&cmd, &["f", "serve", "-V"]).kind(),
            ErrorKind::UnknownOption
        );
        assert_eq!(
            err(&Command::new("x"), &["--version"]).kind(),
            ErrorKind::UnknownOption
        );
    }

    #[test]
    fn user_defined_h_and_v() {
        let cmd = Command::new("srv")
            .version("1")
            .arg(Arg::option("host").short('h'))
            .arg(Arg::flag("verbose").short('V'));
        let m = ok(&cmd, &["-h", "example.org", "-V"]);
        assert_eq!(m.value("host"), Some("example.org"));
        assert!(m.flag("verbose"));
        assert_eq!(err(&cmd, &["--help"]).kind(), ErrorKind::DisplayHelp);
        assert_eq!(err(&cmd, &["--version"]).kind(), ErrorKind::DisplayVersion);
    }

    #[test]
    fn utf8_and_unicode_names() {
        let cmd = Command::new("vn")
            .arg(Arg::option("tên").short('t'))
            .arg(Arg::positional("x"));
        let m = ok(&cmd, &["--tên", "Nguyễn", "Hà Nội"]);
        assert_eq!(
            (m.value("tên"), m.value("x")),
            (Some("Nguyễn"), Some("Hà Nội"))
        );
        assert_eq!(err(&cmd, &["--ten"]).suggestion(), Some("--tên"));
        let m = ok(&cmd, &["-tĐà", "Hà Nội"]);
        assert_eq!((m.value("tên"), m.value("x")), (Some("Đà"), Some("Hà Nội")));
    }

    #[cfg(any(unix, windows))]
    fn non_utf8() -> std::ffi::OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![0x66, 0xff])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[0x66, 0xD800])
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn invalid_utf8_argument() {
        use std::ffi::OsString;
        let cmd = hotel();
        let argv: Vec<OsString> = vec!["hotel".into(), "-v".into(), non_utf8()];
        let e = cmd.parse_from_with_env(argv, |_| None).unwrap_err();
        assert_eq!((e.kind(), e.index()), (ErrorKind::InvalidUtf8, Some(2)));
        assert_eq!(e.to_string(), "argument 2 is not valid UTF-8");
        // The program name is never inspected.
        let argv: Vec<OsString> = vec![non_utf8(), "f".into()];
        assert!(cmd.parse_from_with_env(argv, |_| None).is_ok());
    }

    #[test]
    fn invalid_utf8_env() {
        use super::NotUnicode;
        let cmd = hotel();
        let mut env = |_: &str| -> Result<Option<String>, NotUnicode> { Err(NotUnicode) };
        let e = cmd.parse_with(["hotel", "f"], &mut env).unwrap_err();
        assert_eq!((e.kind(), e.index()), (ErrorKind::InvalidUtf8, None));
        assert_eq!(
            e.to_string(),
            "environment variable HOTEL_PORT is not valid UTF-8"
        );
    }

    #[test]
    fn empty_and_program_name_only() {
        let cmd = Command::new("a").arg(Arg::flag("x"));
        let none: [&str; 0] = [];
        assert!(!cmd.parse_from_with_env(none, |_| None).unwrap().flag("x"));
        assert!(!cmd.parse_from_with_env(["a"], |_| None).unwrap().flag("x"));
    }

    #[test]
    fn process_env_is_used_by_parse_from() {
        // PATH (or Path on Windows) is set in every test environment; no mutation needed.
        let var = if cfg!(windows) { "Path" } else { "PATH" };
        let cmd = Command::new("a").arg(Arg::option("path").env(var));
        let m = cmd.parse_from(["a"]).unwrap();
        assert_eq!(m.source("path"), Some(ValueSource::Env));
    }

    #[test]
    fn spec_example() {
        // `parse_from` reads the process environment; the command line wins over any
        // HOTEL_PORT that happens to be set.
        let m = hotel()
            .parse_from(["hotel", "-vv", "--port=9000", "file.txt"])
            .unwrap();
        assert_eq!(m.get::<u16>("port").unwrap(), Some(9000));
        let m = ok(&hotel(), &["-vv", "--port=9000", "file.txt"]);
        assert_eq!(m.count("verbose"), 2);
        assert!(m.flag("verbose"));
        assert_eq!(m.get::<u16>("port").unwrap(), Some(9000));
        assert_eq!(m.required::<u16>("port").unwrap(), 9000);
        assert_eq!(m.value("input"), Some("file.txt"));
        assert_eq!(m.value("format"), Some("text"));
        assert_eq!(m.source("format"), Some(ValueSource::Default));
        assert!(m.values("rest").is_empty());
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let cmd = tool();
        let long = "x".repeat(100_000);
        let inputs: Vec<Vec<&str>> = vec![
            vec!["-"],
            vec!["--"],
            vec!["---"],
            vec!["--="],
            vec!["-="],
            vec!["-v="],
            vec!["--=x"],
            vec!["-\u{1F600}"],
            vec!["--\u{1F600}"],
            vec!["-v\u{1F600}"],
            vec!["-c\u{1F600}"],
            vec![long.as_str()],
            vec!["--", long.as_str()],
            vec!["db", "--", "-x"],
            vec!["db", "migrate", "--to"],
            vec!["-ccc"],
            vec!["-vvvvvvvvvvvvvvvvvvvv"],
        ];
        for args in inputs {
            let _ = parse(&cmd, &args);
        }
        let long_opt = format!("--{long}");
        assert_eq!(err(&cmd, &[&long_opt]).suggestion(), None);
    }
}
