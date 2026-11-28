//! `CashierSupport` and `StaffSyncNudge` (ADR 0150, `StaffSyncNudge.cs`): the PIN key by
//! key id with one refetch of a stale stored key, and the staff sync wake-up.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dl_agent_pda_sync::commands::CashierCommandSupport;
use dl_agent_pda_sync::contracts::staff as staff_pin;
use dl_agent_pda_sync::contracts::wire::StaffSnapshot;
use dl_agent_pda_sync::security::{FileSecretStore, SharedSecretStore, names};
use dl_agent_pda_sync::staff::{
    CashierSupport, PinKeyProvider, StaffCloud, StaffCloudResult, StaffCloudStatus, StaffError,
    StaffPinKey, StaffSyncNudge,
};
use dl_agent_pda_sync::util::ctx::Ctx;

#[derive(Default)]
struct FakeCloud {
    keys: Mutex<VecDeque<Vec<u8>>>,
    calls: Mutex<u32>,
}

impl StaffCloud for FakeCloud {
    fn get_pin_key(&self, _: &Ctx) -> Result<StaffCloudResult, StaffError> {
        *self.calls.lock().unwrap() += 1;
        Ok(match self.keys.lock().unwrap().pop_front() {
            Some(key) => StaffCloudResult {
                status: StaffCloudStatus::Ok,
                http_status: 200,
                pin_key: Some(StaffPinKey {
                    key_id: staff_pin::key_id(&key),
                    key,
                }),
            },
            None => StaffCloudResult::new(StaffCloudStatus::Failed, 0),
        })
    }

    fn send(&self, _: &StaffSnapshot, _: &Ctx) -> Result<StaffCloudResult, StaffError> {
        Ok(StaffCloudResult::new(StaffCloudStatus::Ok, 200))
    }
}

struct Env {
    dir: PathBuf,
    secrets: SharedSecretStore,
    cloud: Arc<FakeCloud>,
    nudge: Arc<StaffSyncNudge>,
    support: CashierSupport,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const OLD: [u8; 32] = [1; 32];
const NEW: [u8; 32] = [2; 32];

impl Env {
    fn new(stored: Option<&[u8]>, cloud_keys: &[&[u8]]) -> Env {
        let dir = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(dl_uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let secrets: SharedSecretStore = Arc::new(FileSecretStore::new(dir.join("secrets")));
        if let Some(k) = stored {
            secrets.save(names::PIN_KEY, k).unwrap();
        }
        let cloud = Arc::new(FakeCloud::default());
        cloud
            .keys
            .lock()
            .unwrap()
            .extend(cloud_keys.iter().map(|k| k.to_vec()));
        let nudge = Arc::new(StaffSyncNudge::new());
        let support = CashierSupport::new(
            Arc::new(PinKeyProvider::new(secrets.clone(), cloud.clone())),
            nudge.clone(),
        );
        Env {
            dir,
            secrets,
            cloud,
            nudge,
            support,
        }
    }

    fn key(&self, key_id: Option<&str>) -> Option<Vec<u8>> {
        self.support
            .pin_key(key_id, &Ctx::background())
            .unwrap()
            .map(|k| k.key)
    }

    fn calls(&self) -> u32 {
        *self.cloud.calls.lock().unwrap()
    }
}

#[test]
fn the_stored_key_answers_without_a_key_id_or_with_its_own() {
    let e = Env::new(Some(&OLD), &[]);
    assert_eq!(e.key(None), Some(OLD.to_vec()));
    assert_eq!(e.key(Some(&staff_pin::key_id(&OLD))), Some(OLD.to_vec()));
    assert_eq!(e.calls(), 0);
}

#[test]
fn a_stale_stored_key_is_fetched_again_once() {
    let e = Env::new(Some(&OLD), &[&NEW]);
    assert_eq!(e.key(Some(&staff_pin::key_id(&NEW))), Some(NEW.to_vec()));
    assert_eq!(e.calls(), 1);
    assert_eq!(e.secrets.load(names::PIN_KEY).unwrap(), Some(NEW.to_vec()));
}

#[test]
fn an_unknown_key_id_or_no_key_is_none() {
    let e = Env::new(Some(&OLD), &[&OLD]);
    assert_eq!(e.key(Some(&staff_pin::key_id(&NEW))), None);
    assert_eq!(e.calls(), 1);

    let none = Env::new(None, &[]);
    assert_eq!(none.key(None), None);
    assert_eq!(none.key(Some(&staff_pin::key_id(&NEW))), None);
}

#[test]
fn clerks_changed_wakes_the_staff_sync_wait_and_nudges_coalesce() {
    let e = Env::new(None, &[]);
    e.support.clerks_changed();
    e.support.clerks_changed();
    let ctx = Ctx::background();
    let start = Instant::now();
    assert!(e.nudge.wait(Duration::from_secs(30), &ctx));
    assert!(start.elapsed() < Duration::from_secs(5));
    // One pending wake-up only: the next wait runs its full (short) interval.
    let start = Instant::now();
    assert!(e.nudge.wait(Duration::from_millis(120), &ctx));
    assert!(start.elapsed() >= Duration::from_millis(100));
}
