//! `Capture/CaptureStatusTests.cs` and `Capture/CaptureProblemReportingTests.cs`: failure
//! classification, the status tracker, the sinks and the runtime-config mapping.

mod capture_runner_support;

use std::sync::Arc;
use std::time::Duration;

use capture_runner_support::*;
use dl_agent_pda_sync::capture::runner::{allowlisted, failure_detail, options_for, rule_counts};
use dl_agent_pda_sync::capture::status::{LEDGER_TAIL_JOB, MASTERS_JOB, OPEN_JOB};
use dl_agent_pda_sync::capture::{
    BatchSink, BlobItem, BlobSink, CaptureError, CaptureFailureInfo, CapturePlan, CaptureReport,
    CaptureStatusTracker, CaptureTimings, ErrorKind, OPEN_STREAM, OutboxBatchSink, QueueBlobSink,
    RejectedRow, TableFault, classify_failure,
};
use dl_agent_pda_sync::config::time::{ManualClock, Timestamp};
use dl_agent_pda_sync::config::{AgentError, RuntimeConfig, codes};
use dl_agent_pda_sync::contracts::map::StrMap;
use dl_agent_pda_sync::contracts::time::{DateTimeOffset, TimeZone};
use dl_agent_pda_sync::contracts::wire::{SyncBatch, SyncBlobRef, SyncMode};
use dl_agent_pda_sync::outbox::BlobEnqueueResult;
use dl_agent_pda_sync::reporting::SqlFailure;
use dl_agent_pda_sync::util::ctx::Ctx;

fn info(code: &str, reachable: Option<bool>) -> CaptureFailureInfo {
    CaptureFailureInfo::new(code, reachable)
}

#[test]
fn failures_map_to_stable_status_codes() {
    let cases = [
        (
            CaptureError::backpressure(codes::OUTBOX_FULL, "full"),
            codes::OUTBOX_FULL,
            Some(true),
        ),
        (
            CaptureError::from(AgentError::new(codes::REENROLL_REQUIRED, "no password")),
            codes::REENROLL_REQUIRED,
            Some(false),
        ),
        (
            CaptureError::busy("lock timeout", None),
            codes::SQL_BUSY,
            Some(true),
        ),
        (
            CaptureError::local_store("disk I/O error"),
            codes::LOCAL_STORE_FAILED,
            None,
        ),
        (
            dl_agent_pda_sync::capture::error::time_zone_not_found("Not/AZone"),
            codes::SITE_TIME_ZONE_INVALID,
            None,
        ),
        (
            CaptureError::invalid_operation("no columns"),
            codes::CAPTURE_FAILED,
            None,
        ),
    ];
    for (e, code, reachable) in cases {
        assert_eq!(classify_failure(&e), info(code, reachable), "{e:?}");
    }
}

#[test]
fn sql_errors_map_by_number_and_class() {
    for (n, class, code, reachable) in [
        (18456, 14, codes::SQL_LOGIN_FAILED, false),
        (4060, 11, codes::SQL_DATABASE_NOT_FOUND, false),
        (1222, 16, codes::SQL_BUSY, true),
        (53, 20, codes::SQL_UNREACHABLE, false),
        (0, 20, codes::SQL_UNREACHABLE, false),
        (229, 14, codes::SQL_NO_ACCESS, true),
        (207, 16, codes::SQL_SCHEMA_MISMATCH, true),
        (245, 16, codes::SQL_ERROR, true),
    ] {
        let e = CaptureError::sql(SqlFailure::single(n, class, "x"));
        assert_eq!(classify_failure(&e), info(code, Some(reachable)), "{n}");
    }
}

#[test]
fn a_server_that_does_not_answer_is_sql_unreachable() {
    let cfg = dl_tds::Config::new("127.0.0.1")
        .with_port(1)
        .credentials("pda_sync", "x")
        .with_database("SMILE_POS")
        .with_connect_timeout(Duration::from_secs(3));
    let Err(err) = dl_tds::Client::connect(&cfg) else {
        panic!("nothing listens on port 1");
    };
    let failure = SqlFailure::from_tds(&err, &[]).expect("a SqlException");
    let (n, c) = (failure.number(), failure.class());
    assert_eq!(
        classify_failure(&CaptureError::sql(failure)),
        info(codes::SQL_UNREACHABLE, Some(false)),
        "SQL error {n}, class {c}"
    );
}

fn plan_for(config: &RuntimeConfig) -> CapturePlan {
    let cat = catalog();
    let c = allowlisted(&cat, config.table_allowlist.as_deref());
    CapturePlan::build(
        &c,
        &options_for(
            config,
            Arc::new(TimeZone::utc()),
            "agent-test",
            &fast_timings(),
        ),
    )
    .unwrap()
}

fn lag_of(t: &CaptureStatusTracker, stream: &str) -> f64 {
    t.stream_lags()
        .into_iter()
        .find(|s| s.stream == stream)
        .unwrap()
        .lag_seconds
}

fn clock_at(y: i32, mo: u32, d: u32, h: u32) -> Arc<ManualClock> {
    let t = DateTimeOffset::utc(y, mo, d, h, 0, 0).unwrap();
    Arc::new(ManualClock::new(Timestamp::from_unix_nanos(t.unix_nanos())))
}

#[test]
fn lag_grows_from_start_until_the_feeding_job_completes_a_full_pass() {
    let time = clock_at(2026, 10, 28, 8);
    let tracker = CaptureStatusTracker::new(time.clone(), &plan_for(&fast_config()));
    time.advance(Duration::from_millis(90_700));

    assert_eq!(
        lag_of(&tracker, "Invoice"),
        90.0,
        "lag is whole seconds and starts at start-up"
    );
    assert!(!tracker.sql_reachable(), "nothing has been read yet");

    tracker.succeeded(LEDGER_TAIL_JOB, &CaptureReport::new());
    assert_eq!(lag_of(&tracker, "Invoice"), 0.0);
    assert_eq!(
        lag_of(&tracker, OPEN_STREAM),
        90.0,
        "another job feeds the open stream"
    );
    assert!(tracker.sql_reachable());

    time.advance(Duration::from_secs(30));
    let report = CaptureReport {
        deadline_hit: true,
        ..CaptureReport::new()
    };
    tracker.succeeded(LEDGER_TAIL_JOB, &report);
    assert_eq!(
        lag_of(&tracker, "Invoice"),
        30.0,
        "a pass cut short by the deadline is not a fresh read"
    );
    assert_eq!(tracker.runs(LEDGER_TAIL_JOB), 2);
}

#[test]
fn a_table_skipped_after_a_schema_error_keeps_its_lag_while_the_others_turn_fresh() {
    let time = clock_at(2026, 10, 28, 8);
    let plan = plan_for(&fast_config());
    let tracker = CaptureStatusTracker::new(time.clone(), &plan);
    let other = plan
        .ledger
        .iter()
        .map(|p| p.name().to_owned())
        .find(|n| n != "Invoice")
        .unwrap();
    time.advance(Duration::from_secs(60));
    let mut report = CaptureReport::new();
    report.table_faults.push(TableFault {
        table: "Invoice".into(),
        error: CaptureError::invalid_operation("Invalid column name"),
    });

    tracker.succeeded(LEDGER_TAIL_JOB, &report);

    assert_eq!(
        lag_of(&tracker, "Invoice"),
        60.0,
        "a skipped table was not read"
    );
    assert_eq!(lag_of(&tracker, &other), 0.0);
}

fn rejected(table: &str, reason: &str, rule: &str) -> RejectedRow {
    RejectedRow {
        table: table.into(),
        reason: reason.into(),
        rule: rule.into(),
    }
}

#[test]
fn rejected_rows_and_deferred_checks_show_as_lag_rows_of_their_stream() {
    let tracker = CaptureStatusTracker::new(clock_at(1970, 1, 1, 0), &plan_for(&fast_config()));
    let mut report = CaptureReport {
        unstable_checks_deferred: 2,
        ..CaptureReport::new()
    };
    report
        .rejected
        .push(rejected("TmpTrn", "bad money value", "format:SubTotal"));
    report
        .rejected
        .push(rejected("TmpInvoice", "bad datetime", "format:OpenTime"));

    tracker.succeeded(OPEN_JOB, &report);

    let lags = tracker.stream_lags();
    assert_eq!(
        lags.iter()
            .find(|s| s.stream == OPEN_STREAM)
            .unwrap()
            .lag_rows,
        4
    );
    assert_eq!(
        lags.iter()
            .find(|s| s.stream == "Invoice")
            .unwrap()
            .lag_rows,
        0
    );
}

#[test]
fn a_failure_sets_the_code_and_changes_reachability_only_when_it_proves_it() {
    let tracker = CaptureStatusTracker::new(clock_at(1970, 1, 1, 0), &plan_for(&fast_config()));
    tracker.succeeded(MASTERS_JOB, &CaptureReport::new());

    tracker.failed(&info(codes::LOCAL_STORE_FAILED, None), None);
    assert_eq!(tracker.error().as_deref(), Some(codes::LOCAL_STORE_FAILED));
    assert!(tracker.sql_reachable());

    tracker.failed(&info(codes::SQL_LOGIN_FAILED, Some(false)), None);
    assert!(!tracker.sql_reachable());

    tracker.succeeded(MASTERS_JOB, &CaptureReport::new());
    assert_eq!(tracker.error(), None);
    assert!(tracker.sql_reachable());
}

#[test]
fn a_new_plan_drops_streams_that_left_it_and_keeps_the_lag_of_the_rest() {
    let time = clock_at(1970, 1, 1, 0);
    let first = RuntimeConfig {
        table_allowlist: allow(&["Invoice", "TmpInvoice"]),
        ..fast_config()
    };
    let tracker = CaptureStatusTracker::new(time.clone(), &plan_for(&first));
    time.advance(Duration::from_secs(10));
    tracker.succeeded(OPEN_JOB, &CaptureReport::new());

    let second = RuntimeConfig {
        table_allowlist: allow(&["TmpInvoice", "DefRVCList"]),
        ..fast_config()
    };
    tracker.use_plan(&plan_for(&second));

    let names: Vec<String> = tracker
        .stream_lags()
        .into_iter()
        .map(|s| s.stream)
        .collect();
    assert_eq!(names, ["DefRVCList", OPEN_STREAM]);
    assert_eq!(lag_of(&tracker, OPEN_STREAM), 0.0);
    assert_eq!(lag_of(&tracker, "DefRVCList"), 10.0);
}

#[test]
fn runtime_config_is_clamped_to_safe_capture_options() {
    let options = options_for(
        &RuntimeConfig {
            open_check_poll_ms: 1,
            master_scan_seconds: 0,
            closed_rescan_days: 900,
            history_months: 3,
            table_allowlist: None,
        },
        Arc::new(TimeZone::utc()),
        "agent-test",
        &CaptureTimings::default(),
    );
    assert_eq!(
        options.open_interval,
        CaptureTimings::default().min_open_interval
    );
    assert_eq!(
        options.master_interval,
        CaptureTimings::default().min_master_interval
    );
    assert_eq!(options.rescan_days, 31);
    assert_eq!(options.snapshot_window_months, 3);
    assert_eq!(options.log_snapshot_window_months, 3);
}

#[test]
fn the_allowlist_is_case_insensitive_and_empty_means_every_table() {
    let cat = catalog();
    let one = allowlisted(&cat, allow(&["invoice"]).as_deref());
    let names: Vec<&str> = one.tables.iter().map(|t| t.table.as_str()).collect();
    assert_eq!(names, ["Invoice"]);
    assert!(matches!(
        allowlisted(&cat, Some(&[])),
        std::borrow::Cow::Borrowed(_)
    ));
    assert!(matches!(
        allowlisted(&cat, None),
        std::borrow::Cow::Borrowed(_)
    ));
}

fn batch(stream: &str, seq: i64) -> SyncBatch {
    SyncBatch {
        agent_id: "agent-test".into(),
        stream: stream.into(),
        seq,
        mapper_ver: 1,
        table: "Invoice".into(),
        mode: SyncMode::Delta,
        rows: vec![],
        tombstones: vec![],
        watermark: None,
        hops: Default::default(),
    }
}

#[test]
fn the_batch_sink_restamps_the_outbox_sequence_and_fails_when_the_outbox_is_full() {
    let store = Arc::new(MemoryBatchStore::default());
    let sink = OutboxBatchSink::new(store.clone());
    let ctx = Ctx::background();

    sink.send(&batch("Invoice", 41), &ctx).unwrap();
    sink.send(&batch("Invoice", 7), &ctx).unwrap();
    let seqs: Vec<i64> = store.batches().iter().map(|b| b.seq).collect();
    assert_eq!(seqs, [1, 2]);

    store.set_full(true);
    let full = sink.send(&batch("Invoice", 3), &ctx).unwrap_err();
    assert_eq!(
        full.kind,
        ErrorKind::Backpressure(codes::OUTBOX_FULL.into())
    );
    assert_eq!(
        full.message,
        "The outbox is full; capture of stream Invoice waits until uploads drain it."
    );
}

#[test]
fn the_blob_sink_treats_a_refused_blob_as_handled_but_a_full_queue_as_backpressure() {
    let mut key = StrMap::new();
    key.insert("RVCNo", Some("1".to_owned()));
    let blob = BlobItem {
        ref_: SyncBlobRef {
            table: "DefRVCList".into(),
            key,
            column: "Picture".into(),
            sha256: "a".repeat(64),
        },
        content: vec![1],
    };
    let ctx = Ctx::background();

    QueueBlobSink::new(Arc::new(RecordingBlobStore::new(
        BlobEnqueueResult::Refused,
    )))
    .send(&blob, &ctx)
    .unwrap();

    let full = QueueBlobSink::new(Arc::new(RecordingBlobStore::new(
        BlobEnqueueResult::CapReached,
    )))
    .send(&blob, &ctx)
    .unwrap_err();
    assert_eq!(
        full.kind,
        ErrorKind::Backpressure(codes::BLOB_QUEUE_FULL.into())
    );
}

// CaptureProblemReportingTests

#[test]
fn reject_rules_are_counted_most_frequent_first_without_the_row_values() {
    let rows = [
        rejected(
            "tmpInvoiceInfo",
            "Table 'tmpInvoiceInfo': key column 'CheckNo' is missing or null.",
            "missing_key:CheckNo",
        ),
        rejected(
            "tmpInvoiceInfo",
            "Column 'Amount': 'secret-42' is not a decimal.",
            "format:Amount",
        ),
        rejected(
            "tmpInvoiceInfo",
            "Column 'Amount': 'secret-43' is not a decimal.",
            "format:Amount",
        ),
    ];
    let rules = rule_counts(&rows);
    assert_eq!(rules, "format:Amount=2, missing_key:CheckNo=1");
    assert!(!rules.contains("secret"));
}

#[test]
fn a_session_failure_detail_names_the_table_but_not_the_message() {
    let mut e = CaptureError::invalid_operation("Invalid column name 'OrderTime'. value 'secret'");
    e.mark_schema_fault("ReOpenLog");

    let detail = failure_detail(&e);

    assert_eq!(
        detail.get("exception").and_then(|v| v.as_str()),
        Some("InvalidOperationException")
    );
    assert_eq!(
        detail.get("capture_table").and_then(|v| v.as_str()),
        Some("ReOpenLog")
    );
    assert!(!dl_json::Value::from(detail).to_string().contains("secret"));
}

#[test]
fn the_failure_detail_lives_until_the_next_successful_pass() {
    let tracker = CaptureStatusTracker::new(
        dl_agent_pda_sync::config::SystemClock::shared(),
        &plan_for(&fast_config()),
    );
    let mut detail = dl_json::Map::new();
    detail.insert("skipped_tables", "masters/ReOpenLog");
    tracker.failed(&info(codes::SQL_SCHEMA_MISMATCH, Some(true)), Some(&detail));

    assert_eq!(
        tracker
            .error_detail()
            .unwrap()
            .get("skipped_tables")
            .and_then(|v| v.as_str()),
        Some("masters/ReOpenLog")
    );

    tracker.succeeded(MASTERS_JOB, &CaptureReport::new());
    assert!(tracker.error_detail().is_none());
}
