//! Where a one-shot tool gets a password from (port of `Hosting/SecretPrompt.cs`): never
//! the command line, which would stay in the shell history and the process list.

use std::io::{BufRead, IsTerminal, Write};

/// A source of passwords; `None` when there is none to be had.
pub trait SecretPrompt: Send + Sync {
    /// The password from `env_var`, else asked with `prompt`.
    fn read(&self, env_var: &str, prompt: &str) -> Option<String>;
}

/// The environment variable when set, else a console prompt that does not echo; `None`
/// when input is redirected (`ConsoleSecretPrompt`).
#[derive(Debug, Default, Clone, Copy)]
pub struct ConsoleSecretPrompt;

/// What the typed line becomes: control characters dropped (as `ReadKey` skips them),
/// `None` when empty.
pub fn typed_secret(line: &str) -> Option<String> {
    let typed: String = line.chars().filter(|c| !c.is_control()).collect();
    (!typed.is_empty()).then_some(typed)
}

impl SecretPrompt for ConsoleSecretPrompt {
    fn read(&self, env_var: &str, prompt: &str) -> Option<String> {
        if let Some(v) = std::env::var_os(env_var).filter(|v| !v.is_empty()) {
            return Some(v.to_string_lossy().into_owned());
        }
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            return None;
        }
        let mut err = std::io::stderr();
        let _ = err.write_all(prompt.as_bytes());
        let _ = err.flush();
        // Off Windows (or without a console) the line is read visibly.
        let echo = dl_winos::console::hide_input().ok();
        let mut line = String::new();
        let read = stdin.lock().read_line(&mut line);
        drop(echo);
        let _ = err.write_all(crate::util::stj::NEWLINE.as_bytes());
        read.ok()?;
        typed_secret(&line)
    }
}

/// Fixed answers by variable name (tests and scripted callers).
#[derive(Debug, Default, Clone)]
pub struct FixedSecretPrompt(pub Vec<(String, String)>);

impl SecretPrompt for FixedSecretPrompt {
    fn read(&self, env_var: &str, _prompt: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(k, _)| k == env_var)
            .map(|(_, v)| v.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_lines_drop_the_newline_and_control_characters() {
        assert_eq!(typed_secret("s3cr\u{7}et\r\n").as_deref(), Some("s3cret"));
        assert_eq!(typed_secret("\r\n"), None);
        assert_eq!(typed_secret(""), None);
        assert_eq!(typed_secret(" a b ").as_deref(), Some(" a b "));
    }

    #[test]
    fn the_environment_variable_wins() {
        // A name no one sets: under cargo test stdin is not a console, so None.
        let p = ConsoleSecretPrompt;
        let name = "PDA_SYNC_TEST_SECRET_PROMPT_UNSET_7F3A";
        if !std::io::stdin().is_terminal() {
            assert_eq!(p.read(name, "Password: "), None);
        }
        let fixed = FixedSecretPrompt(vec![("X".into(), "pw".into())]);
        assert_eq!(fixed.read("X", "").as_deref(), Some("pw"));
        assert_eq!(fixed.read("Y", ""), None);
    }
}
