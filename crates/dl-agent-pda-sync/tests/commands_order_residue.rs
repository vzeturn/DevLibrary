//! The order residue (RT-1345, ADR 0182) and `pos.order_residue` (RT-1365, ADR 0183) on a
//! hand-written source: the reconcile only ever lists, and the cleanup only ever touches,
//! rows of checks this agent's commands named; the command is the read half, never cleans and
//! writes no command-log row. Port of the .NET `OrderResidueTests` and
//! `PosOrderResidueCommandTests`; the SQL text is pinned in the crate's unit tests.

mod commands_support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use commands_support::{FakeStore, clock};
use dl_agent_pda_sync::commands::order_residue::{
    DEFAULT_MIN_AGE_MINUTES, check_no_of, describe, run,
};
use dl_agent_pda_sync::commands::{
    OrderResidueSnapshot, OrderResidueSource, ResidueCheck, ResidueCleaned, ResidueInfoRow,
    SiteCommandExecutor, reasons,
};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::time::{DateTime, DateTimeOffset};
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, PosOrderResiduePayload, PosOrderResidueResult, SyncCommand, SyncCommandResult,
    site_command_types,
};
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_agent_pda_sync::util::stj;
use dl_uuid::Uuid;

const OURS: i64 = 400000101;
const POS_WIN: i64 = 400000202;

fn opened() -> DateTime {
    DateTime::new(2026, 11, 26, 8, 0, 0).unwrap()
}

fn chk(check_no: i64) -> ResidueCheck {
    ResidueCheck {
        rvc_no: 4,
        check_no,
        opened_local: opened(),
    }
}

fn info(check_no: i64, trn_no: i64, sent: bool) -> ResidueInfoRow {
    ResidueInfoRow {
        check_no,
        trn_no,
        sent,
    }
}

struct FakeSource {
    all: OrderResidueSnapshot,
    mine: HashSet<i64>,
    cleaned: Mutex<Vec<OrderResidueSnapshot>>,
    min_age: Mutex<Option<i32>>,
}

impl FakeSource {
    fn new(all: OrderResidueSnapshot, mine: &[i64]) -> FakeSource {
        FakeSource {
            all,
            mine: mine.iter().copied().collect(),
            cleaned: Mutex::new(Vec::new()),
            min_age: Mutex::new(None),
        }
    }
}

impl OrderResidueSource for FakeSource {
    fn our_check_nos(&self, _ctx: &Ctx) -> dl_agent_pda_sync::commands::Result<HashSet<i64>> {
        Ok(self.mine.clone())
    }

    fn read(
        &self,
        min_age_minutes: i32,
        _ctx: &Ctx,
    ) -> dl_agent_pda_sync::commands::Result<OrderResidueSnapshot> {
        *self.min_age.lock().unwrap() = Some(min_age_minutes);
        Ok(self.all.clone())
    }

    fn clean(
        &self,
        ours: &OrderResidueSnapshot,
        _cleaned_by: &str,
        _min_age_minutes: i32,
        _ctx: &Ctx,
    ) -> dl_agent_pda_sync::commands::Result<ResidueCleaned> {
        self.cleaned.lock().unwrap().push(ours.clone());
        let n = |len: usize| i32::try_from(len).unwrap();
        Ok(ResidueCleaned {
            empty_checks: n(ours.empty_open_checks.len()),
            info_rows: n(ours.orphan_info_rows.len()),
            open_logs: n(ours.stale_open_logs.len()),
        })
    }
}

fn snapshot() -> OrderResidueSnapshot {
    OrderResidueSnapshot {
        empty_open_checks: vec![chk(OURS), chk(POS_WIN)],
        orphan_info_rows: vec![
            info(OURS, 1, false),
            info(OURS, 2, true),
            info(POS_WIN, 3, false),
        ],
        stale_open_logs: vec![chk(OURS), chk(POS_WIN)],
        orphan_send_audits: vec![OURS, POS_WIN],
    }
}

fn run_on(
    source: &FakeSource,
    apply: bool,
    min_age: i32,
) -> (dl_agent_pda_sync::commands::ResidueReport, Vec<String>) {
    let mut audit = Vec::new();
    let report = run(
        source,
        apply,
        "tester",
        min_age,
        &mut |l| {
            audit.push(l.to_owned());
            Ok(())
        },
        &Ctx::background(),
    )
    .unwrap();
    (report, audit)
}

#[test]
fn report_is_read_only_and_lists_only_our_checks() {
    let source = FakeSource::new(snapshot(), &[OURS]);

    let (report, audit) = run_on(&source, false, 90);

    assert!(source.cleaned.lock().unwrap().is_empty());
    assert!(audit.is_empty());
    assert_eq!(*source.min_age.lock().unwrap(), Some(90));
    let checks: Vec<i64> = report
        .ours
        .empty_open_checks
        .iter()
        .map(|c| c.check_no)
        .collect();
    assert_eq!(checks, [OURS]);
    assert_eq!(report.ours.orphan_send_audits, [OURS]);
    assert_eq!(report.sent_orphan_info_rows, 1);
    assert_eq!(report.other_rows_ignored, 4);
    assert_eq!(report.cleaned, None);
    let text = describe(&report);
    assert!(text.contains("empty open checks"), "{text}");
    assert!(!text.contains(&POS_WIN.to_string()), "{text}");
    assert!(
        text.contains("empty check rvc=4 no=400000101 opened=2026-11-26T08:00:00"),
        "{text}"
    );
}

#[test]
fn apply_cleans_our_never_sent_rows_only_and_audits_each_action() {
    let source = FakeSource::new(snapshot(), &[OURS]);

    let (report, audit) = run_on(&source, true, 120);

    let cleaned = source.cleaned.lock().unwrap();
    assert_eq!(cleaned.len(), 1);
    assert_eq!(cleaned[0].empty_open_checks, [chk(OURS)]);
    assert_eq!(cleaned[0].orphan_info_rows, [info(OURS, 1, false)]);
    assert_eq!(cleaned[0].stale_open_logs, [chk(OURS)]);
    assert_eq!(
        report.cleaned,
        Some(ResidueCleaned {
            empty_checks: 1,
            info_rows: 1,
            open_logs: 1
        })
    );
    assert_eq!(
        audit,
        [
            "order-residue apply by=tester emptyChecks=1 infoRows=1 openLogs=1",
            "cancel-empty rvc=4 check=400000101",
            "drop-info check=400000101 trn=1",
            "close-openlog rvc=4 check=400000101",
            "order-residue done emptyChecks=1 infoRows=1 openLogs=1",
        ]
    );
    assert!(!audit.iter().any(|l| l.contains(&POS_WIN.to_string())));
    assert!(!audit.iter().any(|l| l.contains("trn=2")));
    assert!(describe(&report).ends_with("Cleaned: 1 empty checks, 1 info rows, 1 open-log rows."));
}

#[test]
fn an_audit_failure_stops_before_anything_is_cleaned() {
    let source = FakeSource::new(snapshot(), &[OURS]);
    let r = run(
        &source,
        true,
        "tester",
        DEFAULT_MIN_AGE_MINUTES,
        &mut |_| {
            Err(dl_agent_pda_sync::commands::CommandError::invalid(
                "disk full",
            ))
        },
        &Ctx::background(),
    );
    assert!(r.is_err());
    assert!(source.cleaned.lock().unwrap().is_empty());
}

#[test]
fn check_no_of_reads_the_check_of_an_applied_result_and_skips_everything_else() {
    let applied = SyncCommandResult {
        command_id: Uuid::new_v4(),
        status: CommandStatus::Applied,
        reason: None,
        result: Some(stj::parse(r#"{"checkNo":400000101,"lines":[]}"#).unwrap()),
        at_utc: DateTimeOffset::UNIX_EPOCH,
        exec_ms: None,
        hops: Default::default(),
    };
    let rejected = SyncCommandResult {
        status: CommandStatus::Rejected,
        reason: Some("check_not_found".into()),
        ..applied.clone()
    };
    let empty = SyncCommandResult {
        result: None,
        ..applied.clone()
    };
    let other_case = SyncCommandResult {
        result: Some(stj::parse(r#"{"CHECKNO":400000101}"#).unwrap()),
        ..applied.clone()
    };

    assert_eq!(check_no_of(&applied.to_json()), Some(OURS));
    assert_eq!(check_no_of(&other_case.to_json()), Some(OURS));
    assert_eq!(check_no_of(&rejected.to_json()), None);
    assert_eq!(check_no_of(&empty.to_json()), None);
    assert_eq!(check_no_of("not json"), None);
}

// pos.order_residue (PosOrderResidueCommandTests).

fn command_source() -> Arc<FakeSource> {
    Arc::new(FakeSource::new(
        OrderResidueSnapshot {
            empty_open_checks: vec![chk(OURS), chk(POS_WIN)],
            orphan_info_rows: vec![info(OURS, 1, false), info(OURS, 2, true)],
            stale_open_logs: vec![chk(POS_WIN)],
            orphan_send_audits: vec![OURS],
        },
        &[OURS],
    ))
}

fn command(payload: &str) -> SyncCommand {
    SyncCommand {
        command_id: Uuid::new_v4(),
        type_: site_command_types::POS_ORDER_RESIDUE.into(),
        aggregate_key: PosOrderResiduePayload::AGGREGATE.into(),
        expected_hash: None,
        payload: stj::parse(payload).unwrap(),
        created_by: "platform".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

fn execute(
    store: &Arc<FakeStore>,
    source: Option<Arc<FakeSource>>,
    c: &SyncCommand,
) -> SyncCommandResult {
    let mut e = SiteCommandExecutor::new(store.clone(), clock(1_795_766_400), &Logger::none());
    if let Some(s) = source {
        e = e.with_residue(s);
    }
    e.execute(c, &Ctx::background()).unwrap()
}

#[test]
fn answers_only_our_rows_counts_the_rest_and_never_cleans() {
    let store = FakeStore::new();
    let source = command_source();
    let c = command(r#"{"min_age_minutes":90,"reason":"acceptance"}"#);

    let r = execute(&store, Some(source.clone()), &c);

    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    let body = PosOrderResidueResult::from_value(r.result.as_ref().unwrap()).unwrap();
    assert_eq!(body.min_age_minutes, 90);
    let checks: Vec<i64> = body.empty_open_checks.iter().map(|c| c.check_no).collect();
    assert_eq!(checks, [OURS]);
    assert_eq!(
        body.empty_open_checks[0].opened_local,
        "2026-11-26T08:00:00"
    );
    assert_eq!(body.orphan_info_rows.len(), 2);
    assert_eq!(body.sent_orphan_info_rows, 1);
    assert!(body.stale_open_logs.is_empty());
    assert_eq!(body.orphan_send_audits, [OURS]);
    assert_eq!(body.other_rows_ignored, 2);
    assert!(!body.truncated);
    assert_eq!(*source.min_age.lock().unwrap(), Some(90));
    assert!(source.cleaned.lock().unwrap().is_empty());
    assert_eq!(store.runs(), 0);
    assert!(
        !store.logged(c.command_id),
        "a read-only command writes no command-log row"
    );
}

#[test]
fn a_list_above_the_cap_is_cut_and_says_truncated() {
    let many: Vec<i64> = (0..=PosOrderResiduePayload::MAX_ROWS as i64)
        .map(|i| OURS + i)
        .collect();
    let source = Arc::new(FakeSource::new(
        OrderResidueSnapshot {
            orphan_send_audits: many.clone(),
            ..Default::default()
        },
        &many,
    ));
    let r = execute(
        &FakeStore::new(),
        Some(source),
        &command(r#"{"min_age_minutes":120}"#),
    );
    let body = PosOrderResidueResult::from_value(r.result.as_ref().unwrap()).unwrap();
    assert_eq!(
        body.orphan_send_audits.len(),
        PosOrderResiduePayload::MAX_ROWS
    );
    assert!(body.truncated);
}

#[test]
fn a_bad_payload_is_rejected_without_reading() {
    let source = command_source();
    let r = execute(
        &FakeStore::new(),
        Some(source.clone()),
        &command(r#"{"min_age_minutes":0,"reason":null}"#),
    );
    assert_eq!(r.status, CommandStatus::Rejected);
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .starts_with(reasons::INVALID_PAYLOAD),
        "{:?}",
        r.reason
    );
    assert_eq!(*source.min_age.lock().unwrap(), None);
}

#[test]
fn an_agent_without_the_source_refuses_the_type() {
    let r = execute(
        &FakeStore::new(),
        None,
        &command(r#"{"min_age_minutes":120}"#),
    );
    assert_eq!(r.status, CommandStatus::Rejected);
    assert_eq!(r.reason.as_deref(), Some(reasons::UNKNOWN_TYPE));
}
