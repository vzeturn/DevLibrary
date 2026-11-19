//! `membership-enroll` against a fake hub: the code and the SMILE connection string come
//! from the environment (or stdin), are never printed, and `config.dpapi` is written only
//! when the hub answered with an address on the origin that was called.

mod support;

use std::sync::Arc;

use dl_agent_membership::config::{ConfigStore, FILE_NAME, PlainSealer};
use dl_agent_membership::enroll::{EnrollArgs, SMILE_ENV, TOKEN_ENV, run_enroll};
use support::{FakeHub, HOTEL, TOKEN, TestDir};

const CODE: &str = "ENROLL-CODE-7f3a";
const SMILE: &str =
    "Data Source=SMILESRV\\SQL2008;Initial Catalog=SMILE_FO;User ID=smile;Password=Secr3t!pw;";
const ENROLL: &str = "/api/agent/enroll";

fn answer(hub: &FakeHub, base: &str) {
    hub.reply(
        ENROLL,
        200,
        &format!(
            r#"{{"HotelId":"{HOTEL}","AgentToken":"{TOKEN}","HubBaseUrl":"{base}","OriginCode":"AZ","EnrollmentId":"3f2504e0-4f89-11d3-9a0c-0305e82c3301","MachineChanged":false,"PreviousMachineName":null}}"#
        ),
    );
}

fn env(k: &str) -> Option<String> {
    match k {
        TOKEN_ENV => Some(CODE.into()),
        SMILE_ENV => Some(SMILE.into()),
        "COMPUTERNAME" => Some("FRONTDESK-01".into()),
        _ => None,
    }
}

fn enroll(
    hub_url: &str,
    dir: &TestDir,
    force: bool,
    env: &dyn Fn(&str) -> Option<String>,
    stdin: &[u8],
) -> (Result<(), String>, String) {
    let args = EnrollArgs {
        hub_url: hub_url.into(),
        config_dir: dir.0.clone(),
        force,
    };
    let mut input = stdin;
    let mut out = Vec::new();
    let r = run_enroll(
        &args,
        Arc::new(PlainSealer),
        Some(&mut input),
        &mut out,
        env,
    );
    (r, String::from_utf8(out).unwrap())
}

fn assert_no_secret(text: &str) {
    for secret in [TOKEN, CODE, "Secr3t", "Password"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
}

#[test]
fn enrolls_writes_the_config_and_prints_no_secret() {
    let hub = FakeHub::start();
    answer(&hub, &format!("{}/api", hub.url()));
    let dir = TestDir::new("enroll");
    let (r, out) = enroll(&hub.url(), &dir, false, &env, b"");
    r.unwrap();
    assert_no_secret(&out);
    assert!(out.contains("SMILE: SMILESRV\\SQL2008 / SMILE_FO"), "{out}");
    assert!(
        out.contains(&format!(
            "enrolled for hotel {HOTEL} (enrollment 3f2504e0-4f89-11d3-9a0c-0305e82c3301)"
        )),
        "{out}"
    );

    let sent = hub.to(ENROLL);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].header("Authorization"),
        None,
        "enroll carries no token"
    );
    let body = dl_json::parse(&sent[0].body).unwrap();
    let s = |k: &str| body.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    assert_eq!(s("Code").as_deref(), Some(CODE));
    assert_eq!(s("MachineName").as_deref(), Some("FRONTDESK-01"));
    assert_eq!(s("SmileDataSource").as_deref(), Some("SMILESRV\\SQL2008"));
    assert_eq!(s("SmileDatabase").as_deref(), Some("SMILE_FO"));
    assert!(s("AgentVersion").unwrap().starts_with("dl-agent/"));
    assert_no_secret(&sent[0].body.replace(CODE, ""));

    let cfg = ConfigStore::new(dir.0.clone(), Arc::new(PlainSealer))
        .load()
        .unwrap()
        .expect("config written");
    assert_eq!(cfg.agent_token, TOKEN);
    assert_eq!(cfg.hotel_id.to_string(), HOTEL);
    assert_eq!(cfg.smile_connection_string, SMILE);
    assert_eq!(cfg.origin_code, "AZ");
    assert_eq!(cfg.hub_base_url, format!("{}/api", hub.url()));
    assert_no_secret(&format!("{cfg:?}"));
}

#[test]
fn a_second_run_skips_unless_forced() {
    let hub = FakeHub::start();
    answer(&hub, &hub.url());
    let dir = TestDir::new("again");
    enroll(&hub.url(), &dir, false, &env, b"").0.unwrap();

    let (r, out) = enroll(&hub.url(), &dir, false, &env, b"");
    r.unwrap();
    assert!(
        out.starts_with(&format!("already enrolled for hotel {HOTEL}")),
        "{out}"
    );
    assert_eq!(hub.to(ENROLL).len(), 1, "the code is not spent again");

    let (r, out) = enroll(&hub.url(), &dir, true, &env, b"");
    r.unwrap();
    assert_no_secret(&out);
    assert!(
        out.contains(&format!("previous {FILE_NAME} kept as ")),
        "{out}"
    );
    assert_eq!(hub.to(ENROLL).len(), 2);
}

#[test]
fn the_code_may_come_from_stdin_and_smile_from_the_previous_config() {
    let hub = FakeHub::start();
    answer(&hub, &hub.url());
    let dir = TestDir::new("stdin");
    enroll(&hub.url(), &dir, false, &env, b"").0.unwrap();
    let no_env = |k: &str| (k == "COMPUTERNAME").then(|| "FRONTDESK-02".to_owned());
    let (r, out) = enroll(
        &hub.url(),
        &dir,
        true,
        &no_env,
        "\u{feff}CODE-FROM-STDIN\r\n".as_bytes(),
    );
    r.unwrap();
    assert!(!out.contains("CODE-FROM-STDIN"), "{out}");
    let body = dl_json::parse(&hub.to(ENROLL)[1].body).unwrap();
    assert_eq!(
        body.get("Code").and_then(|v| v.as_str()),
        Some("CODE-FROM-STDIN")
    );
    let cfg = ConfigStore::new(dir.0.clone(), Arc::new(PlainSealer))
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(
        cfg.smile_connection_string, SMILE,
        "kept from the previous config"
    );
}

#[test]
fn another_origin_in_the_answer_saves_nothing() {
    let hub = FakeHub::start();
    answer(&hub, "https://elsewhere.example");
    let dir = TestDir::new("origin");
    let (r, out) = enroll(&hub.url(), &dir, false, &env, b"");
    let e = r.unwrap_err();
    assert!(e.starts_with("enrollment not saved: "), "{e}");
    assert_no_secret(&e);
    assert_no_secret(&out);
    assert!(!dir.0.join(FILE_NAME).exists());
}

#[test]
fn a_refused_code_or_an_insecure_hub_saves_nothing() {
    let hub = FakeHub::start();
    hub.reply(ENROLL, 400, r#"{"error":"code expired"}"#);
    let dir = TestDir::new("refused");
    let e = enroll(&hub.url(), &dir, false, &env, b"").0.unwrap_err();
    assert!(e.contains("HTTP 400"), "{e}");
    assert!(!dir.0.join(FILE_NAME).exists());

    let e = enroll("http://hub.example", &dir, false, &env, b"")
        .0
        .unwrap_err();
    assert!(e.contains("https"), "{e}");
    assert_eq!(
        hub.to(ENROLL).len(),
        1,
        "an insecure --hub-url is refused before any call"
    );

    let no_code = |_: &str| None;
    let e = enroll(&hub.url(), &dir, false, &no_code, b"")
        .0
        .unwrap_err();
    assert!(e.contains(TOKEN_ENV), "{e}");
}
