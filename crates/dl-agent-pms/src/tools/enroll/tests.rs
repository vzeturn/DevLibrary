use std::sync::Mutex;

use super::*;
use crate::acl::AclError;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("pms-enr-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Records what was checked; every path counts as restricted.
#[derive(Default)]
struct FakeGuard(Mutex<Vec<PathBuf>>);

impl Guard for FakeGuard {
    fn restrict(&self, _: &Path) -> Result<(), AclError> {
        Ok(())
    }

    fn check(&self, path: &Path) -> Result<(), AclError> {
        self.0.lock().unwrap().push(path.to_owned());
        Ok(())
    }
}

fn no_env(_: &str) -> String {
    String::new()
}

const CODE: &str = "SECRET-CODE-123";

fn args(cfg: &Path, state: Option<&Path>, force: bool) -> Args {
    Args {
        cloud_url: "https://pms.example.com".into(),
        code: CODE.into(),
        config: Some(cfg.to_owned()),
        state_dir: state.map(Path::to_owned),
        force,
    }
}

fn run_with(a: &Args, guard: &FakeGuard) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let r = run_args(a, &mut out, guard, &no_env);
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn grammar_takes_the_documented_flags() {
    let cmd = grammar(Command::new(NAME));
    cmd.debug_assert();
    let m = cmd
        .parse_from([
            NAME,
            "--cloud-url",
            " https://pms.example.com ",
            "--code",
            "C1",
            "--state-dir",
            "s",
            "--force",
        ])
        .unwrap();
    let a = Args::from_matches(&m);
    assert_eq!(a.cloud_url, "https://pms.example.com");
    assert_eq!(a.code, "C1");
    assert_eq!(a.state_dir, Some(PathBuf::from("s")));
    assert!(a.force && a.config.is_none());
    assert!(cmd.parse_from([NAME, "--code", "C1"]).is_err());
}

#[test]
fn writes_a_new_config_sorted_restricted_and_never_prints_the_code() {
    let d = TempDir::new("new");
    let cfg = d.0.join("Agent").join("config.json");
    let state = d.0.join("state");
    let g = FakeGuard::default();
    let (r, out) = run_with(&args(&cfg, Some(&state), false), &g);
    r.unwrap();
    assert!(!out.contains(CODE), "{out}");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let (u, c, s) = (
        text.find("\"cloud_url\"").unwrap(),
        text.find("\"enrollment_code\"").unwrap(),
        text.find("\"state_dir\"").unwrap(),
    );
    assert!(u < c && c < s, "{text}");
    assert!(state.is_dir());
    let checked = g.0.lock().unwrap().clone();
    assert!(
        checked.iter().any(|p| p.ends_with("config.json.tmp")),
        "{checked:?}"
    );
    assert!(checked.contains(&state), "{checked:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let loaded = config::load(Some(&cfg), &no_env).unwrap();
    assert_eq!(loaded.enrollment_code, CODE);
    assert_eq!(loaded.state_dir, state);
}

#[test]
fn keeps_the_other_keys_of_an_existing_config() {
    let d = TempDir::new("merge");
    let cfg = d.0.join("config.json");
    let state = d.0.join("st");
    std::fs::write(
        &cfg,
        format!(
            r#"{{"log_level":"debug","cloud_url":"https://old.example.com","state_dir":{}}}"#,
            json::quote(&state.to_string_lossy())
        ),
    )
    .unwrap();
    let (r, _) = run_with(&args(&cfg, None, false), &FakeGuard::default());
    r.unwrap();
    let loaded = config::load(Some(&cfg), &no_env).unwrap();
    assert_eq!(loaded.log_level, "debug");
    assert_eq!(loaded.cloud_url, "https://pms.example.com");
    assert_eq!(loaded.state_dir, state);
    assert_eq!(loaded.enrollment_code, CODE);
}

#[test]
fn an_enrolled_site_is_left_alone_unless_forced() {
    let d = TempDir::new("enrolled");
    let cfg = d.0.join("config.json");
    let state = d.0.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join(IDENTITY_FILE), r#"{"agent_id":"a"}"#).unwrap();

    let (r, out) = run_with(&args(&cfg, Some(&state), false), &FakeGuard::default());
    r.unwrap();
    assert!(out.contains("already enrolled"), "{out}");
    assert!(!cfg.exists());
    assert!(state.join(IDENTITY_FILE).exists());

    let (r, out) = run_with(&args(&cfg, Some(&state), true), &FakeGuard::default());
    r.unwrap();
    assert!(out.contains("removed"), "{out}");
    assert!(!state.join(IDENTITY_FILE).exists());
    assert_eq!(
        config::load(Some(&cfg), &no_env).unwrap().enrollment_code,
        CODE
    );
}

#[test]
fn refuses_a_bad_url_an_empty_code_and_a_broken_config() {
    let d = TempDir::new("bad");
    let cfg = d.0.join("config.json");
    let g = FakeGuard::default();
    let mut a = args(&cfg, Some(&d.0.join("s")), false);
    a.cloud_url = "ftp://pms.example.com".into();
    assert!(run_with(&a, &g).0.is_err());
    let mut a = args(&cfg, Some(&d.0.join("s")), false);
    a.code = String::new();
    assert!(run_with(&a, &g).0.is_err());
    std::fs::write(&cfg, "[1]").unwrap();
    let (r, out) = run_with(&args(&cfg, Some(&d.0.join("s")), false), &g);
    assert!(r.unwrap_err().contains("not a JSON object"));
    assert!(out.is_empty());
    assert!(!cfg.exists() || std::fs::read_to_string(&cfg).unwrap() == "[1]");
}
