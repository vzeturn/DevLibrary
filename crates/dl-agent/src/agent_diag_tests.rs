//! The diagnostics bundle: what goes in, what never does, and the upload.

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dl_json::{Value, json};
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

    let inputs = Inputs {
        diagnostics: diag,
        ..Inputs::default()
    };
    let zip = bundle(&inputs, data, 1_763_460_000).unwrap();
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

    let zip = bundle_with(&Inputs::default(), &d.0, 1, 500).unwrap();
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
        v["uploaded"]["r2_key"].as_str(),
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
    let enroll = get("modules/arrival/dl-enroll.json").unwrap();
    assert!(
        enroll.contains(r#""identity_source": "fresh""#)
            && enroll.contains(r#""tenant_key": "[redacted]""#)
            && !enroll.contains("imported"),
        "{enroll}"
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

/// A module like `pda-sync`: a status file plus a rolling log folder in its own install dir.
struct Rolling(PathBuf);

impl Module for Rolling {
    fn name(&self) -> &str {
        "pda-sync"
    }

    fn run(&mut self, ctx: &ModuleContext) -> Result<(), Error> {
        ctx.add_diagnostic_file(self.0.join("status.json"));
        ctx.add_diagnostic_folder(self.0.join("logs"));
        ctx.add_diagnostic_folder(self.0.join("keys"));
        ctx.set_running("ok");
        while !ctx.stop().wait_timeout(Duration::from_millis(10)) {}
        Ok(())
    }
}

fn age(path: &std::path::Path, secs: u64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

/// Every secret shape planted in every source the bundle reads; none may survive.
const JWT: &str = "eyJhbGciOiJFZERTQSJ9.eyJhZ2VudCI6ImFndF8xIn0.U0lHTkFUVVJFX0xFQUs";

#[test]
fn the_bundle_has_every_source_and_no_planted_secret_survives() {
    let d = TestDir::new("diag-full");
    let hub = FakeHub::default();
    let install = d.0.join("PdaSync");
    let mut a = agent(
        &d,
        &hub,
        vec![Arc::new({
            let install = install.clone();
            move || Box::new(Rolling(install.clone()))
        })],
    );
    let data = a.opts.data_dir.clone();
    let logs = data.join("logs");
    write(
        &logs.join("agent.log"),
        &format!(
            "INFO current\nWARN sql Server=db;uid=sa;PASSWORD='Plant''A';\nINFO token {JWT}\n"
        ),
    );
    write(
        &logs.join("agent.log.1"),
        "INFO rotated\nWARN proxy http://op:PlantB@proxy:3128 refused\n",
    );
    age(&logs.join("agent.log.1"), 3600);
    write(
        &data.join("config.json"),
        r#"{"version": 7, "body": {"modules": {"pda-sync": {"cloud_url": "https://cloud.test",
        "api_token": "PlantC", "client_secret": "PlantD", "conn": "Data Source=x;Pwd=PlantE;"}}}}"#,
    );
    write(&data.join("identity.key"), "PlantF");
    write(&data.join("sql.json"), r#"{"password_sealed": "PlantG"}"#);
    write(
        &data.join(crate::selftest::LAST_RUN_FILE),
        r#"{"run_id": "r1", "checks": [{"id": "sql", "status": "fail", "detail": "login failed for sa, pwd=PlantH"}]}"#,
    );
    write(
        &install.join("status.json"),
        r#"{"state": "running", "refresh_token": "PlantI"}"#,
    );
    let pda_logs = install.join("logs");
    write(
        &pda_logs.join("pda-2026-11-19.log"),
        "today Bearer PlantJ\n",
    );
    write(
        &pda_logs.join("pda-2026-11-18.log"),
        "yesterday password=PlantK\n",
    );
    age(&pda_logs.join("pda-2026-11-18.log"), 86_400);
    write(&pda_logs.join("tenant-key.json"), "PlantL");
    write(&install.join("keys").join("agent.log"), "PlantM");
    a.sql.pools.set_survey(
        "SMILE_POS",
        json!({"engine": {"version": "10.50"}, "connection": "Server=relay;Password=PlantN"}),
    );
    wait_running(&mut a);
    hub.push(
        200,
        r#"{"id": "d1", "key": "diag/k.zip", "size": 1, "sha256": "ab"}"#,
    );
    let (ok, out) = a.collect_diagnostics("c-full", 1000);
    a.supervisor.stop_all(crate::agent::STOP_WAIT);
    assert!(ok, "{out}");
    let all = entries(&hub.seen()[0].body);
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    for want in [
        "diagnostics.json",
        "survey.json",
        "logs/agent.log",
        "logs/agent.log.1",
        "state/config.json",
        "state/selftest.last.json",
        "modules/pda-sync/status.json",
        "modules/pda-sync/logs/pda-2026-11-19.log",
        "modules/pda-sync/logs/pda-2026-11-18.log",
        "bundle.json",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    assert!(
        names
            .iter()
            .all(|n| !n.contains("identity") && !n.contains("sql.json") && !n.contains("key")),
        "{names:?}"
    );
    let get = |name: &str| all.iter().find(|(n, _)| n == name).unwrap().1.clone();
    assert!(get("logs/agent.log").contains("current"));
    assert!(get("logs/agent.log.1").contains("rotated"));
    assert!(get("survey.json").contains("10.50"));
    assert!(get("state/selftest.last.json").contains("login failed"));
    let diag = dl_json::parse(&get("diagnostics.json")).unwrap();
    assert_eq!(diag["modules"][1]["state"].as_str(), Some("running"));
    let text: String = all.iter().map(|(n, t)| format!("{n}\n{t}\n")).collect();
    for plant in [
        "PlantA", "PlantB", "PlantC", "PlantD", "PlantE", "PlantF", "PlantG", "PlantH", "PlantI",
        "PlantJ", "PlantK", "PlantL", "PlantM", "PlantN", JWT,
    ] {
        assert!(!text.contains(plant), "{plant} leaked");
    }
    assert!(!out.contains("Plant"), "{out}");
}

#[test]
fn module_folders_take_the_newest_files_within_their_share() {
    let ctx = ModuleContext::standalone("pda-sync", Value::Null, std::env::temp_dir());
    for refused in ["keys", "secrets", "identity", "certs", ""] {
        assert!(!ctx.add_diagnostic_folder(refused), "{refused}");
    }
    assert!(ctx.add_diagnostic_folder("PdaSync/logs"));
    assert!(ctx.add_diagnostic_folder("PdaSync/logs"));
    assert_eq!(ctx.status().diagnostic_folders.len(), 1);

    let d = TestDir::new("diag-folder");
    let dir = d.0.join("logs");
    let lines: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
    for (i, name) in ["c.log", "b.log", "a.log"].iter().enumerate() {
        write(&dir.join(name), &lines);
        age(&dir.join(name), 60 * i as u64);
    }
    let inputs = Inputs {
        module_folders: vec![
            ("modules/pda-sync/logs".into(), dir),
            ("modules/pda-sync/gone".into(), d.0.join("gone")),
        ],
        ..Inputs::default()
    };
    // A 4000-byte log budget leaves 1000 bytes for the folder: c.log whole, b.log's end.
    let zip = bundle_with(&inputs, &d.0, 1, 4000).unwrap();
    let all = entries(&zip);
    let get = |name: &str| all.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone());
    assert_eq!(get("modules/pda-sync/logs/c.log").unwrap().len(), 900);
    let b = get("modules/pda-sync/logs/b.log").unwrap();
    assert!(b.len() <= 100 && b.ends_with("line 099\n"), "{b}");
    assert!(get("modules/pda-sync/logs/a.log").is_none());
    let index = get("bundle.json").unwrap();
    assert!(
        index.contains("modules/pda-sync/logs/a.log") && index.contains("log budget used up"),
        "{index}"
    );
    assert!(index.contains("modules/pda-sync/gone") && index.contains("not there"));
}
