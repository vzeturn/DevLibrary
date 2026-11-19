//! Command line (port of `Hosting/Cli.cs`): `enroll`, `grants`, `diag`, `status`, the
//! legacy and update verbs; no verb (or `run`) starts the service host.

use super::verbs::{self, HostContext, VerbContext, VerbTable, dispatch};
use crate::config::options::{base_directory, host_configuration, site_settings_file};
use crate::config::{AgentOptions, AgentPaths};
use crate::util::console::{LineWriter, Stderr, Stdout};
use crate::util::stj::eq_ignore_case;
use std::ffi::OsString;

/// Success.
pub const EXIT_OK: i32 = 0;
/// The verb ran and failed (or refused).
pub const EXIT_FAILED: i32 = 1;
/// Invalid command line.
pub const EXIT_USAGE: i32 = 2;
/// An unexpected error (`legacy-upload`, ADR 0095): distinct from a refusal, which
/// writes nothing.
pub const EXIT_UNEXPECTED: i32 = 3;
/// What a .NET process exits with on an unhandled exception (`0xE0434352`): the C# agent
/// lets a few start-up failures (unreadable configuration, data folder that cannot be
/// created) escape `Main`, and so does this port.
pub const EXIT_UNHANDLED: i32 = 0xE043_4352_u32 as i32;

/// `StatusCommand.Usage`.
pub const STATUS_USAGE: &str = "status [--wait <seconds>]";
/// `LegacyExportOptions.Usage`.
pub const LEGACY_EXPORT_USAGE: &str = "legacy-export --out <dir> [--pda-db <path> | --from-sql] (--site-root <dir> | --app-pool <name> | --allow-live) [--sql-instance <instance>] [--database SMILE_POS] [--sql-user <login>]  |  legacy-export --out <dir> --vietqr-only [--site-root <dir>]";
/// `LegacyArchiveOptions.Usage`.
pub const LEGACY_ARCHIVE_USAGE: &str = "legacy-archive --in <bundle dir> [--dry-run] [--sql-instance <instance>] [--archive-db PDA_LEGACY_ARCHIVE]";
/// `LegacyUploadOptions.Usage`.
pub const LEGACY_UPLOAD_USAGE: &str = "legacy-upload --in <bundle dir> [--url <https cloud url>] [--dry-run] [--prefer-legacy] [--json <file>]";
/// `ApplyUpdateCommand.Usage`.
pub const APPLY_UPDATE_USAGE: &str = "apply-update --zip <zip> --sha256 <hex> --install-dir <dir> --result-file <file> [--wait <seconds>]";
/// `ExeUpdateLauncher.Verb`.
pub const APPLY_UPDATE_VERB: &str = "apply-update";

/// The usage line printed for an unknown verb.
pub fn usage() -> String {
    format!(
        "Usage: PdaSyncAgent [run | enroll --code <code> --url <url> [--dry-run] [--sql-instance <instance>] \
         [--database <name>] [--sql-admin-user <login>] | \
         grants [--sql-instance <instance>] [--database <name>] [--sql-admin-user <login>] [--dry-run] | diag [--out file.zip] | {STATUS_USAGE} | \
         {LEGACY_EXPORT_USAGE} | {LEGACY_ARCHIVE_USAGE} | {LEGACY_UPLOAD_USAGE} | {APPLY_UPDATE_USAGE}]"
    )
}

/// `--name value` options (`Cli.ParseOptions`): keys compare ordinal-ignore-case, a later
/// duplicate replaces the value, and a `--flag` with no value (followed by another
/// `--option` or nothing) is `"true"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    entries: Vec<(String, String)>,
}

impl Options {
    /// Parses every `--key [value]` in `args` (other words are skipped).
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Options {
        let mut map = Options::default();
        for (i, arg) in args.iter().enumerate() {
            let Some(key) = arg.as_ref().strip_prefix("--") else {
                continue;
            };
            let value = match args.get(i + 1) {
                Some(next) if !next.as_ref().starts_with("--") => next.as_ref(),
                _ => "true",
            };
            map.insert(key, value);
        }
        map
    }

    /// Sets `key` (keeping the first spelling of an existing key, like `Dictionary`).
    pub fn insert(&mut self, key: &str, value: &str) {
        match self
            .entries
            .iter_mut()
            .find(|(k, _)| eq_ignore_case(k, key))
        {
            Some(e) => e.1 = value.to_owned(),
            None => self.entries.push((key.to_owned(), value.to_owned())),
        }
    }

    /// The value of `key` (`TryGetValue` / `GetValueOrDefault`).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| eq_ignore_case(k, key))
            .map(|(_, v)| v.as_str())
    }

    /// Whether `key` was given (`ContainsKey`).
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The options in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Number of options.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The verb: the first argument that does not start with `-`, else `run`.
pub fn verb_of<S: AsRef<str>>(args: &[S]) -> &str {
    args.iter()
        .map(AsRef::as_ref)
        .find(|a| !a.starts_with('-'))
        .unwrap_or("run")
}

/// A start-up failure the C# agent does not catch.
pub type Unhandled = Box<dyn std::error::Error + Send + Sync>;

/// Writes `Unhandled exception. <error>` (as the .NET runtime does) and returns
/// [`EXIT_UNHANDLED`].
pub fn unhandled(stderr: &dyn LineWriter, error: &dyn std::fmt::Display) -> i32 {
    stderr.write_line(&format!("Unhandled exception. {error}"));
    EXIT_UNHANDLED
}

/// The host configuration, options and data folder (`Host.CreateApplicationBuilder` +
/// site settings + `--data-dir` + `paths.EnsureCreated()`).
pub fn build_host<S: AsRef<str>>(args: &[S], opts: &Options) -> Result<HostContext, Unhandled> {
    let configuration = host_configuration(args, &base_directory(), &site_settings_file())?;
    let mut options = AgentOptions::load(&configuration)?;
    if let Some(dd) = opts.get("data-dir") {
        options.data_dir = Some(dd.to_owned());
    }
    let paths = AgentPaths::new(options.data_dir_path()?);
    paths.ensure_created()?;
    Ok(HostContext {
        configuration,
        options,
        paths,
    })
}

/// Runs the agent with the command-line arguments (without the program name) and
/// returns the process exit code.
pub fn run(args: Vec<OsString>) -> i32 {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    run_with(&args, &Stdout, &Stderr, &verbs::table())
}

/// [`run`] with injectable console and verb table (tests).
pub fn run_with(
    args: &[String],
    stdout: &dyn LineWriter,
    stderr: &dyn LineWriter,
    table: &VerbTable,
) -> i32 {
    let verb = verb_of(args);
    let opts = Options::parse(args);
    let ctx = |host: Option<&HostContext>, f: Option<verbs::VerbFn>| {
        dispatch(
            f,
            &VerbContext {
                verb,
                args,
                opts: &opts,
                stdout,
                stderr,
                host,
            },
        )
    };
    // One-shot legacy verbs and the detached updater: before the host is built, so they
    // create no agent directory.
    match verb {
        "legacy-export" => return ctx(None, table.legacy_export),
        "legacy-archive" => return ctx(None, table.legacy_archive),
        "legacy-upload" => return ctx(None, table.legacy_upload),
        APPLY_UPDATE_VERB => return ctx(None, table.apply_update),
        _ => {}
    }
    let host = match build_host(args, &opts) {
        Ok(h) => h,
        Err(e) => return unhandled(stderr, &e),
    };
    match verb {
        "enroll" => ctx(Some(&host), table.enroll),
        "grants" => ctx(Some(&host), table.grants),
        "status" => super::status::run_console(&host.paths, &opts, stdout, stderr),
        "run" => ctx(Some(&host), table.run),
        _ => {
            stderr.write_line(&usage());
            EXIT_USAGE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_follow_cli_parse_options() {
        let o = Options::parse(&[
            "legacy-archive",
            "--in",
            "dir",
            "--DRY-RUN",
            "--wait",
            "-5",
            "--in",
            "other",
            "--last",
        ]);
        assert_eq!(o.get("in"), Some("other"));
        assert_eq!(o.get("dry-run"), Some("true"));
        assert_eq!(o.get("Wait"), Some("-5"));
        assert_eq!(o.get("last"), Some("true"));
        assert!(!o.contains("legacy-archive"));
        assert_eq!(o.len(), 4);
        assert_eq!(o.iter().next(), Some(("in", "other")));
        let flag_then_option = Options::parse(&["--dry-run", "--out", "x.zip"]);
        assert_eq!(flag_then_option.get("dry-run"), Some("true"));
        assert_eq!(flag_then_option.get("out"), Some("x.zip"));
        assert_eq!(Options::parse(&["--", "v"]).get(""), Some("v"));
        assert!(Options::parse::<&str>(&[]).is_empty());
    }

    #[test]
    fn the_verb_is_the_first_word_without_a_dash() {
        assert_eq!(verb_of::<&str>(&[]), "run");
        assert_eq!(verb_of(&["--data-dir"]), "run");
        assert_eq!(verb_of(&["-x", "status", "--wait", "5"]), "status");
        // As in C#: an option value before the verb is taken for the verb.
        assert_eq!(verb_of(&["--data-dir", "d", "status"]), "d");
    }

    #[test]
    fn usage_lists_every_verb() {
        let u = usage();
        assert!(u.starts_with("Usage: PdaSyncAgent [run | enroll --code <code> --url <url> [--dry-run] [--sql-instance <instance>] [--database <name>] [--sql-admin-user <login>] | grants "), "{u}");
        assert!(u.contains(
            " | diag [--out file.zip] | status [--wait <seconds>] | legacy-export --out <dir> "
        ));
        assert!(u.contains("[--archive-db PDA_LEGACY_ARCHIVE] | legacy-upload --in <bundle dir>"));
        assert!(u.ends_with("--result-file <file> [--wait <seconds>]]"));
        assert_eq!(EXIT_UNHANDLED, -532_462_766);
    }
}
