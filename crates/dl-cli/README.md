# dl-cli

Command-line argument parser for Rust with flags, options, positional arguments, nested
subcommands, environment-variable fallbacks, defaults, typed values, "did you mean"
suggestions and generated help. No dependencies outside the standard library.

Part of [DevLibrary](https://github.com/vzeturn/DevLibrary), a set of zero-dependency Rust
libraries.

## Replaces

| Ecosystem | Package |
|---|---|
| Rust | `clap` (builder API) |
| Node.js / TypeScript | `commander` |
| .NET | `System.CommandLine` |
| Go | `cobra` / `pflag` |
| Python | `argparse` |

## Features

- Flags (`--verbose`, `-v`), counted flags (`-vvv`), options with values, positional
  arguments, repeated (`multiple`) options and positionals.
- GNU-style syntax: `--name=value`, `-nvalue`, short clusters (`-vp8080`), `--` to end
  options, `-` as a positional, values that start with `-` (`--offset -5`).
- Long names default to the argument name; extra long aliases, hidden arguments.
- Environment-variable fallbacks and defaults, with a fixed precedence
  (command line, then environment, then default) and a way to ask which one won.
- Restricted values (`choices`) and typed access through `FromStr`.
- Nested subcommands and global arguments shared by every level below.
- Errors with a kind, the argument position and a suggestion within two edits
  (`did you mean '--port'?`); `exit()` prints them and uses the conventional exit codes.
- Automatic `-h`/`--help` and `-V`/`--version`, deterministic help text aligned in
  columns and wrapped at 100 columns.
- Invalid user input never panics. Non-UTF-8 arguments are reported, not lost.
- Injectable environment lookup, so tests never have to mutate the process environment.
- Builds for `wasm32-unknown-unknown`; no `unsafe` code.

## Installation

From GitHub:

```toml
[dependencies]
dl-cli = { git = "https://github.com/vzeturn/DevLibrary" }
```

From a local checkout:

```toml
[dependencies]
dl-cli = { path = "../DevLibrary/crates/dl-cli" }
```

Inside the DevLibrary workspace, use `dl-cli.workspace = true`.

## Quick start

`src/main.rs`:

```rust,no_run
use dl_cli::{Arg, Command, Error, ErrorKind};

fn cli() -> Command {
    Command::new("hotel")
        .version("1.2.0")
        .about("Hotel PMS tools")
        .arg(Arg::flag("verbose").short('v').help("More output").count())
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
                .default("text")
                .help("Output format"),
        )
        .arg(Arg::option("include").short('I').multiple().help("Extra search directory"))
        .arg(Arg::positional("input").required().help("Input file"))
        .arg(Arg::positional("rest").multiple().help("More input files"))
        .subcommand(Command::new("serve").about("Run server"))
}

fn main() {
    // Prints help, version or the error and exits on any of them.
    let m = cli().parse_env_or_exit();

    let port: u16 = m.required("port").unwrap_or_else(|e| e.exit());
    if port < 1024 {
        Error::new(ErrorKind::InvalidValue, format!("port {port} is privileged")).exit();
    }
    if m.count("verbose") > 1 {
        eprintln!("include path: {:?}", m.values("include"));
    }
    match m.subcommand() {
        Some(("serve", _serve)) => println!("serving on port {port}"),
        _ => println!(
            "reading {} as {}",
            m.value("input").unwrap_or_default(),
            m.value("format").unwrap_or_default()
        ),
    }
}

#[cfg(test)]
mod tests {
    // Catches definition mistakes (duplicate names, ...) in `cargo test`.
    #[test]
    fn cli_is_valid() {
        super::cli().debug_assert();
    }
}
```

```text
$ hotel -vv --port=9000 file.txt
include path: []
reading file.txt as text
$ hotel --prot 9000 file.txt
error: unknown option '--prot'; did you mean '--port'?

Usage: hotel [OPTIONS] <input> [rest]... [COMMAND]

For more information, try 'hotel --help'.
```

## Guide

Every argument has a *name*. It is the key for reading the result from `Matches`, and
for flags and options it is also the default long name (`Arg::option("port")` is
`--port`). The examples below use `parse_from_with_env(argv, |_| None)`, which parses an
explicit argument list with an empty environment; `argv[0]` is the program name and is
skipped.

### Flags

A flag is a switch without a value. Repeating a flag is an error unless it is `count()`
(or `multiple()`); `Matches::count` tells how often it was given.

```rust
use dl_cli::{Arg, Command, ErrorKind};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("app")
        .arg(Arg::flag("dry-run").short('n').help("Show what would be done"))
        .arg(Arg::flag("verbose").short('v').count().help("More output, repeatable"));

    let m = cmd.parse_from_with_env(["app", "-vvn", "--verbose"], |_| None)?;
    assert!(m.flag("dry-run"));
    assert_eq!(m.count("verbose"), 3);

    let err = cmd.parse_from_with_env(["app", "-n", "--dry-run"], |_| None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::DuplicateOption);

    let err = cmd.parse_from_with_env(["app", "--dry-run=yes"], |_| None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnexpectedValue);
    Ok(())
}
```

### Options

An option takes a value. `long()` replaces the long name, `alias()` adds more,
`value_name()` sets the placeholder shown in help (default: the upper-cased name), and
`multiple()` collects every occurrence in order. An option always consumes the next
argument, even when it starts with `-`.

```rust
use dl_cli::{Arg, Command};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("app")
        .arg(Arg::option("output").short('o').long("out").alias("output").value_name("FILE"))
        .arg(Arg::option("include").short('I').multiple())
        .arg(Arg::option("offset"))
        .arg(Arg::option("token").hidden()); // accepted, but not shown in help

    let m = cmd.parse_from_with_env(
        ["app", "--output=report.txt", "-I", "src", "-Itests", "--include", "benches", "--offset", "-5"],
        |_| None,
    )?;
    assert_eq!(m.value("output"), Some("report.txt"));
    assert_eq!(m.values("include"), ["src", "tests", "benches"]);
    assert_eq!(m.value("offset"), Some("-5"));
    assert_eq!(m.value("token"), None);
    Ok(())
}
```

### Environment variables

`env("NAME")` reads the value from an environment variable when the argument is not on
the command line. Empty variables are ignored. For flags the variable must be `1`/`0`,
`true`/`false`, `yes`/`no` or `on`/`off` (any case).

`parse_from` and `parse_env_or_exit` read the process environment. In tests, pass a lookup
function to `parse_from_with_env` instead; this avoids `std::env::set_var`, which is
`unsafe` in edition 2024.

```rust
use dl_cli::{Arg, Command, ValueSource};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("hotel")
        .arg(Arg::option("port").env("HOTEL_PORT").default("8080"))
        .arg(Arg::flag("debug").env("HOTEL_DEBUG"));
    let env = |name: &str| match name {
        "HOTEL_PORT" => Some("9000".to_string()),
        "HOTEL_DEBUG" => Some("yes".to_string()),
        _ => None,
    };

    let m = cmd.parse_from_with_env(["hotel"], env)?;
    assert_eq!(m.value("port"), Some("9000"));
    assert_eq!(m.source("port"), Some(ValueSource::Env));
    assert!(m.flag("debug"));

    // The command line wins over the environment.
    let m = cmd.parse_from_with_env(["hotel", "--port", "7000"], env)?;
    assert_eq!(m.source("port"), Some(ValueSource::CommandLine));
    Ok(())
}
```

### Defaults and precedence

A value is taken from the command line, then the environment variable, then the default.
`required()` is checked after all three, so a required argument with a default or a set
environment variable is satisfied. `Matches::source` reports which one provided the value.

```rust
use dl_cli::{Arg, Command, ErrorKind, ValueSource};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("app")
        .arg(Arg::option("port").default("8080"))
        .arg(Arg::option("token").env("APP_TOKEN").required());

    let m = cmd.parse_from_with_env(["app"], |name| {
        (name == "APP_TOKEN").then(|| "s3cret".to_string())
    })?;
    assert_eq!(m.value("port"), Some("8080"));
    assert_eq!(m.source("port"), Some(ValueSource::Default));
    assert_eq!(m.count("port"), 0); // defaults are not occurrences

    let err = cmd.parse_from_with_env(["app"], |_| None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::MissingRequired);
    assert_eq!(err.to_string(), "missing required argument '--token <TOKEN>'");
    Ok(())
}
```

### Choices

`choices()` restricts the value; it applies to command-line and environment values.

```rust
use dl_cli::{Arg, Command, ErrorKind};

let cmd = Command::new("app").arg(Arg::option("format").choices(["json", "text"]).default("text"));

let err = cmd.parse_from_with_env(["app", "--format", "jsn"], |_| None).unwrap_err();
assert_eq!(err.kind(), ErrorKind::InvalidChoice);
assert_eq!(err.suggestion(), Some("json"));
assert_eq!(
    err.to_string(),
    "invalid value 'jsn' for '--format <FORMAT>' [possible values: json, text]; did you mean 'json'?"
);
```

### Positional arguments

Positionals are filled in definition order. Only the last one can be `multiple()`, and a
required positional cannot follow an optional one. After `--` every argument is
positional, even if it looks like an option or a subcommand name; a lone `-` is always a
positional. A negative number such as `-5` is a positional too, unless the command has a
short option that is a digit.

```rust
use dl_cli::{Arg, Command, ErrorKind};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("cat")
        .arg(Arg::flag("number").short('n'))
        .arg(Arg::positional("first").required())
        .arg(Arg::positional("files").multiple());

    let m = cmd.parse_from_with_env(["cat", "a.txt", "-", "-n", "--", "-weird.txt"], |_| None)?;
    assert_eq!(m.value("first"), Some("a.txt"));
    assert_eq!(m.values("files"), ["-", "-weird.txt"]);
    assert!(m.flag("number"));

    let two = Command::new("mv").arg(Arg::positional("from")).arg(Arg::positional("to"));
    let err = two.parse_from_with_env(["mv", "a", "b", "c"], |_| None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnexpectedPositional);
    assert_eq!(err.index(), Some(3));
    Ok(())
}
```

### Subcommands

Subcommands nest to any depth. An argument equal to a subcommand name selects it, and the
arguments after it belong to the subcommand. `Matches::subcommand` returns the selected
name and its matches. `subcommand_required()` makes a missing subcommand an error.

```rust
use dl_cli::{Arg, Command, ErrorKind};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("hotel")
        .subcommand_required()
        .subcommand(Command::new("serve").about("Run server"))
        .subcommand(
            Command::new("db")
                .about("Database tools")
                .subcommand(Command::new("migrate").arg(Arg::option("to").required())),
        );

    let m = cmd.parse_from_with_env(["hotel", "db", "migrate", "--to", "42"], |_| None)?;
    if let Some(("db", db)) = m.subcommand() {
        if let Some(("migrate", migrate)) = db.subcommand() {
            assert_eq!(migrate.required::<u32>("to")?, 42);
        }
    }

    let err = cmd.parse_from_with_env(["hotel", "srve"], |_| None).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::UnknownSubcommand);
    assert_eq!(err.to_string(), "unknown subcommand 'srve'; did you mean 'serve'?");
    Ok(())
}
```

Options of a parent are not accepted after the subcommand name unless they are global.
The parent's own required arguments still apply when a subcommand is selected.

### Global arguments

A `global()` flag or option is accepted by the command that defines it and by every
subcommand below it, and its value is visible in the matches of all those levels. It has
one shared value: a non-multiple global given at two levels is a `DuplicateOption` error.

```rust
use dl_cli::{Arg, Command};

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("hotel")
        .arg(Arg::flag("verbose").short('v').count().global())
        .arg(Arg::option("config").short('c').global())
        .subcommand(Command::new("serve"));

    let m = cmd.parse_from_with_env(["hotel", "-v", "serve", "-v", "-c", "prod.toml"], |_| None)?;
    let (_, serve) = m.subcommand().expect("serve was given");
    assert_eq!(serve.count("verbose"), 2);
    assert_eq!(m.count("verbose"), 2);
    assert_eq!(m.value("config"), Some("prod.toml"));
    Ok(())
}
```

### Typed values

Values are stored as strings. `get::<T>()`, `get_all::<T>()` and `required::<T>()` convert
them with `FromStr`, so any type that implements it works. A failed conversion is an
`InvalidValue` error that names the argument, where the value came from and the parser's
message.

```rust
use dl_cli::{Arg, Command, ErrorKind};
use std::net::IpAddr;

fn main() -> Result<(), dl_cli::Error> {
    let cmd = Command::new("app")
        .arg(Arg::option("bind").default("127.0.0.1"))
        .arg(Arg::option("ratio"))
        .arg(Arg::option("id").multiple())
        .arg(Arg::option("port"));

    let m = cmd.parse_from_with_env(
        ["app", "--ratio", "0.5", "--id", "1", "--id", "2", "--port", "80x"],
        |_| None,
    )?;
    let bind: IpAddr = m.required("bind")?;
    assert!(bind.is_loopback());
    assert_eq!(m.get::<f64>("ratio")?, Some(0.5));
    assert_eq!(m.get_all::<u32>("id")?, [1, 2]);

    let err = m.get::<u16>("port").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidValue);
    assert_eq!(err.index(), Some(8));
    assert_eq!(err.to_string(), "invalid value '80x' for '--port <PORT>': invalid digit found in string");
    Ok(())
}
```

### Error handling and exit codes

Parsing returns `Result<Matches, Error>`. Help and version requests are returned as
errors of kind `DisplayHelp` and `DisplayVersion`, so the caller decides what to do with
them. `Error::exit` handles all cases the conventional way:

| Kind | Printed to | Exit code |
|---|---|---|
| `DisplayHelp`, `DisplayVersion` | stdout, text as is | 0 |
| every other kind | stderr, `error: <message>` plus the usage line | 2 |

`parse_env_or_exit` parses `std::env::args_os` and calls `exit` for you. Use `Error::new`
to report your own validation errors the same way.

```rust
use dl_cli::{Arg, Command, ErrorKind};

let cmd = Command::new("hotel").arg(Arg::option("port").short('p'));

match cmd.parse_from_with_env(["hotel", "--prot", "9000"], |_| None) {
    Ok(_) => unreachable!(),
    Err(err) if err.exit_code() == 0 => print!("{err}"), // help or version text
    Err(err) => {
        assert_eq!(err.kind(), ErrorKind::UnknownOption);
        assert_eq!(err.index(), Some(1)); // position in argv, program name = 0
        assert_eq!(err.suggestion(), Some("--port"));
        assert_eq!(
            err.render(),
            "error: unknown option '--prot'; did you mean '--port'?\n\
             \n\
             Usage: hotel [OPTIONS]\n\
             \n\
             For more information, try 'hotel --help'.\n"
        );
        // In a binary: err.exit();
    }
}
```

| `ErrorKind` | Cause |
|---|---|
| `UnknownOption` | `--name` or `-n` that the command does not define |
| `UnknownSubcommand` | a word that is no subcommand, when no positional slot is free |
| `MissingValue` | an option that takes a value is the last argument |
| `MissingRequired` | a required argument or subcommand is missing from every source |
| `InvalidValue` | `FromStr` conversion failed, or a flag's environment variable is not a boolean |
| `InvalidChoice` | a value outside `choices()` |
| `UnexpectedValue` | a flag given a value: `--verbose=yes`, `-v=1` |
| `UnexpectedPositional` | more positionals than the command accepts |
| `DuplicateOption` | a non-multiple flag or option given twice |
| `InvalidUtf8` | an argument (`index()` tells which) or environment variable is not UTF-8 |
| `DisplayHelp` | `-h` / `--help` |
| `DisplayVersion` | `-V` / `--version` |

Arguments are processed in order, so `--help` after an invalid argument reports the
invalid argument.

### Generated help

`-h`/`--help` and `Command::render_help` produce the same deterministic text: the usage
line, the `about` text, then `Arguments:`, `Options:` and `Commands:` sections. The
descriptions start in a shared column and wrap at 100 columns; an entry wider than 36
characters puts its description on the next line. Defaults, environment variables,
aliases and choices are listed after the description. Hidden arguments are left out. The
help of a subcommand includes the global arguments it inherits.

For the command from the quick start:

```rust
use dl_cli::{Arg, Command};

let cmd = Command::new("hotel")
    .version("1.2.0")
    .about("Hotel PMS tools")
    .arg(Arg::flag("verbose").short('v').help("More output").count())
    .arg(Arg::option("port").short('p').value_name("PORT").default("8080").env("HOTEL_PORT").help("Listen port"))
    .arg(Arg::option("format").choices(["json", "text"]).default("text").help("Output format"))
    .arg(Arg::option("include").short('I').multiple().help("Extra search directory"))
    .arg(Arg::positional("input").required().help("Input file"))
    .arg(Arg::positional("rest").multiple().help("More input files"))
    .subcommand(Command::new("serve").about("Run server"));

assert_eq!(cmd.render_help(), "\
Usage: hotel [OPTIONS] <input> [rest]... [COMMAND]

Hotel PMS tools

Arguments:
  <input>                     Input file
  [rest]...                   More input files

Options:
  -v, --verbose...            More output
  -p, --port <PORT>           Listen port [default: 8080] [env: HOTEL_PORT]
      --format <FORMAT>       Output format [default: text] [possible values: json, text]
  -I, --include <INCLUDE>...  Extra search directory
  -h, --help                  Print help
  -V, --version               Print version

Commands:
  serve                       Run server
");

let version = cmd.parse_from_with_env(["hotel", "-V"], |_| None).unwrap_err();
assert_eq!(version.to_string(), "hotel 1.2.0\n");
```

The program name in the help and error text is the name given to `Command::new`, not
`argv[0]`, so the output does not depend on how the binary was started. An argument that
defines `-h`, `--help`, `-V` or `--version` itself takes over that switch from the
automatic one.

## Syntax

| Form | Meaning |
|---|---|
| `--name`, `-n` | a flag |
| `--name value`, `--name=value` | an option value |
| `-n value`, `-nvalue`, `-n=value` | an option value, short form |
| `-abc` | clustered short flags |
| `-vp8080`, `-vp 8080` | a cluster whose last switch takes a value |
| `--` | every following argument is positional |
| `-` | a positional (the usual "standard input" placeholder) |
| `-5`, `-1.5` | a positional, unless a short option is a digit |

- An option that takes a value always consumes the next argument: `--offset -5`,
  `--name --`, `-p -h`.
- In a cluster, the first switch that takes a value takes the rest of the cluster:
  `-pv` sets the option `-p` to `v`.
- Long names match exactly; there are no abbreviations (`--verb` is not `--verbose`).
- `--name=` gives the empty string as the value.

## Definition mistakes

A wrong *definition* is a bug in the program, not a user error, and panics with a
message that names the command and the argument. `Command::debug_assert` runs these
checks; it runs at the start of every parse in debug builds and is skipped in release
builds, so call it from a unit test (see the quick start). It rejects:

- duplicate argument names, short names or long names and aliases (including those of
  inherited global arguments), duplicate subcommand names;
- empty names, long names that start with `-` or contain `=`, subcommand names that start
  with `-`;
- a multiple positional that is not last, a required positional after an optional one;
- `count()` on a non-flag; a default, choices or value name on a flag; a default that is
  not one of the choices; a positional with short or long names; a global positional.

Reading a name that the command does not define (`m.value("prot")`) panics in debug
builds and returns "absent" in release builds.

## API overview

| Item | Purpose |
|---|---|
| `Command::new(name)` | a command; `.version()`, `.about()`, `.arg()`, `.subcommand()`, `.subcommand_required()` |
| `Command::parse_from(argv)` | parse an explicit argument list with the process environment |
| `Command::parse_from_with_env(argv, lookup)` | parse with an injected environment lookup |
| `Command::parse_env()` / `parse_env_or_exit()` | parse `std::env::args_os`; the second prints and exits on errors |
| `Command::render_help()` / `debug_assert()` | the help text / check the definition |
| `Arg::flag(name)`, `Arg::option(name)`, `Arg::positional(name)` | the three kinds of argument |
| `.short(c)`, `.long(s)`, `.alias(s)`, `.value_name(s)`, `.help(s)` | names and help text |
| `.default(v)`, `.env(var)`, `.choices([..])` | value sources and restrictions |
| `.required()`, `.multiple()`, `.count()`, `.hidden()`, `.global()` | behavior |
| `Matches::flag`, `count`, `value`, `values`, `source` | raw results |
| `Matches::get::<T>`, `get_all::<T>`, `required::<T>` | typed results via `FromStr` |
| `Matches::subcommand()` | `Option<(&str, &Matches)>` for the selected subcommand |
| `Error::kind`, `index`, `suggestion`, `exit_code`, `render`, `exit` | error details and reporting |
| `Error::new(kind, message)` | report your own validation errors the same way |
| `ErrorKind`, `ValueSource` | error categories / where a value came from |

## Platform support

- Linux, macOS and Windows: everything. Arguments are read with `std::env::args_os`, so a
  non-UTF-8 argument becomes an `InvalidUtf8` error instead of a panic.
- `wasm32-unknown-unknown`: the crate builds, and `parse_from_with_env` and
  `render_help` work as on other targets. That target has no process arguments,
  environment or exit, so `parse_env` sees an empty argument list, `parse_from` sees no
  environment variables, and `Error::exit` aborts; use `Error::render` there.

Help widths are counted in `char`s, so double-width CJK characters and combining marks
can misalign columns. Arguments are not split on `,` or expanded from `@files`.

## Comparison with clap

dl-cli covers the common subset of clap's builder API with a smaller surface and no
dependencies. Intentionally not supported:

- derive macros (`#[derive(Parser)]`); the builder API is the only interface;
- shell completion scripts (on the roadmap);
- a `help` subcommand (`app help serve`); use `app serve --help`;
- argument groups, conflicts and `requires` rules; check them after parsing and report
  with `Error::new`;
- options with optional values (`--color[=WHEN]`), several values per occurrence
  (`--pair a b`), value delimiters (`--tags a,b`) and `--no-flag` negation;
- abbreviated long options and inferred subcommands;
- colored output and terminal-width detection; help always wraps at 100 columns;
- `OsString` values; every value must be UTF-8.

Like clap 4, a non-multiple option given twice is an error, and `-V`/`--version` exist
only on commands that set a version.

## License

Licensed under either of MIT or Apache-2.0, at your option.
