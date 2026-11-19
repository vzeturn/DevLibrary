//! The diagnostics bundle: what goes in, what never does, and the upload.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dl_json::json;
use dl_zip::ZipArchive;

use super::super::tests::{agent, wait_running};
use super::*;
use crate::hub::tests_support::FakeHub;
use crate::store::testdir::TestDir;
use crate::supervisor::{Module, ModuleContext};

/// Every entry of a zip: `(name, text)`.
fn entries(zip: &[u8]) -> Vec<(String, String)> {
    let mut a = ZipArchive::new(Cursor::new(zip.to_vec())).unwrap();
    let names: Vec<String> = a.entries().iter().map(|e| e.name().to_string()).collect();
    names
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            let bytes = a.read(i, MAX_BUNDLE_BYTES).unwrap();
            (n, String::from_utf8(bytes).unwrap())
        })
        .collect()
}

fn write(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn bundle_scrubs_secrets_and_leaves_out_key_files() {
    let d = TestDir::new("diag-scrub");
    let data = &d.0;
    write(
        &data.join("logs").join("agent.log"),
        "INFO connect Server=db;User Id=sa;Password=Hunter2;\nWARN hub Authorization: Bearer tok123\n",
    );
    write(
        &data.join("config.json"),
        r#"{"version": 3, "body": {"modules": {"x": {"api_token": "tok-xyz"}}}}"#,
    );
    write(&data.join("update.json"), r#"{"failures": []}"#);
    write(
        &data.join("sql.json"),
        r#"{"connection": "Server=.", "password_sealed": "AQAAA-SEALED"}"#,
    );
    write(&data.join("identity.key"), "PRIVATE-KEY-SEED");
    let diag =
        json!({"agent_id": "agt_1", "note": "Pwd=Hunter2;", "log_tail": ["password=Hunter2"]});

    let zip = bundle(&diag, data, &[], 1_763_460_000).unwrap();
    let all = entries(&zip);
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "diagnostics.json",
            "logs/agent.log",
            "state/update.json",
            "state/config.json",
            "bundle.json"
        ]
    );
    let text: String = all.iter().map(|(_, t)| t.as_str()).collect();
    for secret in [
        "Hunter2",
        "tok123",
        "tok-xyz",
        "AQAAA-SEALED",
        "PRIVATE-KEY-SEED",
    ] {
        assert!(!text.contains(secret), "{secret} leaked");
    }
    assert!(text.contains("Password=[redacted]"), "{text}");
    assert!(text.contains("\"agt_1\""));
    let index = dl_json::parse(&all[4].1).unwrap();
    assert_eq!(index["files"][0]["file"].as_str(), Some("logs/agent.log"));
}

#[test]
fn logs_take_the_newest_tail_within_the_budget() {
    let d = TestDir::new("diag-budget");
    let logs = d.0.join("logs");
    let lines: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
    write(&logs.join("old.log"), &lines);
    write(&logs.join("new.log"), &lines);
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(logs.join("old.log"))
        .unwrap()
        .set_modified(hour_ago)
        .unwrap();

    let zip = bundle_with(&json!({}), &d.0, &[], 1, 500).unwrap();
    let all = entries(&zip);
    let new = &all.iter().find(|(n, _)| n == "logs/new.log").unwrap().1;
    assert!(new.len() <= 500 && new.len() > 400, "{}", new.len());
    assert!(
        new.starts_with("line ") && new.ends_with("line 099\n"),
        "{new}"
    );
    assert!(all.iter().all(|(n, _)| n != "logs/old.log"));
    let index = dl_json::parse(&all.last().unwrap().1).unwrap();
    assert_eq!(index["files"][0]["size"].as_u64(), Some(900));
    assert_eq!(
        index["files"][1]["skipped"].as_str(),
        Some("log budget used up")
    );
}

#[test]
fn collect_diagnostics_uploads_the_zip_and_answers_with_its_key() {
    let d = TestDir::new("diag-upload");
    let hub = FakeHub::default();
    let a = agent(&d, &hub, Vec::new());
    write(
        &a.opts.data_dir.join("logs").join("agent.log"),
        "started\npassword=Hunter2\n",
    );
    hub.push(
        200,
        r#"{"id": "d1", "key": "diag/acme/agt_1/1-abcd1234.zip", "size": 10, "sha256": "ab"}"#,
    );
    let (ok, out) = a.collect_diagnostics("c-9", 1000);
    assert!(ok, "{out}");
    let v = dl_json::parse(&out).unwrap();
    assert_eq!(
        v["uploaded"]["key"].as_str(),
        Some("diag/acme/agt_1/1-abcd1234.zip")
    );
    assert_eq!(v["diagnostics"]["agent_id"].as_str(), Some("agt_1"));
    assert!(v["diagnostics"].get("log_tail").is_none());

    let seen = hub.seen();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0]
            .url
            .ends_with("/v1/agent/diagnostics?command_id=c-9"),
        "{}",
        seen[0].url
    );
    let header = |name: &str| {
        seen[0]
            .headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("Content-Type").as_deref(), Some("application/zip"));
    assert!(header("X-Agent-Signature").is_some());
    let text: String = entries(&seen[0].body).into_iter().map(|(_, t)| t).collect();
    assert!(text.contains("started") && !text.contains("Hunter2"));
}

#[test]
fn a_failed_upload_fails_the_command_but_keeps_the_diagnostics() {
    let d = TestDir::new("diag-fail");
    let hub = FakeHub::default();
    let a = agent(&d, &hub, Vec::new());
    hub.push(404, r#"{"error": "not_found"}"#);
    let (ok, out) = a.collect_diagnostics("c-1", 1000);
    assert!(!ok);
    let v = dl_json::parse(&out).unwrap();
    assert!(v["upload_error"].as_str().unwrap().contains("404"), "{out}");
    assert_eq!(v["diagnostics"]["agent_id"].as_str(), Some("agt_1"));
    assert!(v["diagnostics"].get("log_tail").is_some());

    // A hub answer without a key is not an upload.
    hub.push(200, "{}");
    let (ok, out) = a.collect_diagnostics("c-2", 1000);
    assert!(!ok && out.contains("no key"), "{out}");
}

/// A module that keeps its state files in its own install folder, like `arrival`.
struct Elsewhere(PathBuf);

impl Module for Elsewhere {
    fn name(&self) -> &str {
        "arrival"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        for f in ["agent-problems.json", "dl-enroll.json", "gone.json"] {
            ctx.add_diagnostic_file(self.0.join(f));
        }
        ctx.add_diagnostic_file(self.0.join("agent-identity.bin"));
        ctx.set_running("ok");
        while !ctx.stop().wait_timeout(Duration::from_millis(10)) {}
        Ok(())
    }
}

#[test]
fn module_files_go_in_scrubbed_and_secret_files_never() {
    let ctx = ModuleContext::standalone("arrival", Value::Null, std::env::temp_dir());
    for refused in [
        "data/agent-identity.bin",
        "agent-tenant-key.bin",
        "appsettings.Production.json",
        "sql-password.txt",
        "secret.json",
        "notes.docx",
    ] {
        assert!(!ctx.add_diagnostic_file(refused), "{refused}");
    }
    assert!(ctx.add_diagnostic_file("data/dl-enroll.json"));
    assert!(ctx.add_diagnostic_file("data/dl-enroll.json"));
    assert_eq!(ctx.status().diagnostic_files.len(), 1);

    let d = TestDir::new("diag-modules");
    let install = d.0.join("MM-Arrival Agent").join("data");
    write(
        &install.join("agent-problems.json"),
        r#"[{"key": "pms", "message": "login failed: Password=Hunter2;"}]"#,
    );
    write(
        &install.join("dl-enroll.json"),
        r#"{"identity_source": "fresh", "tenant_key": "imported"}"#,
    );
    write(&install.join("agent-identity.bin"), "IDENTITY-SECRET");
    let hub = FakeHub::default();
    let mut a = agent(
        &d,
        &hub,
        vec![Arc::new({
            let install = install.clone();
            move || Box::new(Elsewhere(install.clone()))
        })],
    );
    wait_running(&mut a);
    hub.push(
        200,
        r#"{"id": "d1", "key": "diag/k.zip", "size": 1, "sha256": "ab"}"#,
    );
    let (ok, out) = a.collect_diagnostics("c-3", 1000);
    a.supervisor.stop_all(crate::agent::STOP_WAIT);
    assert!(ok, "{out}");
    let all = entries(&hub.seen()[0].body);
    let get = |name: &str| all.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone());
    let problems = get("modules/arrival/agent-problems.json").expect("problem journal");
    assert!(
        problems.contains("login failed") && !problems.contains("Hunter2"),
        "{problems}"
    );
    assert!(
        get("modules/arrival/dl-enroll.json")
            .unwrap()
            .contains("imported")
    );
    assert!(
        all.iter()
            .all(|(n, t)| !n.contains("identity") && !t.contains("IDENTITY-SECRET"))
    );
    let index = get("bundle.json").unwrap();
    assert!(
        index.contains("modules/arrival/gone.json") && index.contains("not there"),
        "{index}"
    );
}
