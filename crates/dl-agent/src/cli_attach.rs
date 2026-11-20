//! `attach --code CODE`: adds another product to an enrolled agent (PROTOCOL.md §2, RT-0298,
//! docs/decisions/0002-one-agent-many-products.md). The machine keeps its identity, tenant and
//! the modules it already runs; the hub merges the code's modules into the config, which is
//! applied here like an enrolment's first config (persisted, last-known-good kept).

use std::path::Path;

use dl_cli::{Arg, Command, Matches};

use crate::cli::EXIT_CONFIG;
use crate::cli_install::{EXIT_FAILED, Failure};
use crate::config::ConfigStore;
use crate::hub::{self, Attached, HttpTransport, HubClient, Transport};
use crate::identity::Identity;
use crate::{Error, now};

/// The sub-command name.
pub const COMMAND: &str = "attach";

/// The `attach` grammar; `data` is the shared `--data` option.
pub fn grammar(data: Arg) -> Command {
    Command::new(COMMAND)
        .about("Adds another product to this enrolled machine with that product's one-time code")
        .arg(
            Arg::option("code")
                .required()
                .env("DL_AGENT_ENROLL_CODE")
                .help("One-time code of the product to add (from its install line)"),
        )
        .arg(data)
}

/// Runs `attach`; returns the exit code.
pub fn main(service: &str, dir: &Path, m: &Matches) -> i32 {
    let code = m.value("code").unwrap_or("");
    match attach_with(dir, Box::new(HttpTransport::new()), code) {
        Ok(a) => {
            println!("{}", summary(&a));
            0
        }
        Err(e) => {
            eprintln!("{service}: attach failed: {e}");
            eprintln!("Next: {}", next_step(&e));
            EXIT_CONFIG
        }
    }
}

/// Sends `code` to the hub on this machine's identity in `dir` and applies the merged config.
/// `Error::NotEnrolled` without an identity (nothing is sent).
pub(crate) fn attach_with(
    dir: &Path,
    transport: Box<dyn Transport>,
    code: &str,
) -> Result<Attached, Error> {
    let identity = Identity::load(dir)?;
    let attached = HubClient::new(transport, identity).attach(code.trim())?;
    if let Some(config) = attached.config.clone() {
        ConfigStore::load(dir)?.apply(config, now())?;
    }
    Ok(attached)
}

/// The installer's step for an enrolled machine given a code (`install --hub … --code …`
/// without `--reenroll`). A code without modules (a plain enrolment code) keeps the identity as
/// before. Any other failure is returned for the end of the setup, so the service still comes
/// back with the products it had.
pub(crate) fn install_step(
    dir: &Path,
    code: &str,
    transport: Box<dyn Transport>,
) -> Option<Failure> {
    match attach_with(dir, transport, code) {
        Ok(a) => {
            println!("Already enrolled: keeping this machine's identity");
            println!("{}", summary(&a));
            None
        }
        Err(e) if hub::error_code(&e).as_deref() == Some("code_has_no_modules") => {
            println!(
                "Already enrolled: keeping this machine's identity (the code adds no product)"
            );
            None
        }
        Err(e) => {
            eprintln!("Warning: attach failed: {e}");
            Some(Failure::new(
                format!("attach failed: {e}; the agent keeps the products it had"),
                next_step(&e),
                EXIT_FAILED,
            ))
        }
    }
}

/// `Attached pda-sync to this agent (config version 3)`.
pub(crate) fn summary(a: &Attached) -> String {
    let products = if a.attached.is_empty() {
        "nothing".to_string()
    } else {
        a.attached.join(", ")
    };
    match &a.config {
        Some(c) => format!(
            "Attached {products} to this agent (config version {})",
            c.version
        ),
        None => format!("Attached {products} to this agent"),
    }
}

/// What to do after a failed attach.
pub(crate) fn next_step(e: &Error) -> &'static str {
    if matches!(e, Error::NotEnrolled) {
        return "this machine is not enrolled: run the product's install line (or enroll --hub <url> --code <code>) first";
    }
    match (e, hub::error_code(e).as_deref()) {
        (_, Some("enroll_code_invalid")) => {
            "the code is unknown, used by another machine, revoked or expired: ask for a new code and run the line again"
        }
        (_, Some("code_has_no_modules")) => {
            "this code adds no product: use the install line of the product to add"
        }
        (_, Some("revoked")) => {
            "this agent was revoked on the hub: run the install line with -Reenroll"
        }
        (Error::Status { code: 404, .. }, _) => {
            "the hub does not know attach yet: run the line again once the hub is updated"
        }
        _ => "check that the hub is reachable and run the line again",
    }
}

#[cfg(test)]
mod tests {
    use dl_json::json;

    use super::*;
    use crate::hub::tests_support::{FakeHub, identity};
    use crate::store::testdir::TestDir;

    fn enrolled(name: &str) -> TestDir {
        let d = TestDir::new(name);
        identity(&["https://hub.example"]).save(&d.0).unwrap();
        let v1 = json!({"version": 1, "body": {"modules": {"arrival": {"enabled": true}}}});
        let mut store = ConfigStore::load(&d.0).unwrap();
        store
            .apply(crate::config::RemoteConfig::from_json(&v1).unwrap(), 1)
            .unwrap();
        store.confirm_if_due(1_000_000).unwrap();
        d
    }

    const MERGED: &str = r#"{"config": {"version": 2, "body": {"modules": {"arrival": {"enabled": true}, "pda-sync": {"enabled": true}}}}, "attached": ["pda-sync"]}"#;

    #[test]
    fn grammar_reads_the_code_from_the_option_or_the_installer_env() {
        let cmd = grammar(Arg::option("data"));
        let m = cmd
            .parse_from_with_env(["attach", "--code", "ABCD-EFGH"], |_| None)
            .unwrap();
        assert_eq!(m.value("code"), Some("ABCD-EFGH"));
        let m = cmd
            .parse_from_with_env(["attach"], |k| {
                (k == "DL_AGENT_ENROLL_CODE").then(|| "FROM-ENV".to_string())
            })
            .unwrap();
        assert_eq!(m.value("code"), Some("FROM-ENV"));
        assert!(cmd.parse_from_with_env(["attach"], |_| None).is_err());
    }

    #[test]
    fn attach_applies_the_merged_config_and_keeps_the_identity() {
        let d = enrolled("attach-ok");
        let before = Identity::load(&d.0).unwrap();
        let hub = FakeHub::default();
        hub.push(200, MERGED);
        let a = attach_with(&d.0, Box::new(hub.clone()), " ABCD-EFGH ").unwrap();
        assert_eq!(
            summary(&a),
            "Attached pda-sync to this agent (config version 2)"
        );
        let seen = hub.seen();
        assert_eq!(seen[0].url, "https://hub.example/v1/attach");
        assert_eq!(
            dl_json::parse_slice(&seen[0].body).unwrap(),
            json!({"code": "ABCD-EFGH"})
        );
        let store = ConfigStore::load(&d.0).unwrap();
        assert_eq!(store.current.version, 2);
        assert!(store.current.module_enabled("arrival"));
        assert!(store.current.module_enabled("pda-sync"));
        assert_eq!(
            store.last_known_good.version, 1,
            "the previous config is the fallback"
        );
        assert!(store.trial_since.is_some());
        let after = Identity::load(&d.0).unwrap();
        assert_eq!(after.agent_id, before.agent_id);
        assert_eq!(after.public_key(), before.public_key());
    }

    #[test]
    fn attach_needs_an_identity_and_sends_nothing_without_one() {
        let d = TestDir::new("attach-none");
        let hub = FakeHub::default();
        let e = attach_with(&d.0, Box::new(hub.clone()), "ABCD-EFGH").unwrap_err();
        assert!(matches!(e, Error::NotEnrolled));
        assert!(next_step(&e).contains("not enrolled"));
        assert!(hub.seen().is_empty());
    }

    #[test]
    fn the_installer_step_defers_failures_and_keeps_plain_codes_quiet() {
        let d = enrolled("attach-install");
        let hub = FakeHub::default();
        hub.push(200, MERGED);
        assert!(install_step(&d.0, "ABCD-EFGH", Box::new(hub.clone())).is_none());

        hub.push(400, r#"{"error": "code_has_no_modules"}"#);
        assert!(install_step(&d.0, "PLAIN-CODE", Box::new(hub.clone())).is_none());

        hub.push(403, r#"{"error": "enroll_code_invalid"}"#);
        let f = install_step(&d.0, "USED-CODE", Box::new(hub.clone())).unwrap();
        assert_eq!(f.code, EXIT_FAILED);
        assert!(f.message.contains("keeps the products it had"));
        assert!(f.next.contains("ask for a new code"));

        hub.push(404, r#"{"error": "not_found"}"#);
        let f = install_step(&d.0, "ABCD-EFGH", Box::new(hub.clone())).unwrap();
        assert!(f.next.contains("hub is updated"));
        assert_eq!(
            ConfigStore::load(&d.0).unwrap().current.version,
            2,
            "failures change nothing"
        );
    }
}
