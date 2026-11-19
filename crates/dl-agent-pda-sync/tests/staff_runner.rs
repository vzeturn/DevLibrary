//! Port of `StaffSyncRunnerTests.cs`: one staff sync cycle on the store (ADR 0101): key,
//! hashed clerk list, send only on change or once a day, one refetch of a stale key, and
//! nothing but counts in the logs.

use dl_agent_pda_sync::config::time::{ManualClock, SharedClock, Timestamp};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::staff as staff_pin;
use dl_agent_pda_sync::contracts::wire::{
    StaffClerk, StaffSnapshot, staff_function_ids, staff_pin_status, staff_sync_limits,
};
use dl_agent_pda_sync::reporting::{Level, Logger, LoggerConfig, MemorySink, Value};
use dl_agent_pda_sync::security::{FileSecretStore, SharedSecretStore, names};
use dl_agent_pda_sync::staff::pin_hasher;
use dl_agent_pda_sync::staff::runner::{RESEND_AFTER, digest};
use dl_agent_pda_sync::staff::{
    PinKeyProvider, STATE_FILE_NAME, StaffCloud, StaffCloudResult, StaffCloudStatus, StaffError,
    StaffPinKey, StaffSource, StaffSyncOutcome, StaffSyncParts, StaffSyncRunner,
};
use dl_agent_pda_sync::util::ctx::Ctx;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CS_TESTS: &str = include_str!("../testdata/cs/Staff/StaffPinHasherTests.cs");

fn golden(marker: &str) -> &'static str {
    let at = CS_TESTS.find(marker).unwrap() + marker.len();
    let rest = &CS_TESTS[at..];
    let start = rest.find('"').unwrap() + 1;
    let len = rest[start..].find('"').unwrap();
    &rest[start..start + len]
}

fn golden_key() -> Vec<u8> {
    dl_encoding::hex::decode(golden("GoldenKey =")).unwrap()
}

fn other_key() -> Vec<u8> {
    (1..=32u8).collect()
}

fn b64(b: &[u8]) -> String {
    dl_encoding::base64::STANDARD.encode(b)
}

#[derive(Clone)]
struct PosClerk {
    key: &'static str,
    name: &'static str,
    touch_id: Option<&'static str>,
    active: bool,
}

#[derive(Default)]
struct FakeStaffSource {
    clerks: Mutex<Vec<PosClerk>>,
    keys_seen: Mutex<Vec<Vec<u8>>>,
}

impl StaffSource for FakeStaffSource {
    fn read(&self, pin_key: &[u8], _ctx: &Ctx) -> Result<Vec<StaffClerk>, StaffError> {
        self.keys_seen.lock().unwrap().push(pin_key.to_vec());
        Ok(self
            .clerks
            .lock()
            .unwrap()
            .iter()
            .map(|c| {
                let (lookup, status) = pin_hasher::hash(pin_key, c.touch_id, c.active);
                StaffClerk {
                    key: c.key.to_owned(),
                    name: c.name.to_owned(),
                    active: c.active,
                    pin_lookup: lookup,
                    pin_status: status.to_owned(),
                    security: Some(1),
                    rvcs: vec![1],
                    functions: vec![staff_function_ids::NEW_CHECK.to_owned()],
                }
            })
            .collect())
    }
}

#[derive(Default)]
struct FakeStaffCloud {
    keys: Mutex<VecDeque<StaffCloudResult>>,
    send_answers: Mutex<VecDeque<StaffCloudResult>>,
    sent: Mutex<Vec<StaffSnapshot>>,
    key_calls: Mutex<u32>,
}

impl StaffCloud for FakeStaffCloud {
    fn get_pin_key(&self, _ctx: &Ctx) -> Result<StaffCloudResult, StaffError> {
        *self.key_calls.lock().unwrap() += 1;
        Ok(self
            .keys
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| StaffCloudResult::new(StaffCloudStatus::Failed, 0)))
    }

    fn send(&self, snapshot: &StaffSnapshot, _ctx: &Ctx) -> Result<StaffCloudResult, StaffError> {
        self.sent.lock().unwrap().push(snapshot.clone());
        Ok(self
            .send_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| StaffCloudResult::new(StaffCloudStatus::Ok, 200)))
    }
}

fn key_answer(key: Vec<u8>) -> StaffCloudResult {
    let key_id = staff_pin::key_id(&key);
    StaffCloudResult {
        status: StaffCloudStatus::Ok,
        http_status: 200,
        pin_key: Some(StaffPinKey { key, key_id }),
    }
}

struct Env {
    dir: PathBuf,
    secrets: SharedSecretStore,
    clock: Arc<ManualClock>,
    source: Arc<FakeStaffSource>,
    cloud: Arc<FakeStaffCloud>,
    sink: Arc<MemorySink>,
    runner: StaffSyncRunner,
    ctx: Ctx,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Env {
    fn new() -> Env {
        let dir = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let secrets: SharedSecretStore = Arc::new(FileSecretStore::new(dir.join("secrets")));
        let clock = Arc::new(ManualClock::new(Timestamp::from_unix_seconds(
            1_780_000_000,
        )));
        let source = Arc::new(FakeStaffSource::default());
        let cloud = Arc::new(FakeStaffCloud::default());
        cloud
            .keys
            .lock()
            .unwrap()
            .push_back(key_answer(golden_key()));
        source.clerks.lock().unwrap().extend([
            PosClerk {
                key: "LAN",
                name: "Nguyễn Thị Lan",
                touch_id: Some(golden("GoldenPin =")),
                active: true,
            },
            PosClerk {
                key: "BINH",
                name: "Trần Bình",
                touch_id: Some("731045"),
                active: true,
            },
        ]);
        let sink = Arc::new(MemorySink::default());
        let log = Logger::new(LoggerConfig {
            sinks: vec![(sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        let shared: SharedClock = clock.clone();
        let runner = StaffSyncRunner::new(
            StaffSyncParts {
                source: source.clone(),
                cloud: cloud.clone(),
                keys: Arc::new(PinKeyProvider::new(secrets.clone(), cloud.clone())),
                state_file: dir.join(STATE_FILE_NAME),
                agent_id: "agent-1".to_owned(),
                agent_version: "1.2.3".to_owned(),
                clock: shared,
            },
            &log,
        );
        Env {
            dir,
            secrets,
            clock,
            source,
            cloud,
            sink,
            runner,
            ctx: Ctx::background(),
        }
    }

    fn run(&self) -> StaffSyncOutcome {
        self.runner.run_once(&self.ctx).unwrap()
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(STATE_FILE_NAME)
    }

    fn sent(&self) -> Vec<StaffSnapshot> {
        self.cloud.sent.lock().unwrap().clone()
    }

    fn warnings(&self) -> Vec<String> {
        self.sink.messages_at(Level::Warning)
    }

    fn stored_key(&self) -> Option<Vec<u8>> {
        self.secrets.load(names::PIN_KEY).unwrap()
    }
}

fn clerk<'a>(s: &'a StaffSnapshot, key: &str) -> &'a StaffClerk {
    s.clerks.iter().find(|c| c.key == key).unwrap()
}

#[test]
fn sends_the_hashed_list_once_then_only_when_it_changes_or_a_day_has_passed() {
    let env = Env::new();
    assert_eq!(env.run(), StaffSyncOutcome::Sent);
    let sent = env.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].agent_id, "agent-1");
    assert_eq!(sent[0].key_id, golden("GoldenKeyId ="));
    let keys: Vec<_> = sent[0].clerks.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["BINH", "LAN"]);
    assert_eq!(
        clerk(&sent[0], "LAN").pin_lookup.as_deref(),
        Some(golden("GoldenLookup ="))
    );

    assert_eq!(env.run(), StaffSyncOutcome::Unchanged);
    env.clock.advance(RESEND_AFTER - Duration::from_secs(60));
    assert_eq!(env.run(), StaffSyncOutcome::Unchanged);
    env.clock.advance(Duration::from_secs(120));
    assert_eq!(env.run(), StaffSyncOutcome::Sent);

    env.source.clerks.lock().unwrap()[1].active = false;
    assert_eq!(env.run(), StaffSyncOutcome::Sent);
    let sent = env.sent();
    assert_eq!(sent.len(), 3);
    let binh = clerk(&sent[2], "BINH");
    assert!(!binh.active && binh.pin_lookup.is_none() && binh.pin_status == staff_pin_status::NONE);
}

#[test]
fn the_key_is_fetched_once_and_kept_in_the_secret_store() {
    let env = Env::new();
    env.run();
    env.clock.advance(RESEND_AFTER);
    env.run();

    assert_eq!(*env.cloud.key_calls.lock().unwrap(), 1);
    assert_eq!(env.stored_key(), Some(golden_key()));
    let seen = env.source.keys_seen.lock().unwrap();
    assert!(!seen.is_empty() && seen.iter().all(|k| *k == golden_key()));
}

#[test]
fn a_stale_key_is_dropped_fetched_again_and_the_list_resent_once() {
    let env = Env::new();
    env.cloud
        .keys
        .lock()
        .unwrap()
        .push_back(key_answer(other_key()));
    env.cloud
        .send_answers
        .lock()
        .unwrap()
        .push_back(StaffCloudResult::new(StaffCloudStatus::KeyStale, 409));

    assert_eq!(env.run(), StaffSyncOutcome::Sent);

    assert_eq!(*env.cloud.key_calls.lock().unwrap(), 2);
    let sent = env.sent();
    let ids: Vec<_> = sent.iter().map(|s| s.key_id.clone()).collect();
    assert_eq!(
        ids,
        [
            golden("GoldenKeyId =").to_owned(),
            staff_pin::key_id(&other_key())
        ]
    );
    assert_eq!(env.stored_key(), Some(other_key()));
    let expected = b64(&staff_pin::lookup(&other_key(), golden("GoldenPin =")).unwrap());
    assert_eq!(clerk(&sent[1], "LAN").pin_lookup, Some(expected));
}

#[test]
fn a_key_stale_twice_in_a_row_fails_the_cycle_without_saving_state() {
    let env = Env::new();
    env.cloud
        .keys
        .lock()
        .unwrap()
        .push_back(key_answer(other_key()));
    for _ in 0..2 {
        env.cloud
            .send_answers
            .lock()
            .unwrap()
            .push_back(StaffCloudResult::new(StaffCloudStatus::KeyStale, 409));
    }

    assert_eq!(env.run(), StaffSyncOutcome::Failed);
    assert_eq!(env.sent().len(), 2);
    assert!(!env.state_path().exists());
}

#[test]
fn a_key_that_cannot_be_fetched_fails_the_cycle_and_reads_nothing() {
    for (status, http) in [
        (StaffCloudStatus::Unauthorized, 403),
        (StaffCloudStatus::NotSupported, 404),
        (StaffCloudStatus::Failed, 0),
    ] {
        let env = Env::new();
        {
            let mut keys = env.cloud.keys.lock().unwrap();
            keys.clear();
            keys.push_back(StaffCloudResult::new(status, http));
        }

        assert_eq!(env.run(), StaffSyncOutcome::Failed);

        assert!(env.source.keys_seen.lock().unwrap().is_empty());
        assert!(env.sent().is_empty());
        assert_eq!(env.stored_key(), None);
        let w = env.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].starts_with("Staff sync: the PIN key could not be fetched (")
                && w[0].contains(status.name())
                && w[0].ends_with(&format!("HTTP {http})")),
            "{status}: {w:?}"
        );
    }
}

#[test]
fn a_refused_send_fails_and_is_retried_next_cycle() {
    let env = Env::new();
    env.cloud
        .send_answers
        .lock()
        .unwrap()
        .push_back(StaffCloudResult::new(StaffCloudStatus::Rejected, 400));

    assert_eq!(env.run(), StaffSyncOutcome::Failed);
    assert_eq!(env.run(), StaffSyncOutcome::Sent);
    assert_eq!(env.sent().len(), 2);
}

#[test]
fn an_empty_store_sends_nothing_since_it_would_disable_every_clerk() {
    let env = Env::new();
    env.source.clerks.lock().unwrap().clear();

    assert_eq!(env.run(), StaffSyncOutcome::Empty);
    assert!(env.sent().is_empty());
}

#[test]
fn a_corrupt_state_file_costs_one_resend() {
    let env = Env::new();
    env.run();
    std::fs::write(env.state_path(), "{ not json").unwrap();

    assert_eq!(env.run(), StaffSyncOutcome::Sent);
    assert_eq!(env.run(), StaffSyncOutcome::Unchanged);
    assert!(
        env.warnings()
            .iter()
            .any(|m| m.contains("could not be read")),
        "{:?}",
        env.warnings()
    );
}

#[test]
fn the_state_file_is_what_system_text_json_writes() {
    let env = Env::new();
    env.run();
    let text = std::fs::read_to_string(env.state_path()).unwrap();
    let d = digest(&env.sent()[0]);
    assert_eq!(
        text,
        format!(r#"{{"ListDigest":"{d}","SentAt":"2026-05-28T20:26:40+00:00"}}"#)
    );
}

#[test]
fn over_long_keys_and_every_clerk_of_a_case_repeated_key_are_left_out_and_the_rest_sorted() {
    let env = Env::new();
    let c = |key: String| StaffClerk {
        key,
        name: "n".to_owned(),
        active: true,
        pin_lookup: None,
        pin_status: staff_pin_status::NONE.to_owned(),
        security: None,
        rvcs: vec![1],
        functions: Vec::new(),
    };
    let max = staff_sync_limits::MAX_KEY_LENGTH;
    let cleaned = env.runner.clean(vec![
        c("zed".into()),
        c("Anh".into()),
        c("ANH".into()),
        c("k".repeat(max + 1)),
        c("Binh".into()),
        c("m".repeat(max)),
    ]);

    let keys: Vec<_> = cleaned.iter().map(|c| c.key.clone()).collect();
    assert_eq!(keys, ["Binh".to_owned(), "m".repeat(max), "zed".to_owned()]);
    let events = env.sink.events();
    assert!(events.iter().any(|e| e.level == Level::Warning
        && e.property("TooLong") == Some(&Value::Int(1))
        && e.property("Repeated") == Some(&Value::Int(2))));
}

#[test]
fn an_agent_update_alone_does_not_change_the_digest() {
    let clerks = vec![StaffClerk {
        key: "A".into(),
        name: "n".into(),
        active: true,
        pin_lookup: None,
        pin_status: staff_pin_status::NONE.into(),
        security: Some(1),
        rvcs: vec![1],
        functions: Vec::new(),
    }];
    let snap = |version: &str, key_id: &str| StaffSnapshot {
        agent_id: "agent".into(),
        agent_version: version.into(),
        key_id: key_id.into(),
        clerks: clerks.clone(),
    };
    let a = digest(&snap("1.0.0", "k"));
    assert_eq!(digest(&snap("2.0.0", "k")), a);
    assert_ne!(digest(&snap("1.0.0", "k2")), a);
}

#[test]
fn no_pin_lookup_key_or_name_reaches_a_log_and_no_pin_or_key_reaches_the_wire() {
    let env = Env::new();
    env.cloud
        .keys
        .lock()
        .unwrap()
        .push_back(key_answer(other_key()));
    env.cloud
        .send_answers
        .lock()
        .unwrap()
        .push_back(StaffCloudResult::new(StaffCloudStatus::KeyStale, 409));
    env.run();
    let mk = |key: &str, name: &str| StaffClerk {
        key: key.into(),
        name: name.into(),
        active: true,
        pin_lookup: None,
        pin_status: staff_pin_status::NONE.into(),
        security: None,
        rvcs: Vec::new(),
        functions: Vec::new(),
    };
    env.runner
        .clean(vec![mk("LAN", "Nguyễn Thị Lan"), mk("lan", "Lan")]);

    let pin = golden("GoldenPin =");
    let secrets = [
        pin.to_owned(),
        "731045".to_owned(),
        golden("GoldenLookup =").to_owned(),
        b64(&golden_key()),
        dl_encoding::hex::encode(golden_key()).to_uppercase(),
        b64(&other_key()),
        "Nguyễn Thị Lan".to_owned(),
        "Trần Bình".to_owned(),
    ];
    let logged: String = env
        .sink
        .events()
        .iter()
        .map(|e| {
            let props: Vec<String> = e
                .properties
                .iter()
                .map(|p| format!("{:?}", p.value))
                .collect();
            format!("{} {}", e.render_message(), props.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!logged.is_empty());
    for s in &secrets {
        assert!(
            !logged.contains(s.as_str()),
            "a staff sync log line carries counts and statuses only: {s}"
        );
    }
    for wire in env.sent().iter().map(WireJson::to_json) {
        for s in [
            pin.to_owned(),
            "731045".to_owned(),
            b64(&golden_key()),
            b64(&other_key()),
        ] {
            assert!(!wire.contains(&s));
        }
        for s in ["touch", "password", "card"] {
            assert!(!wire.contains(s));
        }
    }
}
