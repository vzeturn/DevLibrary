//! `dl-agent arrival-enroll` (src/tools/enroll.rs): what Install.ps1 writes, secrets only
//! from the environment or stdin and never echoed, and the already-enrolled guard.

mod common;

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use common::TempDir;
use dl_agent_arrival::agent::layout::Layout;
use dl_agent_arrival::agent::secrets::Identity;
use dl_agent_arrival::agent::settings::Settings;
use dl_agent_arrival::contracts::values::to_base64;
use dl_agent_arrival::tools::enroll::{Args, Io, PMS_ENV, TOKEN_ENV, run_args};

const PMS: &str = "Server=10.0.0.5;Database=SMILE_FO;User ID=arrival;Password=Pa55-w0rd-x";

fn identity(agent: &str) -> Identity {
    Identity {
        tenant_id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
        agent_id: agent.into(),
        kid: "k-1".into(),
        secret: to_base64(&dl_rand::secure_bytes::<32>().expect("random")),
    }
}

/// The token the platform console shows: base64url of the identity JSON, unpadded.
fn token(id: &Identity) -> String {
    to_base64(id.to_json().as_bytes())
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

fn args(install: &Path) -> Args {
    Args {
        cloud_url: "https://arrival.example/".into(),
        install_dir: Some(install.to_path_buf()),
        legacy_dir: None,
        force: false,
        no_probe: true,
    }
}

/// Runs the tool with `env` and `stdin`; `(result, printed text)`.
fn run(args: &Args, env: &[(&str, &str)], stdin: &str) -> (Result<(), String>, String) {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let getenv = move |k: &str| env.get(k).cloned().unwrap_or_default();
    let mut out = Vec::new();
    let mut input = Cursor::new(stdin.as_bytes().to_vec());
    let fallback = std::env::temp_dir();
    let result = run_args(
        args,
        &fallback,
        &mut Io {
            out: &mut out,
            input: &mut input,
            prompt: false,
            getenv: &getenv,
        },
    );
    (result, String::from_utf8(out).expect("utf-8"))
}

fn no_secrets(text: &str, id: &Identity) {
    for secret in [PMS, "Pa55-w0rd-x", id.secret.as_str(), token(id).as_str()] {
        assert!(!text.contains(secret), "a secret was printed: {text}");
    }
}

#[test]
fn writes_what_install_ps1_writes_and_the_module_reads_it() {
    let dir = TempDir::new("enroll");
    let id = identity("agt-1");
    let (result, out) = run(
        &args(dir.path()),
        &[(TOKEN_ENV, &token(&id)), (PMS_ENV, PMS)],
        "",
    );
    result.expect("enroll");
    no_secrets(&out, &id);
    assert!(
        out.contains("enrolled agent agt-1 for tenant 0f8fad5b-d9cb-469f-a165-70867728950e"),
        "{out}"
    );
    assert!(out.contains("fingerprint "), "{out}");

    let install = dir.path();
    let settings = Settings::load(install).expect("settings");
    assert_eq!(settings.cloud_base_url, "https://arrival.example");
    assert_eq!(settings.pms_connection_string, PMS);
    let identity_file = install.join("data").join("agent-identity.bin");
    assert_eq!(Path::new(&settings.identity_file), identity_file.as_path());
    assert_eq!(
        Path::new(&settings.idempotency_file),
        install
            .join("data")
            .join("agent-idempotency.jsonl")
            .as_path()
    );
    // The base file of the .NET agent came along (its watch and limits apply).
    assert_eq!(settings.watches.len(), 1);
    assert_eq!(settings.max_concurrency, 4);
    let layout = Layout::new(install.to_path_buf(), None);
    let loaded = Identity::load(&layout.resolve(&settings.identity_file))
        .expect("load")
        .expect("saved");
    assert_eq!(loaded, id);
    assert!(
        layout.resolve(&settings.tenant_key_file).exists(),
        "tenant key created"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&install.join("appsettings.Production.json")), 0o600);
        assert_eq!(mode(&identity_file), 0o600);
        assert_eq!(mode(&install.join("data")), 0o700);
    }

    // Enrolled: a second run changes nothing unless forced.
    let other = identity("agt-2");
    let (result, out) = run(
        &args(dir.path()),
        &[(TOKEN_ENV, &token(&other)), (PMS_ENV, PMS)],
        "",
    );
    result.expect("second run");
    assert!(out.contains("already enrolled"), "{out}");
    assert_eq!(Identity::load(&identity_file).expect("load"), Some(id));
    let forced = Args {
        force: true,
        ..args(dir.path())
    };
    run(&forced, &[(TOKEN_ENV, &token(&other))], "")
        .0
        .expect("forced");
    assert_eq!(Identity::load(&identity_file).expect("load"), Some(other));
    // The saved connection string is kept when none is given.
    assert_eq!(
        Settings::load(install)
            .expect("settings")
            .pms_connection_string,
        PMS
    );
}

#[test]
fn reads_the_token_and_connection_string_from_stdin() {
    let dir = TempDir::new("enroll-stdin");
    let id = identity("agt-stdin");
    let (result, out) = run(&args(dir.path()), &[], &format!("{}\n{PMS}\n", token(&id)));
    result.expect("enroll");
    no_secrets(&out, &id);
    assert_eq!(
        Settings::load(dir.path())
            .expect("settings")
            .pms_connection_string,
        PMS
    );
    assert_eq!(
        Identity::load(&dir.path().join("data").join("agent-identity.bin")).expect("load"),
        Some(id)
    );
}

/// Arguments, environment and the expected error text.
type Case = (Args, Vec<(&'static str, String)>, &'static str);

#[test]
fn refuses_bad_input_without_writing_anything() {
    let dir = TempDir::new("enroll-bad");
    let id = identity("agt-bad");
    let cases: Vec<Case> = vec![
        (
            args(dir.path()),
            vec![(PMS_ENV, PMS.into())],
            "no enrollment token",
        ),
        (
            args(dir.path()),
            vec![(TOKEN_ENV, "not-a-token".into()), (PMS_ENV, PMS.into())],
            "Invalid enrollment token",
        ),
        (
            args(dir.path()),
            vec![(TOKEN_ENV, token(&id))],
            "no PMS connection string",
        ),
        (
            Args {
                cloud_url: "http://arrival.example".into(),
                ..args(dir.path())
            },
            vec![(TOKEN_ENV, token(&id)), (PMS_ENV, PMS.into())],
            "https://",
        ),
    ];
    for (a, env, expected) in cases {
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let err = run(&a, &env, "").0.expect_err(expected);
        assert!(err.contains(expected), "{err}");
        no_secrets(&err, &id);
    }
    assert!(
        std::fs::read_dir(dir.path()).expect("dir").next().is_none(),
        "something was written"
    );
}

#[test]
fn a_failed_probe_names_no_secret_and_writes_nothing() {
    let dir = TempDir::new("enroll-probe");
    let id = identity("agt-probe");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let pms = format!(
        "Server=127.0.0.1,{port};Database=PMS;User ID=arrival;Password=Pa55-w0rd-x;Connect Timeout=3"
    );
    let probing = Args {
        no_probe: false,
        ..args(dir.path())
    };
    let (result, out) = run(&probing, &[(TOKEN_ENV, &token(&id)), (PMS_ENV, &pms)], "");
    let err = result.expect_err("nothing listens");
    assert!(err.contains("cannot connect to the PMS database"), "{err}");
    for text in [&err, &out] {
        no_secrets(text, &id);
        assert!(!text.contains(&pms));
    }
    assert!(!dir.path().join("data").exists());
}

#[test]
fn a_fresh_key_is_never_made_over_a_vault_with_files() {
    let dir = TempDir::new("enroll-vault");
    let layout = Layout::new(dir.path().to_path_buf(), None);
    let vault = layout.resolve(&Settings::default().vault_directory);
    std::fs::create_dir_all(vault.join("guests")).expect("vault");
    std::fs::write(vault.join("guests").join("doc"), [0xAA_u8; 64]).expect("sealed file");
    let id = identity("agt-v");
    let (result, out) = run(
        &args(dir.path()),
        &[(TOKEN_ENV, &token(&id)), (PMS_ENV, PMS)],
        "",
    );
    let e = result.expect_err("a new key would not open the vault");
    assert!(e.contains("legal vault"), "{e}");
    no_secrets(&out, &id);
    no_secrets(&e, &id);
    let settings = Settings::load(dir.path()).expect("settings");
    assert!(
        !layout.resolve(&settings.tenant_key_file).exists(),
        "no key was made"
    );
    assert!(
        !layout.resolve(&settings.identity_file).exists(),
        "the site is not enrolled"
    );
}
