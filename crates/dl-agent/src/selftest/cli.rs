//! `selftest [--data <dir>] [--module <name>] [--json] [--no-upload] [--trigger cli|install]`:
//! runs the self-test from the command line, prints it, uploads it and exits 1 on any `fail`.

use std::path::Path;

use dl_cli::{Arg, Command, Matches};
use dl_json::{Value, json};

use super::execute::{execute, module_defaults, upload_note};
use super::{CORE, Run, Status, Trigger};
use crate::cli::{App, EXIT_CONFIG};

/// Exit code when at least one check failed.
pub const EXIT_FAILED: i32 = 1;

/// The sub-command grammar; `data` is the shared `--data` option.
pub fn grammar(data: Arg) -> Command {
    Command::new(super::COMMAND)
        .about("Checks the agent and every enrolled module (read only) and sends the result to the hub")
        .arg(Arg::option("module").help("Only `core` or this module"))
        .arg(Arg::flag("json").help("Print the run as JSON"))
        .arg(Arg::flag("no-upload").help("Do not send the run to the hub"))
        .arg(Arg::option("trigger").help("What started the run: cli (default) or install"))
        .arg(data)
}

/// The plain-text table: status, module, title, detail, then the remedy under it.
pub fn table(run: &Run) -> String {
    let width = |f: fn(&super::Check) -> usize, min: usize| {
        run.checks.iter().map(f).max().unwrap_or(0).max(min)
    };
    let module_w = width(|c| c.module.chars().count(), 6);
    let title_w = width(|c| c.title.chars().count(), 5);
    let indent = 8 + module_w + 2 + title_w + 2;
    let mut out = format!(
        "{:<8}{:<module_w$}  {:<title_w$}  DETAIL\n",
        "STATUS", "MODULE", "CHECK"
    );
    for c in &run.checks {
        out.push_str(&format!(
            "{:<8}{:<module_w$}  {:<title_w$}  {}\n",
            c.status.as_str(),
            c.module,
            c.title,
            c.detail
        ));
        if let Some(r) = &c.remedy {
            out.push_str(&format!("{:indent$}remedy: {r}\n", ""));
        }
    }
    out.push_str(&format!(
        "\n{} pass, {} warn, {} fail, {} skip\n",
        run.count(Status::Pass),
        run.count(Status::Warn),
        run.count(Status::Fail),
        run.count(Status::Skip)
    ));
    out
}

/// Exit code of a finished run.
pub fn exit_code(run: &Run) -> i32 {
    if run.failed() { EXIT_FAILED } else { 0 }
}

/// Runs the sub-command; returns the exit code.
pub fn main(app: &App, dir: &Path, m: &Matches) -> i32 {
    let trigger = match m
        .value("trigger")
        .map_or(Some(Trigger::Cli), Trigger::parse)
    {
        Some(t @ (Trigger::Cli | Trigger::Install)) => t,
        _ => {
            eprintln!("--trigger must be cli or install");
            return EXIT_CONFIG;
        }
    };
    let only = m.value("module");
    let known = module_defaults(app);
    if let Some(o) = only
        && o != CORE
        && !known.iter().any(|(n, _)| n == o)
    {
        let names: Vec<&str> = known.iter().map(|(n, _)| n.as_str()).collect();
        eprintln!("unknown module {o} (known: core, {})", names.join(", "));
        return EXIT_CONFIG;
    }
    let run = execute(app, dir, trigger, only, &Value::Null);
    let note = if m.flag("no-upload") {
        "not uploaded (--no-upload)".to_string()
    } else {
        upload_note(&run, dir)
    };
    if m.flag("json") {
        let doc = json!({"run": run.to_json(), "summary": run.summary(), "upload": note});
        println!("{}", dl_json::to_string_pretty(&doc));
    } else {
        print!("{}", table(&run));
        println!("{note}");
    }
    exit_code(&run)
}
