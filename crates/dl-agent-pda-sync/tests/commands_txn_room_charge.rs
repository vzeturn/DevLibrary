//! `txn.room_charge` / `txn.room_charge_void` (ADR 0162, RT-1427; port of
//! `SiteCommandRoomChargeTests`) on the in-memory fake: the rules. The SQL text is held in
//! `commands_txn_verb_sql`.

mod commands_support;

use std::sync::Arc;

use commands_support::{FakeFolioRow, FakeStore, clock};
use dl_agent_pda_sync::commands::{FoFolio, SiteCommandExecutor, params};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncCommand, SyncCommandResult, TxnRoomChargePayload, TxnRoomChargeResult,
    TxnRoomChargeVoidPayload, txn_command_types, txn_outcomes, txn_reasons, txn_room_charge_scope,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_decimal::Decimal;
use dl_uuid::Uuid;

const CHECK: i64 = 70001;

fn store() -> Arc<FakeStore> {
    let store = FakeStore::new();
    {
        let mut s = store.state();
        // ROOM CHARGE tender -> folio code 12 / 03.
        s.txn.fo.links.insert((9, 1), 1203);
        s.txn.fo.folios.push(folio(5001, "0501"));
    }
    store
}

fn folio(folio_num: i64, room: &str) -> FoFolio {
    FoFolio {
        folio_num,
        room_code: room.into(),
    }
}

fn command(type_: &str, json: &str, id: Option<Uuid>, check: i64) -> SyncCommand {
    SyncCommand {
        command_id: id.unwrap_or_else(Uuid::new_v4),
        type_: type_.into(),
        aggregate_key: txn_room_charge_scope::aggregate_key(check),
        expected_hash: None,
        payload: stj::parse(json).unwrap(),
        created_by: "cloud".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

struct Post {
    version: i64,
    amount: Decimal,
    room: &'static str,
    folio: i64,
    id: Option<Uuid>,
}

fn post(version: i64) -> Post {
    Post {
        version,
        amount: Decimal::from(250_000),
        room: "0501",
        folio: 0,
        id: None,
    }
}

impl Post {
    fn cmd(&self) -> SyncCommand {
        let p = TxnRoomChargePayload {
            rvc: 4,
            check_no: CHECK,
            version: self.version,
            room_no: self.room.into(),
            folio_num: self.folio,
            trn_code: 9,
            trn_sub: 1,
            amount: self.amount,
            meal_period: 0,
            comment: None,
            cashier: "AN".into(),
            at: "2026-11-24T10:15:00".into(),
            actor: "cloud".into(),
        };
        command(txn_command_types::ROOM_CHARGE, &p.to_json(), self.id, CHECK)
    }
}

fn void(version: i64) -> SyncCommand {
    let p = TxnRoomChargeVoidPayload {
        rvc: 4,
        check_no: CHECK,
        version,
        reason: "bill voided".into(),
        actor: "cloud".into(),
    };
    command(
        txn_command_types::ROOM_CHARGE_VOID,
        &p.to_json(),
        None,
        CHECK,
    )
}

fn run(store: &Arc<FakeStore>, c: &SyncCommand) -> SyncCommandResult {
    SiteCommandExecutor::new(store.clone(), clock(1_795_507_200), &Logger::none())
        .execute(c, &Ctx::background())
        .unwrap()
}

fn body(r: &SyncCommandResult) -> TxnRoomChargeResult {
    TxnRoomChargeResult::from_element(r.result.as_ref().expect("a result"))
        .expect("a room charge result")
}

fn active(store: &FakeStore) -> Vec<FakeFolioRow> {
    store.state().txn.fo.active()
}

fn rows(store: &FakeStore) -> Vec<FakeFolioRow> {
    store.state().txn.fo.rows.clone()
}

#[test]
fn a_post_lands_one_active_row_with_the_mapped_folio_code() {
    let store = store();

    let result = run(&store, &post(1).cmd());

    assert_eq!(result.status, CommandStatus::Applied);
    let active = active(&store);
    assert_eq!(active.len(), 1);
    let row = &active[0];
    assert_eq!(
        (
            row.folio,
            row.amount,
            row.reference.as_str(),
            row.trn_code,
            row.trn_sub,
            row.clerk.as_str()
        ),
        (5001, Decimal::from(250_000), "70001", 12, 3, "$AM")
    );
    assert_eq!(row.comment, "Cloud bill 70001");
    let body = body(&result);
    assert_eq!(
        (
            body.folio_num,
            body.active_rows,
            body.active_amount,
            body.outcome.as_str()
        ),
        (5001, 1, Decimal::from(250_000), txn_outcomes::APPLIED)
    );
    assert_eq!(
        store.state().txn.store_keys.get("room_charge:70001"),
        Some(&5001)
    );
}

#[test]
fn a_replay_of_the_same_command_posts_nothing_more() {
    let store = store();
    let id = Uuid::new_v4();
    let once = Post {
        id: Some(id),
        ..post(1)
    };

    run(&store, &once.cmd());
    run(&store, &once.cmd());

    assert_eq!(store.state().txn.fo.posts, 1);
    assert_eq!(active(&store).len(), 1);
}

#[test]
fn an_older_or_equal_version_is_stale_and_changes_nothing() {
    let store = store();
    run(&store, &post(3).cmd());

    let result = run(
        &store,
        &Post {
            amount: Decimal::from(999),
            ..post(3)
        }
        .cmd(),
    );

    let body = body(&result);
    assert_eq!(body.outcome, txn_outcomes::STALE);
    assert_eq!(body.applied_version, 3);
    assert_eq!(
        (body.folio_num, body.active_rows, body.active_amount),
        (5001, 1, Decimal::from(250_000))
    );
    assert_eq!(store.state().txn.fo.posts, 1);
    assert_eq!(active(&store)[0].amount, Decimal::from(250_000));
}

#[test]
fn a_newer_version_reverses_the_earlier_posting_and_posts_the_new_state_once() {
    let store = store();
    run(&store, &post(1).cmd());

    let result = run(
        &store,
        &Post {
            amount: Decimal::from(300_000),
            ..post(2)
        }
        .cmd(),
    );

    assert_eq!(body(&result).active_amount, Decimal::from(300_000));
    let active = active(&store);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].amount, Decimal::from(300_000));
    let voided: Decimal = rows(&store)
        .iter()
        .filter(|r| r.code == 'V')
        .map(|r| r.amount)
        .sum();
    assert_eq!(
        voided,
        Decimal::ZERO,
        "the voided row and its mirror cancel"
    );
    assert!(
        rows(&store)
            .iter()
            .any(|r| r.comment == "Cloud bill 70001 re-posted")
    );
}

#[test]
fn a_newer_version_on_another_folio_reverses_the_earlier_folio() {
    let store = store();
    run(&store, &post(1).cmd());
    store.state().txn.fo.folios.push(folio(5002, "0502"));

    let result = run(
        &store,
        &Post {
            room: "0502",
            ..post(2)
        }
        .cmd(),
    );

    assert_eq!(result.status, CommandStatus::Applied);
    let active = active(&store);
    assert_eq!(active.len(), 1, "only the new posting stays active");
    assert_eq!(active[0].folio, 5002);
}

#[test]
fn a_void_reverses_every_active_row_and_an_older_post_can_not_undo_it() {
    let store = store();
    run(&store, &post(1).cmd());

    let voided = run(&store, &void(2));
    let late = run(
        &store,
        &Post {
            amount: Decimal::from(1),
            ..post(1)
        }
        .cmd(),
    );

    assert_eq!(voided.status, CommandStatus::Applied);
    let v = body(&voided);
    assert_eq!(
        (v.folio_num, v.active_rows, v.active_amount),
        (5001, 0, Decimal::ZERO)
    );
    assert!(
        rows(&store)
            .iter()
            .any(|r| r.comment == "Void cloud bill 70001: bill voided")
    );
    assert!(active(&store).is_empty());
    assert_eq!(body(&late).outcome, txn_outcomes::STALE);
    assert!(active(&store).is_empty());
}

#[test]
fn a_void_before_any_post_is_recorded_as_a_no_op() {
    let store = store();

    let voided = run(&store, &void(2));
    let late = run(&store, &post(1).cmd());

    assert_eq!(voided.status, CommandStatus::Applied);
    assert_eq!(body(&late).outcome, txn_outcomes::STALE);
    assert!(rows(&store).is_empty());
}

#[test]
fn a_folio_that_is_not_postable_is_refused_and_nothing_is_written() {
    // No in-house folio of that room; the given folio is not in house.
    for (room, folio) in [("0999", 0), ("0501", 7777)] {
        let store = store();
        let c = Post {
            room,
            folio,
            ..post(1)
        }
        .cmd();

        let result = run(&store, &c);

        assert_eq!(result.status, CommandStatus::Rejected, "{room} {folio}");
        assert_eq!(
            result.reason.as_deref(),
            Some(txn_reasons::FOLIO_UNAVAILABLE)
        );
        assert!(rows(&store).is_empty());
        assert!(!store.logged(c.command_id), "a refusal writes no log row");
    }
}

#[test]
fn two_in_house_folios_of_the_room_are_ambiguous_unless_the_folio_is_named() {
    let store = store();
    store.state().txn.fo.folios.push(folio(5002, "0501"));

    let ambiguous = run(&store, &post(1).cmd());
    let named = run(
        &store,
        &Post {
            folio: 5002,
            ..post(1)
        }
        .cmd(),
    );

    assert_eq!(
        ambiguous.reason.as_deref(),
        Some(txn_reasons::FOLIO_AMBIGUOUS)
    );
    assert_eq!(named.status, CommandStatus::Applied);
    let active = active(&store);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].folio, 5002);
}

#[test]
fn a_named_folio_of_another_room_is_unavailable() {
    let store = store();
    store.state().txn.fo.folios.push(folio(5002, "0502"));

    let result = run(
        &store,
        &Post {
            folio: 5002,
            ..post(1)
        }
        .cmd(),
    );

    assert_eq!(
        result.reason.as_deref(),
        Some(txn_reasons::FOLIO_UNAVAILABLE)
    );
}

#[test]
fn an_unmapped_tender_and_an_unusable_front_office_database_are_refused() {
    let store = store();
    store.state().txn.fo.links.clear();
    assert_eq!(
        run(&store, &post(1).cmd()).reason.as_deref(),
        Some(txn_reasons::FOLIO_LINK_UNKNOWN)
    );

    store.state().txn.fo.database = None;
    assert_eq!(
        run(&store, &post(1).cmd()).reason.as_deref(),
        Some(txn_reasons::FOLIO_DATABASE_INVALID)
    );
    assert!(rows(&store).is_empty());
}

#[test]
fn a_link_below_one_hundred_is_unknown() {
    let store = store();
    store.state().txn.fo.links.insert((9, 1), 99);

    assert_eq!(
        run(&store, &post(1).cmd()).reason.as_deref(),
        Some(txn_reasons::FOLIO_LINK_UNKNOWN)
    );
}

#[test]
fn the_front_office_database_is_checked_by_access_not_by_db_id() {
    // G-03: DB_ID is NULL for a usable database the login does not own when VIEW ANY
    // DATABASE is revoked.
    assert!(params::DATABASE_ACCESS_SQL.contains("HAS_DBACCESS(@Name)"));
    assert!(!params::DATABASE_ACCESS_SQL.contains("DB_ID"));
}

#[test]
fn a_malformed_payload_or_a_foreign_aggregate_key_is_invalid() {
    let store = store();
    assert_eq!(
        run(
            &store,
            &Post {
                amount: Decimal::ZERO,
                ..post(1)
            }
            .cmd()
        )
        .status,
        CommandStatus::Rejected
    );
    let mut foreign = post(1).cmd();
    foreign.aggregate_key = txn_room_charge_scope::aggregate_key(CHECK + 1);
    assert_eq!(run(&store, &foreign).status, CommandStatus::Rejected);
    assert!(rows(&store).is_empty());
}
