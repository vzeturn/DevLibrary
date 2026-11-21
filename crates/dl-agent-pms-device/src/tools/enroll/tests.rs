use std::cell::RefCell;

use super::*;
use crate::config::{HostInputs, bind_agent_options, load, validate};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("pmsdev-enr-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const LONG: &str = "device-token-0123456789-abcdefghijklmnop";

fn args(dir: &TempDir) -> Args {
    Args {
        api_url: "https://pms.example.com".into(),
        agent_id: Some("front-desk_1".into()),
        appsettings_dir: Some(dir.0.join("app")),
        token_file: Some(dir.0.join("data").join("agent.token")),
        force: false,
        default_dir: dir.0.join("default"),
    }
}

fn env(token: Option<&str>) -> impl Fn(&str) -> String {
    let token = token.map(str::to_owned);
    move |k| match k {
        TOKEN_ENV => token.clone().unwrap_or_default(),
        "COMPUTERNAME" => "FD PC#01".to_owned(),
        _ => String::new(),
    }
}

fn run_with(a: &Args, token: Option<&str>) -> (Result<(), String>, String) {
    let restricted = RefCell::new(Vec::new());
    let mut out = Vec::new();
    let r = run_args(
        a,
        &mut out,
        &|p| {
            restricted.borrow_mut().push(p.to_owned());
            Ok(())
        },
        &env(token),
    );
    let out = String::from_utf8(out).unwrap();
    if r.is_ok() {
        // The token file is restricted exactly once, and only when it was written.
        let want = usize::from(out.contains("token written"));
        assert_eq!(restricted.borrow().len(), want);
    }
    (r, out)
}

fn settings(a: &Args) -> Value {
    let p = a.appsettings_dir.as_ref().unwrap().join(SETTINGS_FILE);
    dl_json::parse(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn token(a: &Args) -> String {
    std::fs::read_to_string(a.token_file.as_ref().unwrap()).unwrap()
}

fn sha(t: &str) -> String {
    dl_encoding::hex::encode(<dl_hash::Sha256 as dl_hash::Digest>::digest(t.as_bytes()))
}

#[test]
fn generates_a_token_and_writes_the_three_keys() {
    let d = TempDir::new("gen");
    let a = args(&d);
    let (r, out) = run_with(&a, None);
    r.unwrap();
    let t = token(&a);
    assert_eq!(t.len(), 64);
    assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
    let s = settings(&a);
    let agent = s.get("Agent").unwrap();
    assert_eq!(
        agent.get("ApiBaseUrl").and_then(Value::as_str),
        Some("https://pms.example.com")
    );
    assert_eq!(
        agent.get("AgentId").and_then(Value::as_str),
        Some("front-desk_1")
    );
    assert_eq!(
        agent.get("TokenFile").and_then(Value::as_str),
        Some(a.token_file.as_ref().unwrap().to_string_lossy().as_ref())
    );
    assert!(out.contains(&format!("sha256={}", sha(&t))));
    assert!(!out.contains(&t));
}

#[cfg(unix)]
#[test]
fn the_token_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = TempDir::new("mode");
    let a = args(&d);
    run_with(&a, None).0.unwrap();
    let mode = std::fs::metadata(a.token_file.as_ref().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn keeps_a_valid_existing_token() {
    let d = TempDir::new("keep");
    let a = args(&d);
    run_with(&a, None).0.unwrap();
    let first = token(&a);
    let (r, out) = run_with(&a, None);
    r.unwrap();
    assert_eq!(token(&a), first);
    assert!(out.contains("token kept") && out.contains(&sha(&first)));
}

#[test]
fn uses_the_env_token_and_never_prints_it() {
    let d = TempDir::new("env");
    let a = args(&d);
    let (r, out) = run_with(&a, Some(&format!("  {LONG}\n")));
    r.unwrap();
    assert_eq!(token(&a), LONG);
    assert!(out.contains(&format!("sha256={}", sha(LONG))));
    assert!(!out.contains(LONG));
}

#[test]
fn a_short_env_token_is_refused() {
    let d = TempDir::new("short");
    let a = args(&d);
    let (r, _) = run_with(&a, Some("too-short"));
    assert!(r.unwrap_err().contains("shorter than 32"));
    assert!(!a.token_file.as_ref().unwrap().exists());
}

#[test]
fn another_env_token_needs_force() {
    let d = TempDir::new("force");
    let mut a = args(&d);
    run_with(&a, Some(LONG)).0.unwrap();
    let other = format!("{LONG}-other");
    let (r, out) = run_with(&a, Some(&other));
    let e = r.unwrap_err();
    assert!(
        e.contains("already enrolled") && e.contains("--force"),
        "{e}"
    );
    assert!(!e.contains(LONG) && !out.contains(LONG));
    assert_eq!(token(&a), LONG);
    // The same token again is fine without --force.
    run_with(&a, Some(LONG)).0.unwrap();
    a.force = true;
    run_with(&a, Some(&other)).0.unwrap();
    assert_eq!(token(&a), other);
}

#[test]
fn force_without_env_rotates_to_a_generated_token() {
    let d = TempDir::new("rotate");
    let mut a = args(&d);
    run_with(&a, Some(LONG)).0.unwrap();
    a.force = true;
    run_with(&a, None).0.unwrap();
    assert_ne!(token(&a), LONG);
    assert_eq!(token(&a).len(), 64);
}

#[test]
fn a_short_token_file_is_replaced_by_a_generated_one() {
    let d = TempDir::new("stub");
    let a = args(&d);
    std::fs::create_dir_all(a.token_file.as_ref().unwrap().parent().unwrap()).unwrap();
    std::fs::write(a.token_file.as_ref().unwrap(), "short").unwrap();
    run_with(&a, None).0.unwrap();
    assert_eq!(token(&a).len(), 64);
}

#[test]
fn refuses_a_bad_url() {
    let d = TempDir::new("url");
    for bad in [
        "",
        "not a url",
        "http://pms.example.com",
        "ftp://pms.example.com",
    ] {
        let mut a = args(&d);
        a.api_url = bad.into();
        let e = run_with(&a, None).0.unwrap_err();
        assert!(e.contains("--api-url"), "{bad}: {e}");
    }
    let mut a = args(&d);
    a.api_url = "http://localhost:3001".into();
    run_with(&a, None).0.unwrap();
}

#[test]
fn refuses_a_bad_agent_id_and_derives_a_default() {
    let d = TempDir::new("id");
    for bad in ["", "has space", "x".repeat(65).as_str(), "a/b"] {
        let mut a = args(&d);
        a.agent_id = Some(bad.into());
        // An empty --agent-id is the default (computer name), the rest is refused.
        if bad.is_empty() {
            continue;
        }
        assert!(
            run_with(&a, None).0.unwrap_err().contains("--agent-id"),
            "{bad}"
        );
    }
    let mut a = args(&d);
    a.agent_id = None;
    run_with(&a, None).0.unwrap();
    // "FD PC#01" reduced to the alphabet.
    let s = settings(&a);
    assert_eq!(
        s.get("Agent")
            .unwrap()
            .get("AgentId")
            .and_then(Value::as_str),
        Some("FDPC01")
    );
}

#[test]
fn merge_keeps_every_other_key_and_the_data_directory_default() {
    let d = TempDir::new("merge");
    let a = args(&d);
    let dir = a.appsettings_dir.clone().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let data = d.0.join("custom-data");
    let existing = format!(
        r#"{{"Logging": {{"LogLevel": {{"Default": "Warning"}}}}, "agent": {{"AgentId": "old", "Adapter": "smile-lock", "PollInterval": "00:00:02", "DataDirectory": {}}}}}"#,
        dl_json::to_string(&Value::from(data.to_string_lossy().into_owned()))
    );
    std::fs::write(dir.join(SETTINGS_FILE), existing).unwrap();
    let mut a = a;
    a.token_file = None; // default: <Agent:DataDirectory>/agent.token
    run_with(&a, None).0.unwrap();
    assert!(data.join(TOKEN_FILE_NAME).is_file());
    let s = settings(&a);
    assert_eq!(
        s.get("Logging")
            .unwrap()
            .get("LogLevel")
            .unwrap()
            .get("Default")
            .and_then(Value::as_str),
        Some("Warning")
    );
    // The key written is the one that was there ("agent"), not a second "Agent".
    assert!(s.get("Agent").is_none());
    let agent = s.get("agent").unwrap();
    assert_eq!(
        agent.get("AgentId").and_then(Value::as_str),
        Some("front-desk_1")
    );
    assert_eq!(
        agent.get("Adapter").and_then(Value::as_str),
        Some("smile-lock")
    );
    assert_eq!(
        agent.get("PollInterval").and_then(Value::as_str),
        Some("00:00:02")
    );
}

#[test]
fn an_unparsable_settings_file_is_left_alone() {
    let d = TempDir::new("bad");
    let a = args(&d);
    let dir = a.appsettings_dir.clone().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let text = "{ // comment\n \"Agent\": {\"Adapter\": \"none\",}, }";
    std::fs::write(dir.join(SETTINGS_FILE), text).unwrap();
    let e = run_with(&a, None).0.unwrap_err();
    assert!(e.contains("not plain JSON"), "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap(),
        text
    );
    assert!(!a.token_file.as_ref().unwrap().exists());
}

#[test]
fn the_written_settings_load_and_validate_through_the_module_path() {
    let d = TempDir::new("e2e");
    let mut a = args(&d);
    a.api_url = "https://pms.example.com:3001".into();
    // An absolute DataDirectory so the validator passes on every OS.
    let dir = a.appsettings_dir.clone().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let data = d.0.join("data");
    std::fs::write(
        dir.join(SETTINGS_FILE),
        format!(
            r#"{{"Agent": {{"DataDirectory": {}}}}}"#,
            dl_json::to_string(&Value::from(data.to_string_lossy().into_owned()))
        ),
    )
    .unwrap();
    run_with(&a, None).0.unwrap();

    let inputs = HostInputs {
        args: Vec::new(),
        env: Vec::new(),
        current_dir: dir.clone(),
        base_dir: dir,
        system_dir: PathBuf::new(),
    };
    let cfg = load(&inputs).unwrap();
    let options = bind_agent_options(&cfg.configuration).unwrap();
    validate(&options).unwrap();
    assert_eq!(options.agent_id, "front-desk_1");
    assert_eq!(options.api_base_url, "https://pms.example.com:3001");
    assert_eq!(
        TokenFile::new(options.token_file.clone())
            .read()
            .unwrap()
            .len(),
        64
    );
}
