//! `--switch` / `--rollback` with a fake service and a fake token check. The enrolment
//! with a fresh code (a token the hub rejects) runs against the fake hub in
//! `tests/switch.rs`.

use std::cell::{Cell, RefCell};

use super::*;
use crate::config::PlainSealer;
use crate::enroll::legacy::tests::scratch;

const HOTEL: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const TOKEN: &str = "mbat-OLD-TOKEN-value";
const PASSWORD: &str = "Secr3t!pw";
const BLOB: &str = concat!(
    r#"{"HubBaseUrl":"https://hub.example.vn/api","AgentToken":"mbat-OLD-TOKEN-value","#,
    r#""HotelId":"0f8fad5b-d9cb-469f-a165-70867728950e","#,
    r#""SmileConnectionString":"Data Source=SMILESRV;Initial Catalog=SMILE_FO;User ID=smile;Password=Secr3t!pw;","#,
    r#""OriginCode":"Membership"}"#
);

#[derive(Default)]
struct FakeService {
    stops: Cell<u32>,
    starts: RefCell<Vec<String>>,
    fail: bool,
}

impl LegacyService for FakeService {
    fn exe_dir(&self) -> Option<PathBuf> {
        None
    }

    fn stop_and_disable(&self) -> Result<Stopped, String> {
        if self.fail {
            return Err("stop MM.Membership.Agent: access denied".into());
        }
        self.stops.set(self.stops.get() + 1);
        Ok(Stopped::Disabled {
            was_running: self.stops.get() == 1,
            start_before: if self.stops.get() == 1 {
                "automatic"
            } else {
                "disabled"
            }
            .into(),
        })
    }

    fn restore_and_start(&self, start: &str) -> Result<Started, String> {
        self.starts.borrow_mut().push(start.to_owned());
        Ok(Started::Restored { running: true })
    }
}

struct Site {
    root: PathBuf,
    config: PathBuf,
    record: PathBuf,
    install: PathBuf,
}

impl Site {
    fn new(tag: &str) -> Site {
        let root = scratch(tag);
        let s = Site {
            config: root.join("ProgramData/MM.Membership.Agent"),
            record: root.join("data/modules/membership"),
            install: root.join("Program Files/MM.Membership.Agent"),
            root,
        };
        std::fs::create_dir_all(&s.config).unwrap();
        std::fs::create_dir_all(&s.install).unwrap();
        s
    }

    fn seal(&self, plain: &str) {
        std::fs::write(
            self.config.join(FILE_NAME),
            PlainSealer.seal(plain.as_bytes()).unwrap(),
        )
        .unwrap();
    }

    fn switch<'a>(&self, service: &'a FakeService, probe: Probe<'a>, force: bool) -> Switch<'a> {
        Switch {
            hub_url: "https://hub.example.vn".into(),
            config_dir: self.config.clone(),
            record_dir: self.record.clone(),
            force,
            sealer: Arc::new(PlainSealer),
            service,
            places: Places {
                install_dirs: vec![self.install.clone()],
                environment: "Production".into(),
                portable: false,
            },
            probe,
        }
    }

    fn record(&self) -> Record {
        read_record(&self.record).unwrap().expect("record")
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn switch(sw: &Switch<'_>) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let mut stdin: &[u8] = b"";
    let r = run_switch(sw, Some(&mut stdin), &mut out, &no_env);
    (r, String::from_utf8(out).unwrap())
}

fn rollback(sw: &Switch<'_>) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let r = run_rollback(sw, &mut out);
    (r, String::from_utf8(out).unwrap())
}

fn assert_no_secret(text: &str) {
    for s in [TOKEN, PASSWORD, "Password"] {
        assert!(!text.contains(s), "{s} leaked: {text}");
    }
}

#[test]
fn keeps_the_dotnet_config_stops_the_service_and_records_without_secrets() {
    let site = Site::new("keep");
    site.seal(BLOB);
    let before = std::fs::read(site.config.join(FILE_NAME)).unwrap();
    let service = FakeService::default();
    let probes = Cell::new(0);
    let probe = |c: &AgentConfig| {
        assert_eq!(c.agent_token, TOKEN);
        probes.set(probes.get() + 1);
        Ok(())
    };
    let sw = site.switch(&service, &probe, false);
    let (r, out) = switch(&sw);
    r.unwrap();
    assert_no_secret(&out);
    assert!(
        out.contains("the hub accepts the .NET agent's token"),
        "{out}"
    );
    assert!(out.contains(&format!("(hotel {HOTEL})")), "{out}");
    assert_eq!(std::fs::read(site.config.join(FILE_NAME)).unwrap(), before);
    let rec = site.record();
    assert_eq!(
        (
            rec.state.as_str(),
            rec.source.as_str(),
            rec.rewrote,
            rec.backup.clone()
        ),
        (SWITCHED, FILE_NAME, false, None)
    );
    assert_eq!(rec.hotel_id, HOTEL);
    assert_eq!(rec.service, "disabled");
    assert!(rec.service_was_running);
    assert_eq!(rec.service_start_before.as_deref(), Some("automatic"));
    let text = std::fs::read_to_string(site.record.join(RECORD_FILE)).unwrap();
    assert_no_secret(&text);

    // The installer runs it again: nothing re-checked or rewritten, the service stays off.
    let (r, out) = switch(&sw);
    r.unwrap();
    assert!(out.contains("already switched"), "{out}");
    assert_eq!(probes.get(), 1);
    assert_eq!(service.stops.get(), 2);
    assert_eq!(site.record(), rec, "the first record (start type) is kept");
}

#[test]
fn seals_config_dpapi_from_the_dotnet_seed_configuration() {
    let site = Site::new("seed");
    std::fs::write(
        site.install.join("appsettings.hotel.json"),
        r#"{"Agent":{"HubBaseUrl":"https://hub.example.vn/","AgentToken":"mbat-OLD-TOKEN-value","HotelId":"0f8fad5b-d9cb-469f-a165-70867728950e","SmileConnectionString":"Server=s;Password=Secr3t!pw"}}"#,
    )
    .unwrap();
    let service = FakeService::default();
    let sw = site.switch(&service, &|_| Ok(()), false);
    let (r, out) = switch(&sw);
    r.unwrap();
    assert_no_secret(&out);
    assert!(out.contains("appsettings.hotel.json"), "{out}");
    let cfg = ConfigStore::new(&site.config, Arc::new(PlainSealer))
        .load()
        .unwrap()
        .unwrap();
    assert_eq!(cfg.hub_base_url, "https://hub.example.vn");
    assert_eq!(cfg.origin_code, "Membership");
    let rec = site.record();
    assert_eq!(
        (rec.source.as_str(), rec.rewrote, rec.reenrolled, rec.backup),
        ("appsettings", true, false, None)
    );
}

#[test]
fn a_rejected_token_without_a_code_changes_nothing() {
    let site = Site::new("rejected");
    site.seal(BLOB);
    let service = FakeService::default();
    let probe = |_: &AgentConfig| {
        Err(HubError::TokenRejected {
            retry_in: Duration::from_secs(900),
        })
    };
    let sw = site.switch(&service, &probe, false);
    let (r, out) = switch(&sw);
    let e = r.unwrap_err();
    assert!(e.contains(TOKEN_ENV), "{e}");
    assert_no_secret(&format!("{out}{e}"));
    assert_eq!(service.stops.get(), 0, "the .NET agent keeps running");
    assert!(read_record(&site.record).unwrap().is_none());
}

#[test]
fn an_unreachable_hub_keeps_the_config_and_a_stop_failure_is_an_error() {
    let site = Site::new("down");
    site.seal(BLOB);
    let service = FakeService {
        fail: true,
        ..FakeService::default()
    };
    let probe = |_: &AgentConfig| Err(HubError::Transport("connection refused".into()));
    let sw = site.switch(&service, &probe, false);
    let (r, out) = switch(&sw);
    assert!(out.contains("could not check the token"), "{out}");
    let e = r.unwrap_err();
    assert!(e.contains("by hand"), "{e}");
    assert_eq!(site.record().service, "", "recorded, service untouched");
}

#[test]
fn rollback_restarts_the_service_and_later_switches_wait_for_force() {
    let site = Site::new("rollback");
    site.seal(BLOB);
    let service = FakeService::default();
    let sw = site.switch(&service, &|_| Ok(()), false);
    switch(&sw).0.unwrap();
    let (r, out) = rollback(&sw);
    r.unwrap();
    assert!(out.contains("was not changed"), "{out}");
    assert!(out.contains("read_only"), "{out}");
    assert_eq!(*service.starts.borrow(), vec!["automatic".to_owned()]);
    assert_eq!(site.record().state, ROLLED_BACK);

    let (r, out) = switch(&sw);
    r.unwrap();
    assert!(out.contains("--force"), "{out}");
    assert_eq!(service.stops.get(), 1);

    let forced = site.switch(&service, &|_| Ok(()), true);
    switch(&forced).0.unwrap();
    assert_eq!(site.record().state, SWITCHED);
    assert_eq!(service.stops.get(), 2);
}

#[test]
fn rollback_restores_a_backup_only_when_its_token_still_works_or_with_force() {
    let site = Site::new("restore");
    site.seal(BLOB);
    let old = std::fs::read(site.config.join(FILE_NAME)).unwrap();
    let backup = site.config.join("config.dpapi.bak-20261119T010203Z");
    std::fs::write(&backup, &old).unwrap();
    std::fs::write(site.config.join(FILE_NAME), b"new").unwrap();
    let mut rec = Record {
        state: SWITCHED.into(),
        hotel_id: HOTEL.into(),
        source: "enrolled".into(),
        rewrote: true,
        reenrolled: true,
        backup: Some(backup.display().to_string()),
        service: "not_installed".into(),
        ..Record::default()
    };
    write_record(&site.record, &rec).unwrap();
    let service = FakeService::default();
    let sw = site.switch(&service, &|_| Ok(()), false);
    let (r, out) = rollback(&sw);
    r.unwrap();
    assert!(out.contains("no longer works"), "{out}");
    assert_eq!(std::fs::read(site.config.join(FILE_NAME)).unwrap(), b"new");
    assert!(service.starts.borrow().is_empty());

    rec.reenrolled = false;
    write_record(&site.record, &rec).unwrap();
    let (r, out) = rollback(&sw);
    r.unwrap();
    assert!(out.contains("restored"), "{out}");
    assert_eq!(std::fs::read(site.config.join(FILE_NAME)).unwrap(), old);
    let kept: Vec<_> = std::fs::read_dir(&site.config)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".switch-"))
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read(kept[0].path()).unwrap(), b"new");
}

#[test]
fn rollback_without_a_record_is_an_error() {
    let site = Site::new("norecord");
    let service = FakeService::default();
    let sw = site.switch(&service, &|_| Ok(()), false);
    assert!(
        rollback(&sw)
            .0
            .unwrap_err()
            .contains("nothing to roll back")
    );
}
