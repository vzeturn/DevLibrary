//! Help, usage and version text.
//!
//! Layout: the usage line, optional about text, then the sections `Arguments:`, `Options:`
//! and `Commands:`. Entries are indented by two spaces and their descriptions start in a
//! shared column (at most [`MAX_HELP_COLUMN`]); an entry wider than that puts its
//! description on the next line. Descriptions wrap at [`WIDTH`] columns.

use crate::Command;
use crate::arg::{Arg, ArgKind};

/// Total line width of the help text.
pub(crate) const WIDTH: usize = 100;
/// Description column limit; wider entries put the description on the next line.
const MAX_HELP_COLUMN: usize = 40;
const INDENT: usize = 2;
const GAP: usize = 2;

/// The automatic `-h/--help` or `-V/--version` switches that the command's own arguments
/// did not take over.
pub(crate) fn builtin_switches(
    args: &[&Arg],
    short: char,
    long: &'static str,
) -> (Option<char>, Option<&'static str>) {
    let short = (!args.iter().any(|a| a.matches_short(short))).then_some(short);
    let long = (!args.iter().any(|a| a.matches_long(long))).then_some(long);
    (short, long)
}

fn all_args<'a>(cmd: &'a Command, inherited: &[&'a Arg]) -> Vec<&'a Arg> {
    cmd.args.iter().chain(inherited.iter().copied()).collect()
}

/// `Usage: <path> [OPTIONS] --required <R> <pos> [rest]... [COMMAND]`.
pub(crate) fn usage(cmd: &Command, path: &str, inherited: &[&Arg]) -> String {
    let all = all_args(cmd, inherited);
    let visible = || all.iter().copied().filter(|a| !a.hidden);
    let mut out = format!("Usage: {path}");
    let has_builtin = !builtins(cmd, &all).is_empty();
    if has_builtin || visible().any(|a| !a.is_positional()) {
        out.push_str(" [OPTIONS]");
    }
    for arg in visible().filter(|a| a.required && !a.is_positional()) {
        out.push(' ');
        out.push_str(&arg.display());
    }
    for arg in visible().filter(|a| a.is_positional()) {
        out.push(' ');
        out.push_str(&arg.usage_token());
    }
    if !cmd.subcommands.is_empty() {
        out.push_str(if cmd.subcommand_required {
            " <COMMAND>"
        } else {
            " [COMMAND]"
        });
    }
    out
}

/// The usage line plus a hint to run `--help`, appended to parse errors.
pub(crate) fn footer(cmd: &Command, path: &str, inherited: &[&Arg]) -> String {
    let mut out = usage(cmd, path, inherited);
    let (short, long) = builtin_switches(&all_args(cmd, inherited), 'h', "help");
    let hint = long
        .map(|l| format!("--{l}"))
        .or_else(|| short.map(|c| format!("-{c}")));
    if let Some(hint) = hint {
        out.push_str(&format!("\n\nFor more information, try '{path} {hint}'."));
    }
    out
}

/// `<path> <version>\n`.
pub(crate) fn version(cmd: &Command, path: &str) -> String {
    format!("{path} {}\n", cmd.version.as_deref().unwrap_or_default())
}

/// The complete help text.
pub(crate) fn render(cmd: &Command, path: &str, inherited: &[&Arg]) -> String {
    let all = all_args(cmd, inherited);
    let mut out = usage(cmd, path, inherited);
    out.push('\n');
    if let Some(about) = &cmd.about {
        out.push('\n');
        for line in wrap(about, WIDTH) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    let arguments: Vec<(String, String)> = all
        .iter()
        .filter(|a| a.is_positional() && !a.hidden)
        .map(|a| (a.usage_token(), describe(a)))
        .collect();
    let mut options: Vec<(String, String)> = all
        .iter()
        .filter(|a| !a.is_positional() && !a.hidden)
        .map(|a| (option_left(a), describe(a)))
        .collect();
    options.extend(builtins(cmd, &all));
    let commands: Vec<(String, String)> = cmd
        .subcommands
        .iter()
        .map(|c| {
            let about = c
                .about
                .as_deref()
                .and_then(|a| a.lines().next())
                .unwrap_or_default();
            (c.name.clone(), about.to_owned())
        })
        .collect();

    let widest = arguments
        .iter()
        .chain(&options)
        .chain(&commands)
        .map(|(left, _)| left.chars().count())
        .max()
        .unwrap_or(0);
    let column = (INDENT + widest + GAP).min(MAX_HELP_COLUMN);
    for (title, entries) in [
        ("Arguments", arguments),
        ("Options", options),
        ("Commands", commands),
    ] {
        if entries.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(title);
        out.push_str(":\n");
        for (left, help) in &entries {
            write_entry(&mut out, left, help, column);
        }
    }
    out
}

/// Entries for the automatic help and version options.
fn builtins(cmd: &Command, all: &[&Arg]) -> Vec<(String, String)> {
    let help = builtin_switches(all, 'h', "help");
    let version = cmd
        .version
        .as_ref()
        .map(|_| builtin_switches(all, 'V', "version"));
    [(Some(help), "Print help"), (version, "Print version")]
        .into_iter()
        .filter_map(|(switches, text)| {
            let (short, long) = switches?;
            (short.is_some() || long.is_some()).then(|| (switch_left(short, long), text.to_owned()))
        })
        .collect()
}

fn switch_left(short: Option<char>, long: Option<&str>) -> String {
    match (short, long) {
        (Some(s), Some(l)) => format!("-{s}, --{l}"),
        (Some(s), None) => format!("-{s}"),
        (None, Some(l)) => format!("    --{l}"),
        (None, None) => String::new(),
    }
}

/// `-p, --port <PORT>`, `    --format <FORMAT>`, `-v, --verbose...`.
fn option_left(arg: &Arg) -> String {
    let mut left = switch_left(arg.short, arg.long.as_deref());
    if arg.kind == ArgKind::Option {
        left.push_str(&format!(" <{}>", arg.placeholder()));
    }
    if arg.multiple {
        left.push_str("...");
    }
    left
}

/// Help text plus `[aliases: ..] [default: ..] [env: ..] [possible values: ..]`.
fn describe(arg: &Arg) -> String {
    let mut parts: Vec<String> = arg.help.iter().cloned().collect();
    if !arg.aliases.is_empty() {
        let aliases: Vec<String> = arg.aliases.iter().map(|a| format!("--{a}")).collect();
        parts.push(format!("[aliases: {}]", aliases.join(", ")));
    }
    if let Some(default) = &arg.default {
        parts.push(format!("[default: {default}]"));
    }
    if let Some(env) = &arg.env {
        parts.push(format!("[env: {env}]"));
    }
    if !arg.choices.is_empty() {
        parts.push(format!("[possible values: {}]", arg.choices.join(", ")));
    }
    parts.join(" ")
}

fn write_entry(out: &mut String, left: &str, help: &str, column: usize) {
    out.push_str(&" ".repeat(INDENT));
    out.push_str(left);
    let used = INDENT + left.chars().count();
    let lines = wrap(help, WIDTH - column);
    let mut lines = lines.iter();
    if let Some(first) = lines.next() {
        if used + GAP <= column {
            out.push_str(&" ".repeat(column - used));
        } else {
            out.push('\n');
            out.push_str(&" ".repeat(column));
        }
        out.push_str(first);
    }
    for line in lines {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(&" ".repeat(column));
            out.push_str(line);
        }
    }
    out.push('\n');
}

/// Greedy word wrap. Each input line is wrapped separately; a word longer than `width`
/// gets a line of its own. Widths are counted in `char`s.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        let mut len = 0;
        for word in paragraph.split_whitespace() {
            let word_len = word.chars().count();
            if len > 0 && len + 1 + word_len > width {
                lines.push(core::mem::take(&mut line));
                len = 0;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += word_len;
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Arg, Command, ErrorKind};

    fn help_of(cmd: &Command, args: &[&str]) -> String {
        let argv = core::iter::once(cmd.name.as_str()).chain(args.iter().copied());
        let e = cmd.parse_from_with_env(argv, |_| None).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::DisplayHelp);
        assert_eq!(e.render(), e.to_string());
        e.to_string()
    }

    #[test]
    fn spec_example_snapshot() {
        let expected = "\
Usage: hotel [OPTIONS] <input> [rest]... [COMMAND]

Hotel PMS tools

Arguments:
  <input>                     Input file
  [rest]...

Options:
  -v, --verbose...            More output
  -p, --port <PORT>           Listen port [default: 8080] [env: HOTEL_PORT]
      --format <FORMAT>       [default: text] [possible values: json, text]
  -I, --include <INCLUDE>...
  -h, --help                  Print help
  -V, --version               Print version

Commands:
  serve                       Run server
";
        let cmd = crate::command::tests::hotel();
        assert_eq!(cmd.render_help(), expected);
        assert_eq!(help_of(&cmd, &["--help"]), expected);
        assert_eq!(help_of(&cmd, &["-h"]), expected);
    }

    #[test]
    fn subcommand_snapshot() {
        let expected = "\
Usage: hotel serve [OPTIONS]

Run server

Options:
      --bind <BIND>  Bind address
  -h, --help         Print help
";
        let cmd = crate::command::tests::hotel();
        assert_eq!(help_of(&cmd, &["in.txt", "serve", "--help"]), expected);
    }

    #[test]
    fn globals_required_and_hidden() {
        let cmd = Command::new("tool")
            .arg(Arg::flag("debug").short('d').global().help("Debug output"))
            .arg(Arg::option("secret").hidden())
            .subcommand_required()
            .subcommand(
                Command::new("deploy")
                    .about("Deploy a release\nLonger second line that is not listed.")
                    .arg(
                        Arg::option("env")
                            .short('e')
                            .required()
                            .choices(["dev", "prod"]),
                    )
                    .arg(
                        Arg::option("tag")
                            .alias("version")
                            .alias("ref")
                            .help("Git tag"),
                    )
                    .arg(Arg::positional("targets").required().multiple()),
            )
            .subcommand(Command::new("status"));
        let root = "\
Usage: tool [OPTIONS] <COMMAND>

Options:
  -d, --debug  Debug output
  -h, --help   Print help

Commands:
  deploy       Deploy a release
  status
";
        assert_eq!(cmd.render_help(), root);
        let deploy = "\
Usage: tool deploy [OPTIONS] --env <ENV> <targets>...

Deploy a release
Longer second line that is not listed.

Arguments:
  <targets>...

Options:
  -e, --env <ENV>  [possible values: dev, prod]
      --tag <TAG>  Git tag [aliases: --version, --ref]
  -d, --debug      Debug output
  -h, --help       Print help
";
        assert_eq!(help_of(&cmd, &["deploy", "-h"]), deploy);
    }

    #[test]
    fn wraps_long_descriptions_and_wide_entries() {
        let long_help = "Path to the directory that holds the property management system \
                         exports; every file in it is imported in name order";
        let cmd = Command::new("w")
            .arg(Arg::option("dir").help(long_help))
            .arg(
                Arg::option("a-very-long-option-name-indeed")
                    .value_name("SOME_VALUE")
                    .help("Wide entry"),
            )
            .arg(Arg::flag("multi").help("First line\n\nThird line"));
        let expected = "\
Usage: w [OPTIONS]

Options:
      --dir <DIR>                       Path to the directory that holds the property management
                                        system exports; every file in it is imported in name order
      --a-very-long-option-name-indeed <SOME_VALUE>
                                        Wide entry
      --multi                           First line

                                        Third line
  -h, --help                            Print help
";
        let help = cmd.render_help();
        assert_eq!(help, expected);
        assert!(
            help.lines()
                .all(|l| l.chars().count() <= WIDTH && !l.ends_with(' '))
        );
    }

    #[test]
    fn user_switches_replace_builtins() {
        let cmd = Command::new("srv")
            .version("1.0")
            .arg(Arg::option("host").short('h'))
            .arg(Arg::flag("verbose").short('V'));
        let expected = "\
Usage: srv [OPTIONS]

Options:
  -h, --host <HOST>
  -V, --verbose
      --help         Print help
      --version      Print version
";
        assert_eq!(cmd.render_help(), expected);
        assert_eq!(help_of(&cmd, &["--help"]), expected);
    }

    #[test]
    fn minimal_command() {
        assert_eq!(
            Command::new("x").render_help(),
            "Usage: x [OPTIONS]\n\nOptions:\n  -h, --help  Print help\n"
        );
    }

    #[test]
    fn wrap_rules() {
        assert_eq!(wrap("a b c", 3), ["a b", "c"]);
        assert_eq!(wrap("abcdef g", 3), ["abcdef", "g"]);
        assert_eq!(wrap("one\n\ntwo", 10), ["one", "", "two"]);
        assert_eq!(wrap("  spaced   out  ", 20), ["spaced out"]);
        assert!(wrap("", 10).is_empty());
    }
}
