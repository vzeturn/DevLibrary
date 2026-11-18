use std::path::Path;

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("pms-prep-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn no_env(_: &str) -> String {
    String::new()
}

fn settings(cfg: &Path, state: Option<&Path>) -> Settings {
    Settings {
        config_path: Some(cfg.to_owned()),
        state_dir: state.map(Path::to_owned),
    }
}

fn waiting(p: &Prepared) -> (&str, bool) {
    match p {
        Prepared::Waiting { detail, invalid } => (detail, *invalid),
        Prepared::Ready(_) => panic!("ready"),
    }
}

#[test]
fn missing_config_is_not_enrolled() {
    let d = TempDir::new("missing");
    let p = prepare_with(&settings(&d.0.join("config.json"), None), &no_env);
    let (detail, invalid) = waiting(&p);
    assert!(
        detail.starts_with("not enrolled: no config file at "),
        "{detail}"
    );
    assert!(!invalid);
}

#[test]
fn bad_config_is_invalid_without_values() {
    let d = TempDir::new("bad");
    let cfg = d.0.join("config.json");
    std::fs::write(&cfg, r#"{"cloud_url":"ftp://secret-host","x":1}"#).unwrap();
    let p = prepare_with(&settings(&cfg, None), &no_env);
    let (detail, invalid) = waiting(&p);
    assert!(invalid);
    assert!(detail.starts_with("config invalid: "), "{detail}");
    assert!(!detail.contains("secret-host"), "{detail}");
}

#[test]
fn no_identity_and_no_code_waits_and_a_code_or_identity_is_ready() {
    let d = TempDir::new("ready");
    let cfg = d.0.join("config.json");
    let state = d.0.join("state");
    std::fs::write(&cfg, r#"{"cloud_url":"https://pms.example.com"}"#).unwrap();
    let s = settings(&cfg, Some(&state));
    let p = prepare_with(&s, &no_env);
    let (detail, _) = waiting(&p);
    assert!(detail.contains("no identity.json in"), "{detail}");
    assert!(detail.contains("state"), "{detail}");

    std::fs::write(
        &cfg,
        r#"{"cloud_url":"https://pms.example.com","enrollment_code":"ABC"}"#,
    )
    .unwrap();
    assert!(prepare_with(&s, &no_env).detail().is_none());

    std::fs::write(&cfg, r#"{"cloud_url":"https://pms.example.com"}"#).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join(IDENTITY_FILE), "{}").unwrap();
    match prepare_with(&s, &no_env) {
        Prepared::Ready(c) => {
            assert_eq!(c.state_dir, state);
            assert_eq!(c.path.as_deref(), Some(cfg.as_path()));
            let before = fingerprint(&c);
            std::fs::remove_file(state.join(IDENTITY_FILE)).unwrap();
            assert_ne!(fingerprint(&c), before);
        }
        p => panic!("{p:?}"),
    }
}
