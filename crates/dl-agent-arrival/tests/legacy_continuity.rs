//! Legal vault continuity across the pilot rollout (src/legacy/tenant_key.rs): the bench
//! removes the .NET agent (service deleted, folder renamed `.bench-old-*`, nothing deleted),
//! `arrival-enroll` enrolls afresh, and every vault file the old agent sealed still opens
//! through the module's own resolution. A key it cannot prove is refused before anything is
//! written. Off Windows key files are the raw 32 bytes (DPAPI is the Windows path).

mod common;

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::TempDir;
use dl_agent_arrival::agent::layout::Layout;
use dl_agent_arrival::agent::secrets::{Identity, TenantKeyStore};
use dl_agent_arrival::agent::settings::Settings;
use dl_agent_arrival::agent::vault::LegalVault;
use dl_agent_arrival::contracts::key_vault::TenantKeyVault;
use dl_agent_arrival::contracts::values::to_base64;
use dl_agent_arrival::stj;
use dl_agent_arrival::tools::enroll::{
    Args, Io, SQL_AUTH_ENV, SQL_PASSWORD_ENV, SQL_USER_ENV, TENANT_KEY_ENV, TOKEN_ENV, run_args,
};

const OLD_PMS: &str =
    "Server=HOTEL\\SMILE;Database=SMILE_FO;Integrated Security=true;TrustServerCertificate=true";
const DOC: &str = "guests/2026/11/folio-1001-id.pdf";
const STAMP: &str = "20261119080000";

/// A hotel machine: `%SystemRoot%`, Program Files, ProgramData and the new install folder.
struct Machine {
    _root: TempDir,
    system32: PathBuf,
    program_files: PathBuf,
    program_data: PathBuf,
    install: PathBuf,
}

impl Machine {
    fn new(tag: &str) -> Machine {
        let root = TempDir::new(tag);
        let r = root.path().to_path_buf();
        let m = Machine {
            system32: r.join("Windows").join("System32"),
            program_files: r.join("Program Files"),
            program_data: r.join("ProgramData"),
            install: r
                .join("ProgramData")
                .join("dl-agent")
                .join("bench")
                .join("arrival"),
            _root: root,
        };
        for d in [&m.system32, &m.program_files, &m.program_data] {
            std::fs::create_dir_all(d).expect("dir");
        }
        m
    }

    fn env(&self) -> Vec<(String, String)> {
        let windows = self.system32.parent().expect("windows");
        vec![
            ("SystemRoot".into(), windows.to_string_lossy().into_owned()),
            (
                "ProgramW6432".into(),
                self.program_files.to_string_lossy().into_owned(),
            ),
            (
                "ProgramData".into(),
                self.program_data.to_string_lossy().into_owned(),
            ),
        ]
    }

    /// Where the .NET agent was installed, and the folder the bench renamed it to.
    fn old_install(&self, stamp: &str, agent: &str) -> (PathBuf, PathBuf) {
        let original = self.program_files.join("MM-Arrival Agent");
        let renamed = self
            .program_files
            .join(format!("MM-Arrival Agent.bench-old-{stamp}"));
        std::fs::create_dir_all(renamed.join("data")).expect("dir");
        // Install.ps1 writes it with ConvertTo-Json | Set-Content -Encoding UTF8 (BOM).
        let data = original.join("data");
        let mut text = format!(
            "\u{feff}{{\"Agent\":{{\"CloudBaseUrl\":\"https://arrival.pms.id.vn\",\"PmsConnectionString\":{},\"IdentityFile\":{},\"IdempotencyFile\":{},\"ServiceName\":\"MM.Arrival.Agent\",\"AutoUpdate\":true{agent}}}}}",
            json(OLD_PMS),
            json(&data.join("agent-identity.bin").to_string_lossy()),
            json(&data.join("agent-idempotency.jsonl").to_string_lossy()),
        );
        text.push('\n');
        std::fs::write(renamed.join("appsettings.Production.json"), text).expect("settings");
        (original, renamed)
    }

    fn enroll(&self, args: &Args, extra: &[(&str, &str)]) -> (Result<(), String>, String) {
        let mut env: HashMap<String, String> = self.env().into_iter().collect();
        env.insert(TOKEN_ENV.into(), token(&identity()));
        for (k, v) in extra {
            env.insert((*k).into(), (*v).into());
        }
        let getenv = move |k: &str| env.get(k).cloned().unwrap_or_default();
        let mut out = Vec::new();
        let mut input = Cursor::new(Vec::new());
        let result = run_args(
            args,
            &std::env::temp_dir(),
            &mut Io {
                out: &mut out,
                input: &mut input,
                prompt: false,
                getenv: &getenv,
            },
        );
        (result, String::from_utf8(out).expect("utf-8"))
    }

    fn args(&self) -> Args {
        Args {
            cloud_url: "https://arrival.example".into(),
            install_dir: Some(self.install.clone()),
            legacy_dir: None,
            force: false,
            no_probe: true,
        }
    }

    /// The vault exactly as the module opens it after the enrol (src/agent/runtime.rs).
    fn module_vault(&self) -> LegalVault {
        let layout = Layout::new(self.install.clone(), Some(self.system32.clone()));
        let s = Settings::load(&self.install).expect("settings");
        let keys = Arc::new(TenantKeyStore::new(
            &s.tenant_key,
            layout.resolve(&s.tenant_key_file),
        ));
        LegalVault::new(layout.resolve(&s.vault_directory), keys)
    }

    fn nothing_written(&self) {
        assert!(!self.install.exists(), "the install folder was written");
    }
}

fn json(s: &str) -> String {
    dl_json::to_string(&dl_json::Value::from(s))
}

fn identity() -> Identity {
    Identity {
        tenant_id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
        agent_id: "agt-new".into(),
        kid: "k-1".into(),
        secret: to_base64(&[5u8; 32]),
    }
}

fn token(id: &Identity) -> String {
    to_base64(id.to_json().as_bytes())
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

fn key(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn fingerprint(k: &[u8; 32]) -> String {
    TenantKeyVault::new(k).expect("key").fingerprint()
}

/// A vault file the .NET `LegalVault` sealed with `k` (path bound as AAD).
fn seal(vault: &Path, k: &[u8; 32], path: &str, data: &[u8]) {
    let file = vault.join(path);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("dir");
    let sealed = TenantKeyVault::new(k)
        .expect("key")
        .wrap(data, path)
        .expect("wrap");
    std::fs::write(file, sealed).expect("seal");
}

fn record(install: &Path) -> dl_json::Value {
    let text = std::fs::read_to_string(install.join("data/dl-enroll.json")).expect("record");
    stj::parse(&text).expect("json")
}

fn key_source(install: &Path) -> String {
    record(install)
        .get("tenant_key")
        .and_then(|k| k.get("source"))
        .and_then(dl_json::Value::as_str)
        .expect("source")
        .to_string()
}

#[test]
fn a_stock_install_keeps_its_system32_key_and_the_vault_opens() {
    let m = Machine::new("cont-stock");
    let k = key(11);
    std::fs::write(m.system32.join("agent-tenant-key.bin"), k).expect("key");
    seal(&m.system32.join("vault"), &k, DOC, b"passport scan");
    m.old_install(STAMP, "");

    let (result, out) = m.enroll(&m.args(), &[]);
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(
        out.contains(&format!("fingerprint {} (kept", fingerprint(&k))),
        "{out}"
    );
    assert!(out.contains("sql_auth=windows"), "{out}");
    assert!(out.contains("1 sealed files"), "{out}");
    assert!(
        !out.contains("SMILE_FO;Integrated"),
        "the connection string was printed"
    );
    let s = Settings::load(&m.install).expect("settings");
    assert_eq!(s.pms_connection_string, OLD_PMS, "taken from the old agent");
    assert_eq!(s.tenant_key_file, "agent-tenant-key.bin", "nothing to name");
    assert!(
        !m.install.join("agent-tenant-key.bin").exists(),
        "no new key"
    );
    assert!(!m.install.join("data/agent-tenant-key.bin").exists());
    assert_eq!(
        m.module_vault().get(DOC).expect("open"),
        Some(b"passport scan".to_vec())
    );
    assert_eq!(key_source(&m.install), "kept");
    let rec = record(&m.install);
    assert_eq!(
        rec.get("identity_source").and_then(dl_json::Value::as_str),
        Some("fresh_enrol")
    );
    assert_eq!(
        rec.get("vault")
            .and_then(|v| v.get("sealed_files"))
            .and_then(dl_json::Value::as_u64),
        Some(1)
    );
}

#[test]
fn a_key_file_inside_the_renamed_folder_is_imported_alone() {
    let m = Machine::new("cont-import");
    let k = key(22);
    let (original, renamed) = m.old_install(
        STAMP,
        &format!(
            ",\"TenantKeyFile\":{},\"VaultDirectory\":{}",
            json(&original_path(&m, "data/tmk.bin")),
            json(&original_path(&m, "legal")),
        ),
    );
    assert_eq!(original, m.program_files.join("MM-Arrival Agent"));
    std::fs::write(renamed.join("data/tmk.bin"), k).expect("key");
    seal(&renamed.join("legal"), &k, DOC, b"contract");
    let (result, out) = m.enroll(&m.args(), &[]);
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(
        out.contains(&format!("fingerprint {} (imported", fingerprint(&k))),
        "{out}"
    );
    let s = Settings::load(&m.install).expect("settings");
    let imported = m.install.join("data/agent-tenant-key.bin");
    assert_eq!(Path::new(&s.tenant_key_file), imported.as_path());
    assert_eq!(
        Path::new(&s.vault_directory),
        renamed.join("legal").as_path()
    );
    assert_eq!(std::fs::read(&imported).expect("imported"), k.to_vec());
    assert!(renamed.join("data/tmk.bin").exists(), "the old file stays");
    assert_eq!(
        m.module_vault().get(DOC).expect("open"),
        Some(b"contract".to_vec())
    );
    assert_eq!(key_source(&m.install), "imported");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&imported)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

fn original_path(m: &Machine, rel: &str) -> String {
    m.program_files
        .join("MM-Arrival Agent")
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_configured_tenant_key_is_imported_into_a_key_file() {
    let m = Machine::new("cont-configured");
    let k = key(33);
    m.old_install(STAMP, &format!(",\"TenantKey\":{}", json(&to_base64(&k))));
    seal(&m.system32.join("vault"), &k, DOC, b"id card");
    let (result, out) = m.enroll(&m.args(), &[]);
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(!out.contains(&to_base64(&k)), "the key was printed");
    let s = Settings::load(&m.install).expect("settings");
    assert!(s.tenant_key.is_empty(), "the key is not copied in clear");
    assert_eq!(
        m.module_vault().get(DOC).expect("open"),
        Some(b"id card".to_vec())
    );
    assert_eq!(key_source(&m.install), "imported");
}

#[test]
fn two_different_old_keys_are_refused_until_one_folder_is_named() {
    let m = Machine::new("cont-two");
    let (k1, k2) = (key(44), key(45));
    let abs = format!(
        ",\"TenantKeyFile\":{}",
        json(&original_path(&m, "data/k.bin"))
    );
    let (_, older) = m.old_install("20261101000000", &abs);
    let (_, newer) = m.old_install(STAMP, &abs);
    std::fs::write(older.join("data/k.bin"), k1).expect("key");
    std::fs::write(newer.join("data/k.bin"), k2).expect("key");

    let (result, out) = m.enroll(&m.args(), &[]);
    let err = result.expect_err("ambiguous");
    assert!(err.contains("2 different tenant keys"), "{err}");
    assert!(
        err.contains(&fingerprint(&k1)) && err.contains(&fingerprint(&k2)),
        "{err}"
    );
    assert!(err.contains("--legacy-dir"), "{err}");
    assert!(!out.contains(&to_base64(&k1)));
    m.nothing_written();

    let named = Args {
        legacy_dir: Some(newer.clone()),
        ..m.args()
    };
    let (result, out) = m.enroll(&named, &[]);
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(
        out.contains(&format!("fingerprint {} (imported", fingerprint(&k2))),
        "{out}"
    );
}

#[test]
fn an_unreadable_old_key_stops_and_creates_nothing() {
    let m = Machine::new("cont-unreadable");
    // Off Windows a key of the wrong size stands for a DPAPI blob of another machine.
    std::fs::write(m.system32.join("agent-tenant-key.bin"), [1u8; 10]).expect("key");
    m.old_install(STAMP, "");
    let (result, _) = m.enroll(&m.args(), &[]);
    let err = result.expect_err("unreadable");
    assert!(err.contains("agent-tenant-key.bin"), "{err}");
    assert!(err.contains("key escrow"), "{err}");
    m.nothing_written();
}

#[test]
fn sealed_files_under_another_key_or_no_key_are_refused() {
    let m = Machine::new("cont-foreign");
    let (k, other) = (key(55), key(56));
    std::fs::write(m.system32.join("agent-tenant-key.bin"), k).expect("key");
    seal(&m.system32.join("vault"), &k, DOC, b"a");
    seal(&m.system32.join("vault"), &other, "guests/x.pdf", b"b");
    m.old_install(STAMP, "");
    let err = m.enroll(&m.args(), &[]).0.expect_err("foreign file");
    assert!(
        err.contains(&format!("1 sealed with {}", fingerprint(&other))),
        "{err}"
    );
    m.nothing_written();

    let lost = Machine::new("cont-lost");
    seal(&lost.system32.join("vault"), &k, DOC, b"a");
    lost.old_install(STAMP, "");
    let err = lost.enroll(&lost.args(), &[]).0.expect_err("no key");
    assert!(err.contains("could not open"), "{err}");
    lost.nothing_written();
}

#[test]
fn a_key_recovered_from_the_escrow_opens_the_vault() {
    let m = Machine::new("cont-supplied");
    let k = key(66);
    seal(&m.system32.join("vault"), &k, DOC, b"recovered");
    m.old_install(STAMP, "");
    let b64 = to_base64(&k);
    let (result, out) = m.enroll(&m.args(), &[(TENANT_KEY_ENV, b64.as_str())]);
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(!out.contains(&b64));
    assert_eq!(
        m.module_vault().get(DOC).expect("open"),
        Some(b"recovered".to_vec())
    );
    assert_eq!(key_source(&m.install), "supplied");
}

#[test]
fn no_old_agent_at_all_creates_a_key() {
    let m = Machine::new("cont-none");
    let (result, out) = m.enroll(
        &m.args(),
        &[(
            "DL_AGENT_ARRIVAL_PMS",
            "Server=.;Database=SMILE_FO;Integrated Security=true",
        )],
    );
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert_eq!(key_source(&m.install), "created");
    assert!(m.install.join("agent-tenant-key.bin").exists());
}

#[test]
fn the_operator_switches_the_old_windows_sign_in_to_a_sql_login() {
    let m = Machine::new("cont-sqlauth");
    m.old_install(STAMP, "");
    let (result, out) = m.enroll(
        &m.args(),
        &[
            (SQL_AUTH_ENV, "sql"),
            (SQL_USER_ENV, "checkin"),
            (SQL_PASSWORD_ENV, "S3cr;et-pw"),
        ],
    );
    result.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(
        out.contains("sql_auth=sql as checkin (preselected windows)"),
        "{out}"
    );
    assert!(!out.contains("S3cr;et-pw"));
    let s = Settings::load(&m.install).expect("settings");
    assert!(s.pms_connection_string.contains("User Id=checkin"));
    assert!(!s.pms_connection_string.contains("Integrated Security"));
    assert!(s.pms_connection_string.contains("Server=HOTEL\\SMILE"));

    let missing = Machine::new("cont-sqlauth-missing");
    missing.old_install(STAMP, "");
    let err = missing
        .enroll(
            &missing.args(),
            &[(SQL_AUTH_ENV, "sql"), (SQL_USER_ENV, "checkin")],
        )
        .0
        .expect_err("no password");
    assert!(err.contains("needs a password"), "{err}");
    missing.nothing_written();
}
