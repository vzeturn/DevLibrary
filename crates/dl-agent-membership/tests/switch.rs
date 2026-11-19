//! `membership-enroll --switch` against a fake hub: the .NET agent's `config.dpapi` is
//! kept while the hub accepts its token; a token the hub rejects is replaced by an
//! enrolment with a fresh code that keeps the old SMILE connection string and backs the
//! old file up. Nothing secret is printed or recorded.

mod support;

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;

use dl_agent_membership::config::{AgentConfig, ConfigStore, FILE_NAME, PlainSealer, Sealer};
use dl_agent_membership::enroll::TOKEN_ENV;
use dl_agent_membership::enroll::legacy::Places;
use dl_agent_membership::enroll::service::{LegacyService, Started, Stopped};
use dl_agent_membership::enroll::switch::{
    RECORD_FILE, Switch, hub_probe, read_record, run_rollback, run_switch,
};
use support::{FakeHub, HOTEL, TOKEN, TestDir};

const OLD_TOKEN: &str = "mbat-dotnet-old-token-42";
const SMILE: &str =
    "Data Source=SMILESRV\\SQL2008;Initial Catalog=SMILE_FO;User ID=smile;Password=Secr3t!pw;";
const CODE: &str = "MMA-ABCDE-FGHIJ";

#[derive(Default)]
struct Service {
    stops: Cell<u32>,
}

impl LegacyService for Service {
    fn exe_dir(&self) -> Option<PathBuf> {
        None
    }
    fn stop_and_disable(&self) -> Result<Stopped, String> {
        self.stops.set(self.stops.get() + 1);
        Ok(Stopped::Disabled {
            was_running: true,
            start_before: "automatic".into(),
        })
    }
    fn restore_and_start(&self, _: &str) -> Result<Started, String> {
        Ok(Started::Restored { running: true })
    }
}

fn site(hub: &FakeHub, tag: &str) -> (TestDir, ConfigStore) {
    let dir = TestDir::new(tag);
    let store = ConfigStore::new(dir.0.join("cfg"), Arc::new(PlainSealer));
    let old = AgentConfig {
        hub_base_url: format!("{}/api", hub.url()),
        agent_token: OLD_TOKEN.into(),
        hotel_id: HOTEL.parse().unwrap(),
        smile_connection_string: SMILE.into(),
        origin_code: "Membership".into(),
    };
    std::fs::create_dir_all(store.dir()).unwrap();
    std::fs::write(
        store.path(),
        PlainSealer.seal(old.to_json().as_bytes()).unwrap(),
    )
    .unwrap();
    (dir, store)
}

fn run(
    hub: &FakeHub,
    dir: &TestDir,
    service: &Service,
    rollback: bool,
) -> (Result<(), String>, String) {
    let sw = Switch {
        hub_url: hub.url(),
        config_dir: dir.0.join("cfg"),
        record_dir: dir.0.join("rec"),
        force: false,
        sealer: Arc::new(PlainSealer),
        service,
        places: Places {
            install_dirs: Vec::new(),
            environment: "Production".into(),
            portable: false,
        },
        probe: &hub_probe,
    };
    let env = |k: &str| match k {
        TOKEN_ENV => Some(CODE.to_owned()),
        "COMPUTERNAME" => Some("FRONTDESK-01".to_owned()),
        _ => None,
    };
    let mut out = Vec::new();
    let mut stdin: &[u8] = b"";
    let r = if rollback {
        run_rollback(&sw, &mut out)
    } else {
        run_switch(&sw, Some(&mut stdin), &mut out, &env)
    };
    (r, String::from_utf8(out).unwrap())
}

fn assert_no_secret(text: &str) {
    for s in [OLD_TOKEN, TOKEN, CODE, "Secr3t", "Password"] {
        assert!(!text.contains(s), "{s} leaked: {text}");
    }
}

#[test]
fn keeps_the_dotnet_config_while_the_hub_accepts_its_token() {
    let hub = FakeHub::start();
    let (dir, store) = site(&hub, "switch-keep");
    let before = std::fs::read(store.path()).unwrap();
    let service = Service::default();
    let (r, out) = run(&hub, &dir, &service, false);
    r.unwrap();
    assert_no_secret(&out);
    let probes = hub.to("/api/agent/sync-config");
    assert_eq!(probes.len(), 1);
    assert_eq!(
        probes[0].header("Authorization"),
        Some(format!("Bearer {OLD_TOKEN}").as_str())
    );
    assert!(hub.to("/api/agent/enroll").is_empty());
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
    assert_eq!(service.stops.get(), 1);
}

#[test]
fn a_rejected_token_is_replaced_keeping_the_smile_connection() {
    let hub = FakeHub::start();
    hub.reply_with(
        "/api/agent/sync-config",
        401,
        &[("X-MM-Agent-Auth-Error", "AGENT_TOKEN_REJECTED")],
        "",
    );
    hub.reply(
        "/api/agent/enroll",
        200,
        &format!(
            r#"{{"HotelId":"{HOTEL}","AgentToken":"{TOKEN}","HubBaseUrl":"{}/api","OriginCode":"AZ","EnrollmentId":null,"MachineChanged":false,"PreviousMachineName":null}}"#,
            hub.url()
        ),
    );
    let (dir, store) = site(&hub, "switch-reenroll");
    let old = std::fs::read(store.path()).unwrap();
    let service = Service::default();
    let (r, out) = run(&hub, &dir, &service, false);
    r.unwrap();
    assert_no_secret(&out);
    let enroll = hub.to("/api/agent/enroll");
    assert_eq!(enroll.len(), 1);
    assert!(
        enroll[0]
            .body
            .contains(r#""SmileDataSource":"SMILESRV\\SQL2008""#),
        "{}",
        enroll[0].body
    );
    let cfg = store.load().unwrap().unwrap();
    assert_eq!(cfg.agent_token, TOKEN);
    assert_eq!(cfg.smile_connection_string, SMILE);
    let rec = read_record(&dir.0.join("rec")).unwrap().unwrap();
    assert!(rec.reenrolled && rec.rewrote);
    let backup = PathBuf::from(rec.backup.expect("backup"));
    assert_eq!(std::fs::read(&backup).unwrap(), old);
    assert_no_secret(&std::fs::read_to_string(dir.0.join("rec").join(RECORD_FILE)).unwrap());

    // The backup's token is dead: rollback keeps the current file for the .NET agent.
    let (r, out) = run(&hub, &dir, &service, true);
    r.unwrap();
    assert_no_secret(&out);
    assert!(out.contains("no longer works"), "{out}");
    assert_eq!(store.load().unwrap().unwrap().agent_token, TOKEN);
    assert!(std::fs::read_dir(store.dir()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&format!("{FILE_NAME}.bak-"))
    }));
}
