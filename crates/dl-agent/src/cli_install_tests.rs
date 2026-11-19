use std::path::Path;
use std::time::Duration;

use dl_ed25519::SigningKey;
use dl_hash::{Digest, Sha256};

use super::*;
use crate::hub::tests_support::FakeHub;
use crate::store::testdir::TestDir;

fn signed(product: &str, version: &str, bytes: &[u8], seed: u8) -> String {
    let text = format!(
        r#"{{"product":"{product}","channel":"stable","version":"{version}","published_at":10,
        "rollout_percent":100,"min_version":null,"allow_downgrade":false,"notes":"",
        "artifacts":[{{"target":"windows-x86_64","url":"https://hub/v1/artifacts/k1","sha256":"{}","size":{}}}],
        "key_id":"rel-1","signature":"{}"}}"#,
        dl_encoding::hex::encode(Sha256::digest(bytes)),
        bytes.len(),
        dl_encoding::base64::STANDARD.encode([0u8; 64]),
    );
    let mut m = Manifest::parse(&text).unwrap();
    m.sign("rel-1", &SigningKey::from_seed(&[seed; 32]));
    m.to_json()
}

fn keys(seed: u8) -> TrustedKeys {
    let pk = SigningKey::from_seed(&[seed; 32]).verifying_key();
    TrustedKeys::new()
        .with_key(
            "rel-1",
            &dl_encoding::base64::STANDARD.encode(pk.as_bytes()),
        )
        .unwrap()
}

fn app() -> App {
    App::new("dl-agent", "DevLibrary Agent")
}

#[test]
fn a_target_is_a_channel_or_a_version() {
    assert_eq!(Target::parse("stable").unwrap(), Target::Stable);
    assert_eq!(Target::parse(" Latest ").unwrap(), Target::Stable);
    assert_eq!(Target::parse("canary").unwrap(), Target::Canary);
    assert_eq!(
        Target::parse("1.4.2").unwrap(),
        Target::Version("1.4.2".parse().unwrap())
    );
    assert_eq!(Target::parse("auto").unwrap(), Target::Auto);
    assert_eq!(Target::Auto.channel(), None);
    assert!(Target::parse("beta").is_err());
    assert!(Target::parse("1.4").is_err());
    assert_eq!(Target::Canary.channel(), Some("canary"));
    assert_eq!(Target::Version("1.0.0".parse().unwrap()).channel(), None);
}

#[test]
fn the_mode_follows_the_hub_option_and_where_the_binary_runs_from() {
    let canary = Some(Target::Canary);
    // The one-line installer hands off with --hub: the whole machine setup.
    assert_eq!(mode(true, true, false, None), Mode::Machine);
    assert_eq!(mode(true, true, true, canary.clone()), Mode::Machine);
    // A downloaded binary run by hand on Windows sets the machine up too.
    assert_eq!(mode(true, false, false, None), Mode::Machine);
    // The installed binary: a target switches, nothing re-registers the service.
    assert_eq!(
        mode(true, false, true, canary.clone()),
        Mode::Switch(Target::Canary)
    );
    assert_eq!(mode(true, false, true, None), Mode::Service);
    // Linux keeps `install` = register the unit.
    assert_eq!(mode(false, false, false, None), Mode::Service);
    assert_eq!(
        mode(false, false, false, canary),
        Mode::Switch(Target::Canary)
    );
}

#[test]
fn the_install_path_is_program_files_devlibrary_service() {
    let env = |k: &str| match k {
        "ProgramW6432" => Some(r"D:\Programs".to_string()),
        _ => None,
    };
    assert_eq!(
        install_exe("dl-agent", env),
        Path::new(r"D:\Programs")
            .join("DevLibrary")
            .join("dl-agent")
            .join("dl-agent.exe")
    );
    let none = |_: &str| None;
    assert!(install_exe("x", none).starts_with(r"C:\Program Files"));
    assert!(same_path(Path::new(r"C:\A\b.exe"), Path::new("c:/a/B.EXE")));
    assert!(!same_path(
        Path::new(r"C:\A\b.exe"),
        Path::new(r"C:\A\c.exe")
    ));
}

#[test]
fn hubs_are_split_trimmed_and_deduplicated() {
    assert_eq!(
        split_hubs(" https://a/, https://b ,,https://a"),
        vec!["https://a".to_string(), "https://b".to_string()]
    );
    assert!(split_hubs("").is_empty());
}

#[test]
fn a_build_is_verified_by_signature_product_size_and_sha256() {
    let bytes = b"agent build";
    let text = signed("dl-agent", "2.0.0", bytes, 7);
    let m = verify_build(&text, bytes, "windows-x86_64", "dl-agent", &keys(7)).unwrap();
    assert_eq!(m.version.to_string(), "2.0.0");
    // Another key, another product, another target, other bytes: refused.
    assert!(verify_build(&text, bytes, "windows-x86_64", "dl-agent", &keys(8)).is_err());
    assert!(verify_build(&text, bytes, "windows-x86_64", "azolo-agent", &keys(7)).is_err());
    assert!(verify_build(&text, bytes, "windows7-x86", "dl-agent", &keys(7)).is_err());
    assert!(
        verify_build(
            &text,
            b"agent bu1ld",
            "windows-x86_64",
            "dl-agent",
            &keys(7)
        )
        .is_err()
    );
    // A proxy's HTML page is no manifest.
    assert!(
        verify_build(
            "<html>blocked</html>",
            bytes,
            "windows-x86_64",
            "dl-agent",
            &keys(7)
        )
        .is_err()
    );
}

#[test]
fn verify_self_exits_65_on_a_build_that_does_not_match() {
    let d = TestDir::new("verify-self");
    let file = d.0.join("tmp.exe");
    std::fs::write(&file, b"agent build").unwrap();
    let manifest = d.0.join("m.json");
    std::fs::write(&manifest, signed("dl-agent", "2.0.0", b"another build", 7)).unwrap();
    let cmd = Command::new("t").subcommand(verify_grammar(Arg::option("data")));
    let m = cmd
        .parse_from_with_env(
            [
                "t",
                "verify-self",
                "--manifest",
                manifest.to_str().unwrap(),
                "--file",
                file.to_str().unwrap(),
                "--target",
                "windows-x86_64",
            ],
            |_| None,
        )
        .unwrap();
    let (_, sub) = m.subcommand().unwrap();
    assert_eq!(main_verify(&app(), sub), EXIT_VERIFY);
    let missing = cmd
        .parse_from_with_env(["t", "verify-self", "--manifest", "nope.json"], |_| None)
        .unwrap();
    let (_, sub) = missing.subcommand().unwrap();
    assert_eq!(main_verify(&app(), sub), crate::cli::EXIT_CONFIG);
}

#[test]
fn the_install_grammar_takes_the_installer_options_but_never_a_password() {
    let cmd = Command::new("t").subcommand(install_grammar(Arg::option("data")));
    let env = |k: &str| (k == "DL_AGENT_ENROLL_CODE").then(|| "ABCD-1234".to_string());
    let m = cmd
        .parse_from_with_env(
            [
                "t",
                "install",
                "canary",
                "--hub",
                "https://a,https://b",
                "--sql-auth",
                "sql",
                "--sql-user",
                "smile",
                "--replace",
                "MM.Arrival*",
                "--replace",
                "Old*",
                "--setup",
                "arrival-enroll",
                "--link-timeout",
                "30",
                "--no-defender",
            ],
            env,
        )
        .unwrap();
    let (_, sub) = m.subcommand().unwrap();
    let o = MachineOpts::from_matches(sub, Some(Target::Canary)).unwrap();
    assert_eq!(
        o.hubs,
        vec!["https://a".to_string(), "https://b".to_string()]
    );
    assert_eq!(o.code.as_deref(), Some("ABCD-1234"));
    assert_eq!(o.sql.auth.as_deref(), Some("sql"));
    assert_eq!(
        o.replace,
        vec!["MM.Arrival*".to_string(), "Old*".to_string()]
    );
    assert_eq!((o.link_timeout, o.defender), (30, false));
    assert!(
        cmd.clone()
            .parse_from_with_env(["t", "install", "--sql-password", "x"], |_| None)
            .is_err()
    );
    assert!(
        cmd.parse_from_with_env(["t", "install", "--sql-auth", "ntlm"], |_| None)
            .is_err()
    );
}

#[test]
fn a_manifest_fetch_rejects_html_and_tries_canary_after_stable() {
    let v: Version = "1.2.3".parse().unwrap();
    let hub = FakeHub::default();
    hub.push(404, "").push(200, "{\"product\":\"dl-agent\"}");
    let hubs = ["https://hub".to_string()];
    let text = fetch_manifest(&hub, &hubs, "dl-agent", &v)
        .unwrap()
        .unwrap();
    assert!(text.starts_with('{'));
    let urls: Vec<String> = hub.seen().into_iter().map(|s| s.url).collect();
    assert_eq!(
        urls,
        vec![
            "https://hub/install/manifest/dl-agent/stable/1.2.3".to_string(),
            "https://hub/install/manifest/dl-agent/canary/1.2.3".to_string()
        ]
    );
    let proxy = FakeHub::default();
    proxy.push(200, "<html>login</html>");
    assert!(fetch_manifest(&proxy, &hubs, "dl-agent", &v).is_err());
    let none = FakeHub::default();
    none.push(404, "").push(404, "");
    assert_eq!(fetch_manifest(&none, &hubs, "dl-agent", &v).unwrap(), None);
}

#[test]
fn update_refuses_while_pinned_and_when_the_service_is_not_running() {
    let d = TestDir::new("cli-update");
    optout::set_pin(&d.0, Some(&"1.0.0".parse().unwrap())).unwrap();
    let f = update(&app(), &d.0, Duration::from_millis(10)).unwrap_err();
    assert_eq!(f.code, crate::cli::EXIT_CONFIG);
    assert!(f.next.contains("install stable"));
    optout::set_pin(&d.0, None).unwrap();
    let f = update(&app(), &d.0, Duration::from_millis(10)).unwrap_err();
    assert_eq!(f.code, EXIT_UNAVAILABLE);
    assert!(!d.0.join(optout::REQUEST_FILE).exists());
}

#[test]
fn update_asks_the_running_service_and_prints_its_answer() {
    let d = TestDir::new("cli-update-ok");
    let beat = crate::watchdog::Beat {
        at: now(),
        hub_at: now(),
        version: "1.0.0".into(),
        pid: 1,
        channel: "stable".into(),
        skew: 0,
        links: Vec::new(),
    };
    crate::watchdog::write_beat(&d.0, &beat).unwrap();
    let dir = d.0.clone();
    // The service: takes the request and answers "up to date".
    let service = std::thread::spawn(move || {
        for _ in 0..200 {
            if optout::take_update_request(&dir).unwrap() {
                let r = optout::UpdateResult {
                    at: now(),
                    ok: true,
                    installed: false,
                    from: "1.0.0".into(),
                    to: String::new(),
                    channel: "stable".into(),
                    output: "no update to install".into(),
                };
                optout::write_update_result(&dir, &r).unwrap();
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    });
    assert_eq!(update(&app(), &d.0, Duration::from_secs(5)).unwrap(), 0);
    assert!(service.join().unwrap());
}

#[test]
fn switching_needs_an_enrolled_machine_and_pins_the_running_version() {
    let d = TestDir::new("cli-switch");
    let a = app();
    let f = switch(&a, &d.0, &Target::Canary).unwrap_err();
    assert_eq!(f.code, crate::cli::EXIT_CONFIG);
    crate::hub::tests_support::identity(&["https://hub"])
        .save(&d.0)
        .unwrap();
    assert_eq!(
        switch(&a, &d.0, &Target::Version(a.version.clone())).unwrap(),
        0
    );
    assert_eq!(optout::pin(&d.0), Some(a.version.clone()));
    // Following a channel removes the pin, then asks the service (not running here).
    let f = switch(&a, &d.0, &Target::Stable).unwrap_err();
    assert_eq!(f.code, EXIT_UNAVAILABLE);
    assert_eq!(optout::pin(&d.0), None);
    assert_eq!(optout::channel(&d.0).as_deref(), Some("stable"));
}
