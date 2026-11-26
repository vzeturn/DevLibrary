//! `fo.inhouse_search` (RT-1296) through the executor on fakes (port of the C#
//! `FoInHouseSearchCommandTests` and the `SiteCommandContractTests` case): payload rules,
//! wire result, refusal, read-only behaviour, no name in the log.

mod commands_support;

use std::sync::{Arc, Mutex};

use commands_support::{FakeStore, clock, lock};
use dl_agent_pda_sync::commands::{
    FoInHouseRead, FoInHouseSearchCommandHandler, FoInHouseSource, SiteCommandExecutor, reasons,
};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, FoInHouseGuest, FoInHouseSearchPayload, FoInHouseSearchResult,
    SiteCommandDelivery, SyncCommand, site_command_contract, site_command_types as s, txn_reasons,
};
use dl_agent_pda_sync::reporting::{Level, Logger, LoggerConfig, MemorySink};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::Value as Json;
use dl_uuid::Uuid;

#[derive(Default)]
struct FakeSource {
    guests: Mutex<Vec<FoInHouseGuest>>,
    refusal: Mutex<Option<String>>,
    calls: Mutex<Vec<(i32, String, i32)>>,
}

impl FoInHouseSource for FakeSource {
    fn search(
        &self,
        rvc: i32,
        query: &str,
        limit: i32,
        _ctx: &Ctx,
    ) -> dl_agent_pda_sync::commands::Result<FoInHouseRead> {
        lock(&self.calls).push((rvc, query.to_owned(), limit));
        Ok(match lock(&self.refusal).clone() {
            Some(r) => FoInHouseRead::refused(&r),
            None => FoInHouseRead {
                refusal: None,
                guests: lock(&self.guests).clone(),
            },
        })
    }
}

struct T {
    store: Arc<FakeStore>,
    source: Arc<FakeSource>,
    sink: Arc<MemorySink>,
}

impl T {
    fn new() -> T {
        T {
            store: FakeStore::new(),
            source: Arc::new(FakeSource::default()),
            sink: Arc::new(MemorySink::default()),
        }
    }

    fn executor(&self, with_handler: bool) -> SiteCommandExecutor {
        let log = Logger::new(LoggerConfig {
            minimum: Level::Verbose,
            sinks: vec![(self.sink.clone(), Level::Verbose)],
            ..LoggerConfig::default()
        });
        let c = clock(1_795_680_000);
        let e = SiteCommandExecutor::new(self.store.clone(), c.clone(), &Logger::none());
        if with_handler {
            e.with_fo_inhouse(Arc::new(FoInHouseSearchCommandHandler::new(
                self.source.clone(),
                c,
                &log,
            )))
        } else {
            e
        }
    }

    fn lines(&self) -> Vec<String> {
        self.sink
            .events()
            .iter()
            .map(|e| e.render_message())
            .collect()
    }
}

fn payload(rvc: i32, query: &str, limit: i32) -> FoInHouseSearchPayload {
    FoInHouseSearchPayload {
        rvc,
        query: Some(query.to_owned()),
        limit,
    }
}

fn command(p: &FoInHouseSearchPayload) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: s::FO_INHOUSE_SEARCH.to_owned(),
        aggregate_key: FoInHouseSearchPayload::aggregate_key(p.rvc, Uuid::new_v4()),
        expected_hash: None,
        payload: p.to_element(),
        created_by: "spa:inhouse_search".to_owned(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn guest(departure: Option<&str>) -> FoInHouseGuest {
    FoInHouseGuest {
        room_no: "101".to_owned(),
        folio_num: 50012,
        guest_name: "John Smith".to_owned(),
        arrival_date: Some("2026-11-24".to_owned()),
        departure_date: departure.map(str::to_owned),
    }
}

#[test]
fn fo_inhouse_search_is_interactive_validates_the_query_and_round_trips_snake_case() {
    let spec = site_command_contract::find(Some(s::FO_INHOUSE_SEARCH)).unwrap();
    assert_eq!(spec.delivery, SiteCommandDelivery::Interactive);
    assert_eq!(spec.max_attempts, 1);
    assert_eq!(s::FO_INHOUSE_SEARCH, "fo.inhouse_search");
    let id = Uuid::parse("0f8fad5b-d9cb-469f-a165-70867728950e").unwrap();
    assert_eq!(
        FoInHouseSearchPayload::aggregate_key(4, id),
        "fo-inhouse:4:0f8fad5bd9cb469fa16570867728950e"
    );
    assert_eq!(payload(4, "101", 10).problem(), None);
    assert_eq!(payload(4, "x", 1).problem(), None);
    assert_eq!(payload(4, &"a".repeat(40), 20).problem(), None);
    assert!(payload(0, "101", 10).problem().unwrap().contains("rvc"));
    assert!(payload(4, "  ", 10).problem().unwrap().contains("query"));
    let none = FoInHouseSearchPayload {
        rvc: 4,
        query: None,
        limit: 10,
    };
    assert!(none.problem().unwrap().contains("query"));
    assert!(
        payload(4, &"a".repeat(41), 10)
            .problem()
            .unwrap()
            .contains("query")
    );
    assert!(payload(4, "101", 0).problem().unwrap().contains("limit"));
    assert!(payload(4, "101", 21).problem().unwrap().contains("limit"));
    assert_eq!(
        payload(4, "101", 10).to_json(),
        r#"{"rvc":4,"query":"101","limit":10}"#
    );
    let result = FoInHouseSearchResult {
        rvc: 4,
        guests: vec![guest(None)],
    };
    let json = result.to_json();
    assert_eq!(
        json,
        r#"{"rvc":4,"guests":[{"room_no":"101","folio_num":50012,"guest_name":"John Smith","arrival_date":"2026-11-24"}]}"#
    );
    assert_eq!(FoInHouseSearchResult::from_json(&json).unwrap(), result);
}

#[test]
fn a_search_is_applied_with_the_guests_and_no_command_log_row() {
    let t = T::new();
    *lock(&t.source.guests) = vec![guest(Some("2026-11-28"))];
    let cmd = command(&payload(4, " smith ", 10));

    let r = t.executor(true).execute(&cmd, &Ctx::background()).unwrap();

    assert_eq!(r.status, CommandStatus::Applied);
    let body = FoInHouseSearchResult::from_element(r.result.as_ref().unwrap()).unwrap();
    assert_eq!(body.rvc, 4);
    assert_eq!(body.guests, vec![guest(Some("2026-11-28"))]);
    assert_eq!(*lock(&t.source.calls), vec![(4, "smith".to_owned(), 10)]);
    assert!(
        !t.store.logged(cmd.command_id),
        "a read-only command writes no command-log row"
    );
}

#[test]
fn the_log_carries_the_count_never_the_query_or_a_name() {
    let t = T::new();
    *lock(&t.source.guests) = vec![FoInHouseGuest {
        arrival_date: None,
        ..guest(None)
    }];

    t.executor(true)
        .execute(&command(&payload(4, "Smith", 10)), &Ctx::background())
        .unwrap();

    let lines = t.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("1 in-house folio(s)"), "{lines:?}");
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("Smith") || l.contains("101") || l.contains("50012"))
    );
}

#[test]
fn an_invalid_payload_is_rejected_without_reading() {
    let cases = [
        (0, "101".to_owned(), 10),
        (4, " ".to_owned(), 10),
        (
            4,
            "12345678901234567890123456789012345678901".to_owned(),
            10,
        ),
        (4, "101".to_owned(), 0),
        (4, "101".to_owned(), 21),
    ];
    for (rvc, query, limit) in cases {
        let t = T::new();
        let r = t
            .executor(true)
            .execute(&command(&payload(rvc, &query, limit)), &Ctx::background())
            .unwrap();
        assert_eq!(r.status, CommandStatus::Rejected);
        assert!(
            r.reason
                .as_deref()
                .unwrap()
                .starts_with(reasons::INVALID_PAYLOAD),
            "{:?}",
            r.reason
        );
        assert!(lock(&t.source.calls).is_empty());
    }
}

#[test]
fn a_payload_that_is_not_an_object_is_rejected_as_invalid() {
    let t = T::new();
    let mut cmd = command(&payload(4, "101", 10));
    cmd.payload = Json::Null;

    let r = t.executor(true).execute(&cmd, &Ctx::background()).unwrap();

    assert_eq!(
        r.reason.as_deref(),
        Some("invalid_payload: payload is not a fo.inhouse_search object")
    );
    assert!(lock(&t.source.calls).is_empty());
}

#[test]
fn an_unusable_front_office_database_is_rejected_with_the_room_charge_reason() {
    let t = T::new();
    *lock(&t.source.refusal) = Some(txn_reasons::FOLIO_DATABASE_INVALID.to_owned());

    let r = t
        .executor(true)
        .execute(&command(&payload(4, "101", 10)), &Ctx::background())
        .unwrap();

    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some("txn_folio_database_invalid"));
}

#[test]
fn an_agent_without_the_handler_rejects_the_type() {
    let t = T::new();
    let r = t
        .executor(false)
        .execute(&command(&payload(4, "101", 10)), &Ctx::background())
        .unwrap();

    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some(reasons::UNKNOWN_TYPE));
}
