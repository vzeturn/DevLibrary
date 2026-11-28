//! `pos.bill_list` on a hand-written bill source (RT-0842): the bills come back as the
//! Applied result (capped, with `truncated`), an invalid payload is the `invalid_payload`
//! rejection that never reaches the source, a source error propagates, and the command
//! writes nothing to the store. The SELECT texts are pinned below; the .NET agent's SQL
//! Server suite runs the same text against a real SMILE_POS shape.

mod commands_support;

use std::sync::{Arc, Mutex};

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::CommandError;
use dl_agent_pda_sync::commands::{
    POS_BILL_TENDERS, POS_BILLS, PosBillSource, SiteCommandExecutor, pos_bill_parameters,
};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, PosBill, PosBillListPayload, PosBillListResult, PosBillPayment,
    SiteCommandDelivery, SyncCommand, SyncCommandResult, site_command_contract, site_command_types,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

#[derive(Default)]
struct FakeBills {
    count: usize,
    fail: bool,
    asked: Mutex<Vec<(i32, String, usize)>>,
}

impl PosBillSource for FakeBills {
    fn bills(
        &self,
        rvc_no: i32,
        business_date: &str,
        cap: usize,
        _ctx: &Ctx,
    ) -> dl_agent_pda_sync::commands::Result<Vec<PosBill>> {
        self.asked
            .lock()
            .unwrap()
            .push((rvc_no, business_date.into(), cap));
        if self.fail {
            return Err(CommandError::invalid("cancelled"));
        }
        Ok((1..=self.count.min(cap))
            .map(|i| bill(i as i64, "270000"))
            .collect())
    }
}

fn bill(check_no: i64, total: &str) -> PosBill {
    PosBill {
        check_no,
        status: "closed".into(),
        total: total.parse().unwrap(),
        payments: vec![PosBillPayment {
            method: "CASH".into(),
            amount: total.parse().unwrap(),
        }],
    }
}

fn command(payload: &str) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::POS_BILL_LIST.into(),
        aggregate_key: "pos-bills:4".into(),
        expected_hash: None,
        payload: stj::parse(payload).unwrap(),
        created_by: "office-1".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn run(
    store: &Arc<FakeStore>,
    source: Option<Arc<FakeBills>>,
    c: &SyncCommand,
) -> Result<SyncCommandResult, CommandError> {
    let mut e = SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none());
    if let Some(s) = source {
        e = e.with_bills(s);
    }
    e.execute(c, &Ctx::background())
}

const OK: &str = r#"{"rvc_no":4,"business_date":"2026-11-20","reason":"bill reconciliation"}"#;

#[test]
fn applies_with_the_bills_and_writes_nothing() {
    let store = FakeStore::new();
    let source = Arc::new(FakeBills {
        count: 2,
        ..Default::default()
    });
    let c = command(OK);
    let r = run(&store, Some(source.clone()), &c).unwrap();
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    let out = PosBillListResult::from_element(r.result.as_ref().unwrap()).unwrap();
    assert_eq!((out.rvc_no, out.business_date.as_str()), (4, "2026-11-20"));
    assert_eq!(out.bills, vec![bill(1, "270000"), bill(2, "270000")]);
    assert!(!out.truncated);
    assert_eq!(
        source.asked.lock().unwrap().as_slice(),
        [(
            4,
            "2026-11-20".to_owned(),
            PosBillListPayload::MAX_BILLS + 1
        )]
    );
    assert_eq!(store.runs(), 0);
    assert!(!store.logged(c.command_id));
}

#[test]
fn a_day_above_the_cap_answers_the_first_bills_and_says_truncated() {
    let source = Arc::new(FakeBills {
        count: PosBillListPayload::MAX_BILLS + 5,
        ..Default::default()
    });
    let r = run(&FakeStore::new(), Some(source), &command(OK)).unwrap();
    let out = PosBillListResult::from_element(r.result.as_ref().unwrap()).unwrap();
    assert_eq!(out.bills.len(), PosBillListPayload::MAX_BILLS);
    assert!(out.truncated);
}

#[test]
fn an_invalid_payload_is_rejected_without_reading() {
    let store = FakeStore::new();
    let source = Arc::new(FakeBills::default());
    for (payload, why) in [
        (
            r#"{"rvc_no":0,"business_date":"2026-11-20"}"#,
            "rvc_no must be positive",
        ),
        (
            r#"{"rvc_no":4,"business_date":"20/11/2026"}"#,
            "business_date must be yyyy-MM-dd",
        ),
        (
            r#"{"rvc_no":4,"business_date":"2026-02-30"}"#,
            "business_date must be yyyy-MM-dd",
        ),
        (r#"[1]"#, "payload is not a pos.bill_list object"),
    ] {
        let r = run(&store, Some(source.clone()), &command(payload)).unwrap();
        assert_eq!(r.status, CommandStatus::Rejected, "{payload}");
        assert_eq!(
            r.reason.as_deref(),
            Some(format!("invalid_payload: {why}").as_str())
        );
    }
    assert!(source.asked.lock().unwrap().is_empty());
}

#[test]
fn a_source_error_propagates_and_nothing_is_logged() {
    let store = FakeStore::new();
    let source = Arc::new(FakeBills {
        fail: true,
        ..Default::default()
    });
    let c = command(OK);
    assert!(run(&store, Some(source), &c).is_err());
    assert!(!store.logged(c.command_id));
}

#[test]
fn without_a_source_the_type_is_unknown() {
    let r = run(&FakeStore::new(), None, &command(OK)).unwrap();
    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some("unknown_command_type"));
}

#[test]
fn the_contract_is_durable_and_matches_the_dotnet_wire_shape() {
    assert_eq!(site_command_types::POS_BILL_LIST, "pos.bill_list");
    assert_eq!(
        site_command_contract::find(Some(site_command_types::POS_BILL_LIST))
            .map(|s| (s.delivery, s.max_attempts)),
        Some((SiteCommandDelivery::Durable, 3))
    );
    assert_eq!(PosBillListPayload::aggregate_key(4), "pos-bills:4");
    let p = PosBillListPayload {
        rvc_no: 4,
        business_date: "2026-11-20".into(),
        reason: None,
    };
    assert_eq!(p.to_json(), r#"{"rvc_no":4,"business_date":"2026-11-20"}"#);
    let result = PosBillListResult {
        rvc_no: 4,
        business_date: "2026-11-20".into(),
        bills: vec![bill(11, "270")],
        truncated: false,
    };
    let raw = r#"{"rvc_no":4,"business_date":"2026-11-20","bills":[{"check_no":11,"status":"closed","total":270,"payments":[{"method":"CASH","amount":270}]}],"truncated":false}"#;
    assert_eq!(result.to_json(), raw);
    assert_eq!(
        PosBillListResult::from_element(&stj::parse(raw).unwrap()).unwrap(),
        result
    );
}

#[test]
fn the_selects_are_read_only_parameterized_and_select_no_guest_column() {
    for text in [POS_BILLS, POS_BILL_TENDERS] {
        let upper = text.to_uppercase();
        for forbidden in [
            "INSERT", "UPDATE", "DELETE", "MERGE", "EXEC", "DROP", "INTO",
        ] {
            assert!(!upper.contains(forbidden), "{forbidden}");
        }
        for name in ["Rvc", "Year", "Month", "Day"] {
            assert!(text.contains(&format!("@{name}")), "{name}");
        }
        for col in [
            "GuestName",
            "CardNumber",
            "OpenBy",
            "Cashier",
            "TableNo",
            "Phone",
        ] {
            assert!(!text.contains(col), "{col}");
        }
    }
    assert!(POS_BILLS.contains("@Cap"));
    assert_eq!(pos_bill_parameters(4, "2026-11-20", Some(2001)).len(), 5);
    assert_eq!(pos_bill_parameters(4, "2026-11-20", None).len(), 4);
}
