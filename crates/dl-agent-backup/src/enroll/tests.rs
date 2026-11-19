//! `backup-enroll`: the key from env or stdin, the sealed enrollment, `--force` and the
//! origin checks, against the fake hub.

use std::io::Cursor;
use std::path::Path;

use super::goagent::GoSource;
use super::tool::{KEY_ENV, Machine, NAME, backup_enroll_tool, run_args};
use super::*;
use crate::testhub::{FakeHub, TOKEN, default_config};
use crate::testutil::temp_dir;

/// Runs the tool body with `env_key` as `DL_AGENT_BACKUP_KEY` and `stdin`; returns stdout.
fn tool(
    dir: &Path,
    hub: &str,
    force: bool,
    env_key: Option<&str>,
    stdin: &str,
) -> Result<String, Error> {
    let env = |k: &str| {
        if k == KEY_ENV {
            env_key.map(str::to_owned)
        } else {
            None
        }
    };
    let mut input = Cursor::new(stdin.as_bytes().to_vec());
    let mut out = Vec::new();
    run_args(
        dir,
        hub,
        force,
        &env,
        &mut input,
        &mut out,
        &Machine {
            name: "SQL-HOST",
            go: &no_go(),
        },
    )?;
    Ok(String::from_utf8(out).unwrap())
}

/// A machine without the Go agent.
pub(super) fn no_go() -> GoSource {
    GoSource::none()
}

#[test]
fn the_tool_is_backup_enroll() {
    assert_eq!(backup_enroll_tool().name, NAME);
    assert_eq!(NAME, "backup-enroll");
}

#[test]
fn the_key_from_the_environment_enrolls_and_is_sealed_with_the_token() {
    let hub = FakeHub::start(default_config());
    let dir = temp_dir("enroll-env");
    let out = tool(
        &dir,
        &format!("{}/", hub.url()),
        false,
        Some(" tenant-key "),
        "",
    )
    .unwrap();
    assert_eq!(out, "enrolled as agent agent-1\n");
    assert_eq!(hub.bearers("/self-register"), ["Bearer tenant-key"]);

    let e = Enrollment::load(&dir).unwrap().unwrap();
    assert_eq!(
        (e.hub_url.as_str(), e.agent_id.as_str(), e.token.as_str()),
        (hub.url().as_str(), "agent-1", TOKEN)
    );
    assert!(e.hwid.parse::<dl_uuid::Uuid>().is_ok(), "{}", e.hwid);
    assert!(!format!("{e:?}").contains(TOKEN), "Debug hides the token");
    assert!(enrollment_path(&dir).ends_with("backup/enrollment.enc"));
}

#[test]
fn without_the_variable_the_key_is_read_from_stdin() {
    let hub = FakeHub::start(default_config());
    let dir = temp_dir("enroll-stdin");
    let out = tool(&dir, &hub.url(), false, Some("  "), "piped-key\n").unwrap();
    assert!(out.starts_with("enroll key: \n"), "{out}");
    assert!(!out.contains("piped-key"), "the key is not echoed");
    assert_eq!(hub.bearers("/self-register"), ["Bearer piped-key"]);

    let err = tool(&temp_dir("enroll-none"), &hub.url(), false, None, "\n").unwrap_err();
    assert!(err.to_string().contains(KEY_ENV), "{err}");
    assert_eq!(hub.count("/self-register"), 1);
}

#[test]
fn an_existing_enrollment_is_kept_unless_forced_and_keeps_its_machine_id() {
    let hub = FakeHub::start(default_config());
    let dir = temp_dir("enroll-again");
    tool(&dir, &hub.url(), false, Some("k"), "").unwrap();
    let first = Enrollment::load(&dir).unwrap().unwrap();

    let out = tool(&dir, &hub.url(), false, Some("k"), "").unwrap();
    assert!(out.contains("already enrolled as agent agent-1"), "{out}");
    assert_eq!(hub.count("/self-register"), 1);

    tool(&dir, &hub.url(), true, Some("k"), "").unwrap();
    assert_eq!(hub.count("/self-register"), 2);
    let again = Enrollment::load(&dir).unwrap().unwrap();
    assert_eq!(again.hwid, first.hwid, "the hub keeps seeing one machine");
}

#[test]
fn moving_to_another_hub_needs_force() {
    let (a, b) = (
        FakeHub::start(default_config()),
        FakeHub::start(default_config()),
    );
    let dir = temp_dir("enroll-move");
    tool(&dir, &a.url(), false, Some("k"), "").unwrap();
    let err = tool(&dir, &b.url(), false, Some("k"), "").unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    assert_eq!(b.count("/self-register"), 0);

    tool(&dir, &b.url(), true, Some("k"), "").unwrap();
    assert_eq!(Enrollment::load(&dir).unwrap().unwrap().hub_url, b.url());
}

#[test]
fn an_enrollment_file_that_does_not_open_is_replaced() {
    let hub = FakeHub::start(default_config());
    let dir = temp_dir("enroll-broken");
    let path = enrollment_path(&dir);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"\x00\x00\x00\x09not json!").unwrap();
    assert!(Enrollment::load(&dir).is_err());

    let out = tool(&dir, &hub.url(), false, Some("k"), "").unwrap();
    assert!(
        out.starts_with("replaced an enrollment file that did not open: "),
        "{out}"
    );
    assert_eq!(Enrollment::load(&dir).unwrap().unwrap().agent_id, "agent-1");
}

#[test]
fn bad_origins_and_empty_keys_are_refused_before_the_hub_is_called() {
    let hub = FakeHub::start(default_config());
    let dir = temp_dir("enroll-bad");
    for url in [
        "http://backup.example.com",
        "https://backup.example.com/api",
        "not a url",
    ] {
        assert!(
            enroll(&dir, url, "k", "m", false, &no_go()).is_err(),
            "{url}"
        );
    }
    let err = enroll(&dir, &hub.url(), " ", "m", false, &no_go()).unwrap_err();
    assert!(matches!(err, Error::Auth(_)), "{err}");
    assert_eq!(hub.count("/self-register"), 0);
    assert!(Enrollment::load(&dir).unwrap().is_none());
}

#[test]
fn an_unreachable_hub_stores_nothing() {
    let dir = temp_dir("enroll-unreachable");
    assert!(
        enroll(
            &dir,
            &crate::testutil::dead_url(),
            "k",
            "m",
            false,
            &no_go()
        )
        .is_err()
    );
    assert!(Enrollment::load(&dir).unwrap().is_none());
}
