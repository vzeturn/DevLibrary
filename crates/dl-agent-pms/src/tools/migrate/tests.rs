use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("pms-mig-{tag}-{}-{n}", std::process::id()));
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

fn run_with(args: &Args) -> (Result<(), String>, dl_json::Value) {
    let mut out = Vec::new();
    let r = run_args(args, &mut out, &no_env);
    let v = json::parse(&out).expect("the result is JSON");
    (r, v)
}

fn error_code(v: &dl_json::Value) -> String {
    let dl_json::Value::Object(o) = v else {
        panic!("not an object: {v:?}");
    };
    let Some(dl_json::Value::Object(e)) = o.get("error") else {
        panic!("no error: {v:?}");
    };
    match e.get("code") {
        Some(dl_json::Value::String(s)) => s.clone(),
        other => panic!("code {other:?}"),
    }
}

#[test]
fn grammar_parses_the_port_flags() {
    let cmd = grammar(Command::new(NAME));
    cmd.debug_assert();
    let m = cmd
        .parse_from([
            NAME,
            "--bundle",
            "b",
            "--dry-run",
            "--rerun-partial",
            "3",
            "--config",
            "c.json",
        ])
        .unwrap();
    let a = Args::from_matches(&m).unwrap();
    assert_eq!(a.bundle, PathBuf::from("b"));
    assert!(a.dry_run && !a.check_drift);
    assert_eq!(a.rerun, 3);
    assert_eq!(a.config, Some(PathBuf::from("c.json")));

    let m = cmd
        .parse_from([NAME, "--bundle", "b", "--rerun-partial", "x"])
        .unwrap();
    assert!(Args::from_matches(&m).is_err());
    assert!(cmd.parse_from([NAME]).is_err());
}

#[test]
fn a_missing_config_prints_a_config_error() {
    let d = TempDir::new("nocfg");
    let args = Args {
        bundle: d.0.join("bundle"),
        dry_run: true,
        config: Some(d.0.join("config.json")),
        ..Args::default()
    };
    let (r, v) = run_with(&args);
    let err = r.unwrap_err();
    assert!(err.starts_with("pms-migrate: CONFIG: "), "{err}");
    assert_eq!(error_code(&v), "CONFIG");
}

#[test]
fn a_config_without_a_keyring_is_refused_before_the_bundle() {
    let d = TempDir::new("nokeys");
    let cfg = d.0.join("config.json");
    std::fs::write(
        &cfg,
        format!(
            r#"{{"cloud_url":"https://pms.example.com","state_dir":{}}}"#,
            json::quote(&d.0.join("state").to_string_lossy())
        ),
    )
    .unwrap();
    let args = Args {
        bundle: d.0.join("bundle"),
        config: Some(cfg),
        ..Args::default()
    };
    let (r, v) = run_with(&args);
    assert!(r.is_err());
    assert_eq!(error_code(&v), "CONFIG");
    let dl_json::Value::Object(o) = &v else {
        unreachable!()
    };
    assert_eq!(o.get("dry_run"), Some(&dl_json::Value::Bool(false)));
    assert!(o.get("bundle_id").is_none());
}
