//! Sub-commands a module adds to the binary, for setup steps that run before the service
//! (for example `einvoice-enroll`, which exchanges an E-Invoice token for the module's own
//! credential). `install.ps1 -Setup <name>` runs one after enrolment; secrets reach it through
//! [`SETUP_ARGS_ENV`] instead of the command line, so they do not show in the process list.

use std::ffi::OsString;
use std::path::Path;

use dl_cli::{Command, Matches};

use crate::{Error, Version};

/// Extra arguments for a tool, whitespace separated, appended to the command line.
pub const SETUP_ARGS_ENV: &str = "DL_AGENT_SETUP_ARGS";

/// What a tool gets besides its parsed arguments.
#[derive(Debug)]
pub struct ToolContext<'a> {
    /// The agent's state directory (`--data`).
    pub data_dir: &'a Path,
    /// Release line of this binary, e.g. `dl-agent`.
    pub product: &'a str,
    /// Version of this binary.
    pub version: &'a Version,
}

/// One module sub-command.
#[derive(Clone, Copy)]
pub struct Tool {
    /// Sub-command name, e.g. `einvoice-enroll`.
    pub name: &'static str,
    /// Adds the about line and arguments to `Command::new(name)`; `--data` is added for it.
    pub grammar: fn(Command) -> Command,
    /// Runs the tool. An error exits with `EXIT_CONFIG` and prints the message.
    pub run: fn(&ToolContext<'_>, &Matches) -> Result<(), Error>,
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tool").field("name", &self.name).finish()
    }
}

/// `args` with the words of `setup_args` appended when `args[1]` names one of `tools`.
/// Built-in commands never take them, so a stray variable cannot change `enroll` or `run`.
pub fn with_setup_args(
    mut args: Vec<OsString>,
    tools: &[Tool],
    setup_args: Option<&str>,
) -> Vec<OsString> {
    let is_tool = args
        .get(1)
        .and_then(|a| a.to_str())
        .is_some_and(|name| tools.iter().any(|t| t.name == name));
    if let (true, Some(extra)) = (is_tool, setup_args) {
        args.extend(extra.split_whitespace().map(OsString::from));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> Tool {
        Tool {
            name: "demo-setup",
            grammar: |c| c,
            run: |_, _| Ok(()),
        }
    }

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn appends_setup_args_to_tools_only() {
        let tools = [tool()];
        let extra = Some("  --token t1\t--url https://x ");
        assert_eq!(
            with_setup_args(os(&["a", "demo-setup", "--data", "/d"]), &tools, extra),
            os(&[
                "a",
                "demo-setup",
                "--data",
                "/d",
                "--token",
                "t1",
                "--url",
                "https://x"
            ])
        );
        assert_eq!(
            with_setup_args(os(&["a", "enroll", "--code", "X"]), &tools, extra),
            os(&["a", "enroll", "--code", "X"])
        );
        assert_eq!(
            with_setup_args(os(&["a", "demo-setup"]), &tools, None),
            os(&["a", "demo-setup"])
        );
        assert_eq!(with_setup_args(os(&["a"]), &tools, extra), os(&["a"]));
    }
}
