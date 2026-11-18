use super::*;
use dl_json::json;

/// A fresh folder under the temp directory, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let p = std::env::temp_dir().join(format!(
            "pms-device-prepare-{tag}-{}-{}",
            std::process::id(),
            dl_uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const TOKEN: &str = "0123456789abcdef0123456789abcdef-secret";

fn appsettings(dir: &Path, token_file: &Path, extra: &str) {
    let data = dir.join("data");
    let text = format!(
        r#"{{"Agent": {{"ApiBaseUrl": "http://127.0.0.1:9", "AgentId": "FD01",
            "TokenFile": {}, "DataDirectory": {}, "RunOnce": true{extra}}}}}"#,
        dl_json::to_string(&dl_json::Value::from(
            token_file.to_string_lossy().into_owned()
        )),
        dl_json::to_string(&dl_json::Value::from(data.to_string_lossy().into_owned())),
    );
    std::fs::write(dir.join("appsettings.json"), text).unwrap();
}

fn settings(dir: &Path, overrides: dl_json::Value) -> Settings {
    Settings::from_config(&json!({
        "appsettings_dir": dir.to_string_lossy().into_owned(),
        "overrides": overrides
    }))
}

#[test]
fn ready_with_the_appsettings_the_env_file_and_the_overrides() {
    let d = Dir::new("ready");
    let token = d.0.join("agent.token");
    std::fs::write(&token, TOKEN).unwrap();
    appsettings(&d.0, &token, r#", "Adapter": "none""#);
    std::fs::write(
        d.0.join("appsettings.Staging.json"),
        r#"{"Agent": {"PollInterval": "00:00:02"}}"#,
    )
    .unwrap();
    let env = vec![("DOTNET_ENVIRONMENT".to_owned(), "Staging".to_owned())];
    let s = settings(&d.0, json!({"Adapter": "simulated-lock"}));
    let Prepared::Ready(r) = prepare(&s, Path::new("/unused"), &env) else {
        panic!("expected ready");
    };
    assert_eq!(r.options.agent_id, "FD01");
    assert_eq!(r.options.adapter, "simulated-lock");
    assert_eq!(r.options.poll_interval, config::TimeSpan::from_secs(2));
    assert!(!r.options.run_once, "RunOnce is forced off under dl-agent");
    assert_eq!(r.environment, "Staging");
    assert_eq!(r.watched().len(), 3);
    assert!(r.watched()[1].ends_with("appsettings.Staging.json"));
}

#[test]
fn an_unconfigured_install_waits_with_the_validator_messages() {
    let d = Dir::new("empty");
    let p = prepare(&settings(&d.0, json!({})), Path::new("/unused"), &[]);
    let detail = p.detail().unwrap();
    assert!(detail.starts_with("config invalid: "), "{detail}");
    assert!(detail.contains("Agent:AgentId"), "{detail}");
    assert!(detail.contains("Agent:TokenFile is required"), "{detail}");
}

#[test]
fn a_broken_appsettings_file_waits_on_one_line() {
    let d = Dir::new("broken");
    std::fs::write(d.0.join("appsettings.json"), "{\"Agent\": ").unwrap();
    let p = prepare(&settings(&d.0, json!({})), Path::new("/unused"), &[]);
    let detail = p.detail().unwrap();
    assert!(detail.starts_with("config invalid: "), "{detail}");
    assert!(!detail.contains('\n'), "{detail}");
}

#[test]
fn a_missing_or_short_token_waits_without_leaking_it() {
    let d = Dir::new("token");
    let token = d.0.join("agent.token");
    appsettings(&d.0, &token, "");
    let s = settings(&d.0, json!({}));
    let detail = prepare(&s, Path::new("/unused"), &[])
        .detail()
        .unwrap()
        .to_owned();
    assert!(detail.starts_with("token not readable: "), "{detail}");
    assert!(detail.contains("was not found"), "{detail}");

    std::fs::write(&token, "short-secret").unwrap();
    let detail = prepare(&s, Path::new("/unused"), &[])
        .detail()
        .unwrap()
        .to_owned();
    assert!(detail.contains("shorter than 32"), "{detail}");
    assert!(!detail.contains("short-secret"), "{detail}");

    std::fs::write(&token, TOKEN).unwrap();
    assert!(prepare(&s, Path::new("/unused"), &[]).detail().is_none());
}

#[test]
fn env_variables_override_the_files_and_the_overrides_win() {
    let d = Dir::new("env");
    let token = d.0.join("agent.token");
    std::fs::write(&token, TOKEN).unwrap();
    appsettings(&d.0, &token, "");
    let env = vec![("Agent__AgentId".to_owned(), "FROM-ENV".to_owned())];
    let Prepared::Ready(r) = prepare(&settings(&d.0, json!({})), Path::new("/x"), &env) else {
        panic!("expected ready");
    };
    assert_eq!(r.options.agent_id, "FROM-ENV");
    let s = settings(&d.0, json!({"AgentId": "FROM-HUB"}));
    let Prepared::Ready(r) = prepare(&s, Path::new("/x"), &env) else {
        panic!("expected ready");
    };
    assert_eq!(r.options.agent_id, "FROM-HUB");
}

#[test]
fn the_fingerprint_follows_size_and_presence() {
    let d = Dir::new("fp");
    let f = d.0.join("a.json");
    let files = vec![f.clone()];
    let missing = fingerprint(&files);
    assert_eq!(missing, vec![None]);
    std::fs::write(&f, "1").unwrap();
    let one = fingerprint(&files);
    assert_ne!(one, missing);
    std::fs::write(&f, "12").unwrap();
    assert_ne!(fingerprint(&files), one);
}
