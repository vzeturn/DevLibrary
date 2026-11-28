//! `pos.revenue_summary` on a hand-written revenue source (ADR 0149, RT-0843): the day
//! rows come back as the Applied result, an invalid payload is the `invalid_payload`
//! rejection that never reaches the source, a source error propagates (nothing logged,
//! nothing lost), and the command writes nothing to the store. The real SELECT against
//! SQL Server is pinned by the text tests below and by the gated SQL suite.

mod commands_support;

use std::sync::{Arc, Mutex};

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::CommandError;
use dl_agent_pda_sync::commands::{
    DAILY_REVENUE, PosRevenueSource, SiteCommandExecutor, daily_revenue_parameters,
};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, PosRevenueDay, PosRevenueSummaryResult, SyncCommand, SyncCommandResult,
    site_command_types,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

#[derive(Default)]
struct FakeRevenue {
    days: Vec<PosRevenueDay>,
    fail: bool,
    asked: Mutex<Vec<(i32, String, String)>>,
}

impl PosRevenueSource for FakeRevenue {
    fn daily(
        &self,
        rvc_no: i32,
        date_from: &str,
        date_to: &str,
        _ctx: &Ctx,
    ) -> dl_agent_pda_sync::commands::Result<Vec<PosRevenueDay>> {
        self.asked
            .lock()
            .unwrap()
            .push((rvc_no, date_from.into(), date_to.into()));
        if self.fail {
            return Err(CommandError::invalid("cancelled"));
        }
        Ok(self
            .days
            .iter()
            .filter(|d| d.date.as_str() >= date_from && d.date.as_str() <= date_to)
            .cloned()
            .collect())
    }
}

fn day(date: &str, bills: i32, gross: &str, net: &str, paid: &str) -> PosRevenueDay {
    PosRevenueDay {
        date: date.into(),
        bills,
        gross: gross.parse().unwrap(),
        net: net.parse().unwrap(),
        paid: paid.parse().unwrap(),
    }
}

fn command(payload: &str) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::POS_REVENUE_SUMMARY.into(),
        aggregate_key: "pos-revenue:3".into(),
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
    source: Option<Arc<FakeRevenue>>,
    c: &SyncCommand,
) -> Result<SyncCommandResult, CommandError> {
    let mut e = SiteCommandExecutor::new(store.clone(), clock(1_794_729_600), &Logger::none());
    if let Some(s) = source {
        e = e.with_revenue(s);
    }
    e.execute(c, &Ctx::background())
}

const OK: &str =
    r#"{"rvc_no":3,"date_from":"2026-11-01","date_to":"2026-11-02","reason":"acceptance"}"#;

#[test]
fn applies_with_the_per_day_sums_and_writes_nothing() {
    let store = FakeStore::new();
    let source = Arc::new(FakeRevenue {
        days: vec![
            day("2026-10-31", 9, "9", "9", "9"),
            day("2026-11-01", 2, "300000.00", "270000.00", "270000.00"),
            day("2026-11-02", 1, "50000", "50000", "60000"),
        ],
        ..Default::default()
    });
    let c = command(OK);
    let r = run(&store, Some(source.clone()), &c).unwrap();
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    assert_eq!(r.command_id, c.command_id);
    let out = PosRevenueSummaryResult::from_element(r.result.as_ref().unwrap()).unwrap();
    assert_eq!((out.rvc_no, out.date_from.as_str()), (3, "2026-11-01"));
    assert_eq!(out.days.len(), 2);
    assert_eq!(
        out.days[0],
        day("2026-11-01", 2, "300000.00", "270000.00", "270000.00")
    );
    assert_eq!(
        source.asked.lock().unwrap().as_slice(),
        [(3, "2026-11-01".to_owned(), "2026-11-02".to_owned())]
    );
    // Read-only: no write transaction, no command-log row.
    assert_eq!(store.runs(), 0);
    assert!(!store.logged(c.command_id));
}

#[test]
fn a_redelivery_reads_again_and_answers_the_same_sums() {
    let store = FakeStore::new();
    let source = Arc::new(FakeRevenue {
        days: vec![day("2026-11-01", 1, "10", "10", "10")],
        ..Default::default()
    });
    let c = command(OK);
    let a = run(&store, Some(source.clone()), &c).unwrap();
    let b = run(&store, Some(source.clone()), &c).unwrap();
    assert_eq!(a.result, b.result);
    assert_eq!(source.asked.lock().unwrap().len(), 2);
}

#[test]
fn an_empty_range_is_applied_with_no_days() {
    let store = FakeStore::new();
    let r = run(&store, Some(Arc::new(FakeRevenue::default())), &command(OK)).unwrap();
    assert_eq!(r.status, CommandStatus::Applied);
    let out = PosRevenueSummaryResult::from_element(r.result.as_ref().unwrap()).unwrap();
    assert!(out.days.is_empty());
}

#[test]
fn an_invalid_payload_is_rejected_without_reading() {
    let store = FakeStore::new();
    let source = Arc::new(FakeRevenue::default());
    for (payload, why) in [
        (
            r#"{"rvc_no":0,"date_from":"2026-11-01","date_to":"2026-11-02"}"#,
            "rvc_no must be positive",
        ),
        (
            r#"{"rvc_no":3,"date_from":"01/11/2026","date_to":"2026-11-02"}"#,
            "date_from must be yyyy-MM-dd",
        ),
        (
            r#"{"rvc_no":3,"date_from":"2026-11-03","date_to":"2026-11-02"}"#,
            "date_from must not be after date_to",
        ),
        (
            r#"{"rvc_no":3,"date_from":"2026-01-01","date_to":"2026-12-31"}"#,
            "the range must not exceed 31 days",
        ),
        (r#"[1]"#, "payload is not a pos.revenue_summary object"),
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
    let source = Arc::new(FakeRevenue {
        fail: true,
        ..Default::default()
    });
    let c = command(OK);
    assert!(run(&store, Some(source), &c).is_err());
    assert!(
        !store.logged(c.command_id),
        "a failed read leaves no log row"
    );
}

#[test]
fn without_a_source_the_type_is_unknown() {
    let r = run(&FakeStore::new(), None, &command(OK)).unwrap();
    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some("unknown_command_type"));
}

#[test]
fn the_select_is_read_only_parameterized_and_binds_every_variable() {
    let sql = DAILY_REVENUE.to_uppercase();
    for forbidden in ["INSERT", "UPDATE", "DELETE", "MERGE", "EXEC", "DROP"] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    let params = daily_revenue_parameters(3, "2026-11-01", "2026-11-02");
    for name in ["Rvc", "From", "To"] {
        assert!(DAILY_REVENUE.contains(&format!("@{name}")), "{name}");
    }
    assert_eq!(params.len(), 3);
    // No names or bill numbers in the select list.
    for col in ["CheckNo", "GuestName", "CardNumber", "OpenBy"] {
        assert!(!DAILY_REVENUE.contains(col), "{col}");
    }
}
