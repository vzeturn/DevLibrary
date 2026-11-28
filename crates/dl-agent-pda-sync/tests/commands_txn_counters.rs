//! `txn.counters` (ADR 0161, RT-1427; port of `SiteCommandCounterTests`) on the in-memory
//! fake: the rules. The SQL text is held in `commands_txn_verb_sql`.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::SiteCommandExecutor;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, TxnCounterFloor, TxnCounterState,
    TxnCountersPayload, TxnCountersResult, txn_command_types, txn_counter_names as names,
    txn_reasons,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

fn counters(rvc: i32, id: Option<Uuid>, floors: &[(&str, i64)]) -> SyncCommand {
    let p = TxnCountersPayload {
        rvc_no: rvc,
        counters: floors
            .iter()
            .map(|(name, floor)| TxnCounterFloor {
                name: (*name).to_owned(),
                floor: *floor,
            })
            .collect(),
        actor: "handover".into(),
    };
    SyncCommand {
        command_id: id.unwrap_or_else(Uuid::new_v4),
        type_: txn_command_types::COUNTERS.into(),
        aggregate_key: names::aggregate_key(rvc),
        expected_hash: None,
        payload: stj::parse(&p.to_json()).unwrap(),
        created_by: "cloud".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn run(store: &Arc<FakeStore>, c: &SyncCommand) -> SyncCommandResult {
    SiteCommandExecutor::new(store.clone(), clock(1_795_507_200), &Logger::none())
        .execute(c, &Ctx::background())
        .unwrap()
}

fn body(r: &SyncCommandResult) -> TxnCountersResult {
    TxnCountersResult::from_element(r.result.as_ref().expect("a result"))
        .expect("a counters result")
}

fn key(rvc: i32, name: &str) -> (i32, String) {
    (rvc, name.to_owned())
}

fn stored(store: &FakeStore, rvc: i32, name: &str) -> Option<i64> {
    store
        .state()
        .txn
        .counters
        .stored
        .get(&key(rvc, name))
        .copied()
}

fn counter_calls(store: &FakeStore) -> Vec<String> {
    store
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("counter:"))
        .collect()
}

fn state(name: &str, before: i64, after: i64, in_use: Option<i64>) -> TxnCounterState {
    TxnCounterState {
        name: name.to_owned(),
        before,
        stored: after,
        in_use,
    }
}

#[test]
fn a_floor_above_the_stored_counter_raises_it_and_reports_before_and_after() {
    let store = FakeStore::new();
    {
        let mut s = store.state();
        s.txn.counters.stored.insert(key(4, names::LAST_CHECK), 120);
        s.txn.counters.in_use.insert(key(4, names::LAST_CHECK), 118);
    }

    let result = run(&store, &counters(4, None, &[(names::LAST_CHECK, 500)]));

    assert_eq!(result.status, CommandStatus::Applied);
    assert_eq!(
        body(&result).counters,
        vec![state(names::LAST_CHECK, 120, 500, Some(118))]
    );
    assert_eq!(stored(&store, 4, names::LAST_CHECK), Some(500));
}

#[test]
fn a_floor_below_the_stored_counter_never_lowers_it() {
    let store = FakeStore::new();
    store
        .state()
        .txn
        .counters
        .stored
        .insert(key(4, names::LAST_TRN), 900);

    let result = run(&store, &counters(4, None, &[(names::LAST_TRN, 300)]));

    assert_eq!(
        body(&result).counters,
        vec![state(names::LAST_TRN, 900, 900, None)]
    );
    assert_eq!(stored(&store, 4, names::LAST_TRN), Some(900));
    assert!(
        !counter_calls(&store)
            .iter()
            .any(|c| c.starts_with("counter:raise:"))
    );
}

#[test]
fn a_zero_floor_is_a_read_that_seeds_a_missing_row_at_zero_and_reports_the_high_water() {
    let store = FakeStore::new();
    store
        .state()
        .txn
        .counters
        .in_use
        .insert(key(2, names::CLOSE_NUMBER), 41);

    let result = run(
        &store,
        &counters(2, None, &[(names::CLOSE_NUMBER, 0), (names::LAST_CHECK, 0)]),
    );

    let body = body(&result);
    let order: Vec<&str> = body.counters.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(order, vec![names::LAST_CHECK, names::CLOSE_NUMBER]);
    assert_eq!(body.high_water(names::CLOSE_NUMBER), Some(41));
    assert_eq!(body.high_water(names::LAST_CHECK), Some(0));
    assert_eq!(
        stored(&store, 2, names::LAST_CHECK),
        Some(0),
        "the missing row is seeded"
    );
}

#[test]
fn a_replay_of_the_same_command_returns_the_recorded_result_and_writes_nothing() {
    let store = FakeStore::new();
    let id = Uuid::new_v4();
    run(&store, &counters(4, Some(id), &[(names::LAST_PAYMENT, 70)]));
    // POS took five more numbers meanwhile.
    store
        .state()
        .txn
        .counters
        .stored
        .insert(key(4, names::LAST_PAYMENT), 75);
    let before = counter_calls(&store).len();

    let replay = run(&store, &counters(4, Some(id), &[(names::LAST_PAYMENT, 70)]));

    assert_eq!(body(&replay).counters[0].stored, 70);
    assert_eq!(
        counter_calls(&store).len(),
        before,
        "a replay makes no counter call"
    );
    assert_eq!(stored(&store, 4, names::LAST_PAYMENT), Some(75));
}

#[test]
fn a_counter_kept_in_its_own_object_refuses_the_whole_command_and_writes_nothing() {
    let store = FakeStore::new();
    store
        .state()
        .txn
        .counters
        .objects
        .insert(key(4, names::LAST_TRN));

    let id = Uuid::new_v4();
    let result = run(
        &store,
        &counters(
            4,
            Some(id),
            &[(names::LAST_CHECK, 10), (names::LAST_TRN, 10)],
        ),
    );

    assert_eq!(result.status, CommandStatus::Rejected);
    assert_eq!(
        result.reason.as_deref(),
        Some(txn_reasons::COUNTER_OBJECT_UNSUPPORTED)
    );
    assert!(
        store.state().txn.counters.stored.is_empty(),
        "the LastCheck raise rolled back"
    );
    assert!(!store.logged(id), "a refusal writes no log row");
}

#[test]
fn a_malformed_payload_is_refused_before_any_sql() {
    for (rvc, name, floor) in [
        (0, "LastCheck", 1),
        (4, "Nope", 1),
        (4, "LastCheck", -1),
        (4, "LastCheck", 100_000_000),
    ] {
        let store = FakeStore::new();

        let result = run(&store, &counters(rvc, None, &[(name, floor)]));

        assert_ne!(
            result.status,
            CommandStatus::Applied,
            "{rvc} {name} {floor}"
        );
        assert!(counter_calls(&store).is_empty(), "{rvc} {name} {floor}");
    }
}

#[test]
fn an_aggregate_key_of_another_rvc_is_refused() {
    let store = FakeStore::new();
    let mut c = counters(4, None, &[(names::LAST_CHECK, 1)]);
    c.aggregate_key = names::aggregate_key(5);

    let result = run(&store, &c);

    assert_ne!(result.status, CommandStatus::Applied);
    assert!(counter_calls(&store).is_empty());
}
