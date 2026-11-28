//! `master.upsert` entity `cashier` (ADR 0150, RT-0834) on the in-memory fake, mirroring
//! the .NET `SiteCommandCashierTests`. The SQL is in `commands_cashier_sql.rs`.

mod commands_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use commands_support::{FakeCashier, FakeStore, cashier_key, clock};
use dl_agent_pda_sync::commands::{CashierApply, CashierCommandSupport, SiteCommandExecutor};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::cashier::{self, CashierWrite};
use dl_agent_pda_sync::contracts::staff;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, MasterUpsertPayload, MasterUpsertResult, SyncCommand, SyncCommandResult,
    site_command_types, staff_function_ids,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::staff::{StaffError, StaffPinKey};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_uuid::Uuid;

const PIN_KEY: [u8; 32] = [7; 32];

#[derive(Default)]
struct FakeSupport {
    key: Mutex<Option<Vec<u8>>>,
    nudges: AtomicUsize,
}

impl CashierCommandSupport for FakeSupport {
    fn pin_key(&self, key_id: Option<&str>, _: &Ctx) -> Result<Option<StaffPinKey>, StaffError> {
        let key = self.key.lock().unwrap().clone();
        Ok(key.and_then(|k| {
            let id = staff::key_id(&k);
            (key_id.is_none() || key_id == Some(id.as_str()))
                .then_some(StaffPinKey { key: k, key_id: id })
        }))
    }

    fn clerks_changed(&self) {
        self.nudges.fetch_add(1, Ordering::SeqCst);
    }
}

struct T {
    store: Arc<FakeStore>,
    support: Arc<FakeSupport>,
}

impl T {
    fn new() -> T {
        let support = Arc::new(FakeSupport::default());
        *support.key.lock().unwrap() = Some(PIN_KEY.to_vec());
        T {
            store: FakeStore::new(),
            support,
        }
    }

    fn run_with(&self, c: &SyncCommand, with_support: bool) -> SyncCommandResult {
        let e = SiteCommandExecutor::new(self.store.clone(), clock(1_795_420_800), &Logger::none());
        let e = if with_support {
            e.with_cashiers(self.support.clone())
        } else {
            e
        };
        e.execute(c, &Ctx::background()).unwrap()
    }

    fn run(&self, c: &SyncCommand) -> SyncCommandResult {
        self.run_with(c, true)
    }

    fn seed(&self, key: &str, pin: &str) -> String {
        let c = FakeCashier {
            key: key.to_owned(),
            name: "An".to_owned(),
            active: true,
            security: Some(1),
            touch_id: Some(pin.to_owned()),
            rvcs: vec![1],
            functions: vec![
                staff_function_ids::NEW_CHECK.to_owned(),
                staff_function_ids::PICKUP.to_owned(),
            ],
        };
        let hash = hash_of(&c);
        self.store.state().cashiers.insert(cashier_key(key), c);
        hash
    }

    fn row(&self, key: &str) -> FakeCashier {
        self.store.state().cashiers[&cashier_key(key)].clone()
    }

    fn edit(&self, key: &str, f: impl FnOnce(&mut FakeCashier)) {
        f(self
            .store
            .state()
            .cashiers
            .get_mut(&cashier_key(key))
            .unwrap());
    }

    fn nudges(&self) -> usize {
        self.support.nudges.load(Ordering::SeqCst)
    }
}

fn hash_of(c: &FakeCashier) -> String {
    cashier::row_hash(&c.clerk(&PIN_KEY))
}

fn seal(key: &[u8], clerk: &str, pin: &str) -> String {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&rand_u32().to_le_bytes());
    cashier::seal_pin(key, clerk, pin, nonce).unwrap()
}

fn rand_u32() -> u32 {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst) as u32
}

fn write(name: &str, pin: Option<&str>, key: &str, seal_key: &[u8]) -> CashierWrite {
    CashierWrite {
        name: name.to_owned(),
        active: true,
        security: 1,
        rvcs: vec![1, 2],
        functions: vec![
            staff_function_ids::PAYMENT.to_owned(),
            staff_function_ids::CASHIER.to_owned(),
        ],
        pin_sealed: pin.map(|p| seal(seal_key, key, p)),
        pin_key_id: pin.map(|_| staff::key_id(seal_key)),
    }
}

fn w(name: &str, pin: Option<&str>) -> CashierWrite {
    write(name, pin, "C01", &PIN_KEY)
}

fn upsert_id(expected: Option<&str>, write: &CashierWrite, key: &str, id: Uuid) -> SyncCommand {
    let payload = MasterUpsertPayload {
        entity: cashier::ENTITY.to_owned(),
        key: key.to_owned(),
        fields: write.to_fields(),
        expected_hash: expected.map(str::to_owned),
        actor: "admin-1".to_owned(),
    };
    SyncCommand {
        command_id: id,
        type_: site_command_types::MASTER_UPSERT.into(),
        aggregate_key: format!("master:cashier:{key}"),
        expected_hash: None,
        payload: payload.to_element(),
        created_by: "admin-1".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn upsert(expected: Option<&str>, write: &CashierWrite, key: &str) -> SyncCommand {
    upsert_id(expected, write, key, Uuid::new_v4())
}

fn refused(r: &SyncCommandResult, reason: &str) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert_eq!(r.reason.as_deref(), Some(reason));
}

fn refused_invalid(r: &SyncCommandResult) {
    assert_eq!(r.status, CommandStatus::Rejected, "{:?}", r.reason);
    assert!(
        r.reason.as_deref().unwrap().starts_with("invalid_payload"),
        "{:?}",
        r.reason
    );
}

fn result_of(r: &SyncCommandResult) -> MasterUpsertResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    MasterUpsertResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

#[test]
fn edit_of_an_unchanged_clerk_writes_all_three_tables_returns_the_new_hash_and_wakes_the_staff_sync()
 {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    let r = t.run(&upsert(Some(&hash), &w("An Nguyễn", Some("5678")), "C01"));
    let res = result_of(&r);
    let row = t.row("C01");
    assert_eq!(row.name, "An Nguyễn");
    assert_eq!(row.touch_id.as_deref(), Some("5678"));
    assert_eq!(row.rvcs, vec![1, 2]);
    let mut functions = row.functions.clone();
    functions.sort();
    let mut expected = vec![
        staff_function_ids::PAYMENT.to_owned(),
        staff_function_ids::CASHIER.to_owned(),
    ];
    expected.sort();
    assert_eq!(functions, expected);
    assert_eq!(res.entity, "cashier");
    assert_eq!(res.row_hash, hash_of(&row));
    assert_ne!(res.row_hash, hash);
    assert_eq!(t.nudges(), 1);
}

#[test]
fn without_a_pin_the_stored_pin_stays() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    result_of(&t.run(&upsert(Some(&hash), &w("An Nguyễn", None), "C01")));
    assert_eq!(t.row("C01").touch_id.as_deref(), Some("1234"));
}

#[test]
fn pos_win_edit_after_the_editor_read_is_refused_and_nothing_is_written() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    t.edit("C01", |c| c.name = "An (POS)".into());
    let store_side = t.row("C01");
    refused(
        &t.run(&upsert(Some(&hash), &w("X", None), "C01")),
        "master_changed",
    );
    assert_eq!(t.row("C01"), store_side);
    assert_eq!(t.nudges(), 0);
}

#[test]
fn pos_win_pin_change_is_a_conflict_too() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    t.edit("C01", |c| c.touch_id = Some("9999".into()));
    refused(
        &t.run(&upsert(Some(&hash), &w("X", None), "C01")),
        "master_changed",
    );
}

#[test]
fn cloud_first_edit_then_a_second_edit_on_the_old_hash_is_refused_and_a_later_pos_edit_is_not_blocked()
 {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    result_of(&t.run(&upsert(Some(&hash), &w("First", None), "C01")));
    refused(
        &t.run(&upsert(Some(&hash), &w("Second", None), "C01")),
        "master_changed",
    );
    assert_eq!(t.row("C01").name, "First");

    t.edit("C01", |c| c.name = "POS".into());
    let fresh = hash_of(&t.row("C01"));
    result_of(&t.run(&upsert(Some(&fresh), &w("Third", None), "C01")));
}

#[test]
fn create_inserts_the_clerk_with_its_pin() {
    let t = T::new();
    let r = t.run(&upsert(
        None,
        &write("Nam", Some("4321"), "N1", &PIN_KEY),
        "N1",
    ));
    let res = result_of(&r);
    assert_eq!(t.row("N1").touch_id.as_deref(), Some("4321"));
    assert_eq!(res.key, "N1");
    assert_eq!(res.row_hash, hash_of(&t.row("N1")));
}

#[test]
fn create_of_a_key_pos_win_already_made_is_a_conflict() {
    let t = T::new();
    t.seed("N1", "1234");
    refused(
        &t.run(&upsert(
            None,
            &write("Nam", Some("4321"), "N1", &PIN_KEY),
            "N1",
        )),
        "master_changed",
    );
}

#[test]
fn the_store_spelling_of_the_key_is_kept_and_answered() {
    let t = T::new();
    let hash = t.seed("c01", "1234");
    let res = result_of(&t.run(&upsert(Some(&hash), &w("An", None), "C01")));
    assert_eq!(res.key, "c01");
    assert_eq!(t.row("C01").key, "c01");
}

#[test]
fn create_needs_a_pin_and_an_edit_of_a_deleted_clerk_is_row_not_found() {
    let t = T::new();
    refused_invalid(&t.run(&upsert(None, &write("Nam", None, "N1", &PIN_KEY), "N1")));
    refused(
        &t.run(&upsert(
            Some(&"a".repeat(64)),
            &write("Nam", None, "N1", &PIN_KEY),
            "N1",
        )),
        "master_row_not_found",
    );
}

#[test]
fn a_pin_another_active_clerk_uses_is_refused_but_a_disabled_clerk_does_not_count() {
    let t = T::new();
    t.seed("X1", "5678");
    let hash = t.seed("C01", "1234");
    refused(
        &t.run(&upsert(Some(&hash), &w("An", Some("5678")), "C01")),
        "cashier_pin_taken",
    );
    assert_eq!(t.row("C01").touch_id.as_deref(), Some("1234"));

    t.edit("X1", |c| c.active = false);
    result_of(&t.run(&upsert(Some(&hash), &w("An", Some("5678")), "C01")));
}

#[test]
fn a_pin_sealed_for_another_clerk_or_under_another_key_is_never_written() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    let mut other_clerk = w("An", None);
    other_clerk.pin_sealed = Some(seal(&PIN_KEY, "X1", "5678"));
    other_clerk.pin_key_id = Some(staff::key_id(&PIN_KEY));
    let r = t.run(&upsert(Some(&hash), &other_clerk, "C01"));
    refused_invalid(&r);
    assert!(
        !r.reason
            .as_deref()
            .unwrap()
            .contains(other_clerk.pin_sealed.as_deref().unwrap())
    );

    let other_key = write("An", Some("5678"), "C01", &[9; 32]);
    refused(
        &t.run(&upsert(Some(&hash), &other_key, "C01")),
        "cashier_pin_key_unavailable",
    );
    assert_eq!(t.row("C01").touch_id.as_deref(), Some("1234"));
}

#[test]
fn an_unusable_pin_is_invalid() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    refused_invalid(&t.run(&upsert(Some(&hash), &w("An", Some("   ")), "C01")));
}

#[test]
fn a_bad_key_is_invalid() {
    let t = T::new();
    refused_invalid(&t.run(&upsert(
        None,
        &write("Nam", Some("4321"), "SML", &PIN_KEY),
        "SML",
    )));
    let long = "K".repeat(11);
    refused_invalid(&t.run(&upsert(
        None,
        &write("Nam", Some("4321"), &long, &PIN_KEY),
        &long,
    )));
}

#[test]
fn a_refused_schema_and_an_agent_without_the_staff_sync_write_nothing() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    t.store.knobs().cashier_refusal = Some("cashier_schema_unsupported".into());
    refused(
        &t.run(&upsert(Some(&hash), &w("X", None), "C01")),
        "cashier_schema_unsupported",
    );
    t.store.knobs().cashier_refusal = None;
    refused(
        &t.run_with(&upsert(Some(&hash), &w("X", None), "C01"), false),
        "master_unsupported_entity",
    );
    assert_eq!(t.row("C01").name, "An");
    assert_eq!(t.nudges(), 0);
}

#[test]
fn redelivery_answers_the_logged_result() {
    let t = T::new();
    let hash = t.seed("C01", "1234");
    let id = Uuid::new_v4();
    let first = t.run(&upsert_id(Some(&hash), &w("First", None), "C01", id));
    t.edit("C01", |c| c.name = "POS".into());
    let again = t.run(&upsert_id(Some(&hash), &w("First", None), "C01", id));
    assert_eq!(again.status, first.status);
    assert_eq!(t.row("C01").name, "POS");
}

#[test]
fn apply_never_prints_the_pin() {
    let allowed: Vec<String> = Vec::new();
    let apply = CashierApply {
        key: "C01",
        create: false,
        name: "An",
        active: true,
        security: 1,
        pin: Some("5678"),
        rvcs: &[1],
        allowed: &allowed,
    };
    assert!(!format!("{apply:?}").contains("5678"));
    let write = w("An", Some("5678"));
    let printed = format!("{write:?}");
    assert!(!printed.contains(write.pin_sealed.as_deref().unwrap()));
}
