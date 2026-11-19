//! `Capture/SqlCaptureRunnerTests.cs`: the capture loop end to end over a fake SMILE_POS
//! (real time, short intervals).

mod capture_runner_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use capture_runner_support::*;
use dl_agent_pda_sync::capture::status::{LEDGER_TAIL_JOB, MASTERS_JOB, OPEN_JOB};
use dl_agent_pda_sync::capture::{
    BatchStore, CaptureError, CaptureSource, CaptureState, OPEN_STREAM, OutboxBatchStore,
    SqliteCaptureState, StateFactory, ZoneFactory,
};
use dl_agent_pda_sync::config::{AgentError, RuntimeConfig, codes};
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::mapping::RawValue;
use dl_agent_pda_sync::contracts::wire::SyncBatch;
use dl_agent_pda_sync::outbox::{BlobEnqueueResult, CaptureStatus, SqliteOutbox};
use dl_agent_pda_sync::reporting::Logger;

struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new() -> TestDir {
        let d = std::env::temp_dir().join(format!("pda-b4-{}", dl_uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        TestDir(d)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn drain(outbox: &SqliteOutbox) -> Vec<SyncBatch> {
    outbox
        .peek::<&str>(10_000, &[])
        .unwrap()
        .iter()
        .map(|i| SyncBatch::from_json(std::str::from_utf8(&i.payload).unwrap()).unwrap())
        .collect()
}

fn streams(r: &impl CaptureStatus) -> Vec<String> {
    r.streams().into_iter().map(|s| s.stream).collect()
}

fn config(c: RuntimeConfig) -> Arc<FakeRuntimeConfig> {
    Arc::new(FakeRuntimeConfig::new(c))
}

fn queued() -> BlobEnqueueResult {
    BlobEnqueueResult::Queued
}

#[test]
fn loads_history_then_streams_live_changes_into_the_outbox_with_gap_free_sequences() {
    let env = TestDir::new();
    let site = site();
    let blobs = Arc::new(RecordingBlobStore::new(queued()));
    let outbox =
        Arc::new(SqliteOutbox::open(env.0.join("outbox.db"), i64::MAX, &Logger::none()).unwrap());
    let queue: Arc<dyn BatchStore> = Arc::new(OutboxBatchStore::new(outbox.clone(), None));
    let runner = runner(&site, one_state(), blobs.clone());

    let run = RunningCapture::start(&runner, queue, config(fast_config()));
    run.until(
        || keys(&drain(&outbox), "Invoice", "check_no").len() == 2 && blobs.refs().len() == 1,
        "initial load",
    );
    site.add("Invoice", vec![invoice(3, 0, "30")]);
    site.update(
        "TmpInvoice",
        |r| {
            r.get_ignore_case("CheckNo")
                .is_some_and(|(_, v)| *v == RawValue::I32(100))
        },
        "SubTotal",
        dec("60000"),
    );
    run.until(
        || {
            let b = drain(&outbox);
            keys(&b, "Invoice", "check_no").contains(&"3".to_owned())
                && b.iter().filter(|b| b.table == "TmpInvoice").count() == 2
        },
        "live changes",
    );
    run.stop();

    let batches = drain(&outbox);
    should_be_contiguous_per_stream(&batches);
    let mut k = keys(&batches, "Invoice", "check_no");
    k.sort();
    assert_eq!(k, ["1", "2", "3"], "each closed check is sent once");
    let mut open: Vec<&str> = batches
        .iter()
        .filter(|b| b.table == "TmpInvoice" || b.table == "TmpTrn")
        .map(|b| b.stream.as_str())
        .collect();
    open.dedup();
    assert_eq!(
        open,
        [OPEN_STREAM],
        "one stream keeps a check's changes in order"
    );
    assert!(batches.iter().all(|b| b.agent_id == "agent-test"));
    assert!(runner.sql_reachable());
    assert_eq!(runner.capture_error(), None);
    let s = streams(&runner);
    for want in ["open", "Invoice", "DefRVCList"] {
        assert!(s.iter().any(|x| x == want), "{want} in {s:?}");
    }
}

#[test]
fn resumes_after_a_restart_without_resending_and_picks_up_what_changed_meanwhile() {
    let env = TestDir::new();
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let path = env.0.join("capture.db");
    let state = || -> StateFactory {
        let p = path.clone();
        Arc::new(move || Ok(Arc::new(SqliteCaptureState::open(&p)?) as Arc<dyn CaptureState>))
    };
    let first = runner(&site, state(), Arc::new(RecordingBlobStore::new(queued())));
    let run = RunningCapture::start(&first, queue.clone(), config(fast_config()));
    run.until(
        || queue.rows("Invoice").len() == 2 && first.status().runs(LEDGER_TAIL_JOB) > 0,
        "first run",
    );
    run.stop();

    site.add("Invoice", vec![invoice(3, 0, "30")]);
    let second = runner(&site, state(), Arc::new(RecordingBlobStore::new(queued())));
    let run = RunningCapture::start(&second, queue.clone(), config(fast_config()));
    run.until(
        || {
            queue.rows("Invoice").len() == 3
                && second.status().runs(MASTERS_JOB) > 0
                && second.status().runs(OPEN_JOB) > 1
        },
        "second run",
    );
    run.stop();

    let mut k = keys(&queue.batches(), "Invoice", "check_no");
    k.sort();
    assert_eq!(k, ["1", "2", "3"], "nothing is re-sent after the restart");
    assert_eq!(
        queue.rows("TmpInvoice").len(),
        1,
        "the open check did not change across the restart"
    );
    assert_eq!(queue.rows("DefRVCList").len(), 1);
    should_be_contiguous_per_stream(&queue.batches());
}

#[test]
fn a_full_outbox_pauses_capture_with_outbox_full_and_loses_nothing_once_it_drains() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    queue.set_full(true);
    let runner = runner(
        &site,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
    );

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(
        || runner.capture_error().as_deref() == Some(codes::OUTBOX_FULL),
        "OUTBOX_FULL reported",
    );
    assert!(
        runner.sql_reachable(),
        "the SQL Server is fine; the local queue is the problem"
    );
    assert!(queue.batches().is_empty());
    assert!(
        !queue.needs_resnapshot().unwrap(),
        "capture keeps the unsent changes itself"
    );

    queue.set_full(false);
    run.until(
        || {
            queue.rows("Invoice").len() == 2
                && !queue.rows("TmpInvoice").is_empty()
                && runner.capture_error().is_none()
        },
        "drained",
    );
    run.stop();
    should_be_contiguous_per_stream(&queue.batches());
}

#[test]
fn the_sqlite_outbox_cap_does_not_flag_a_resnapshot_for_capture() {
    let env = TestDir::new();
    let site = site();
    let outbox = Arc::new(SqliteOutbox::open(env.0.join("outbox.db"), 1, &Logger::none()).unwrap());
    let runner = runner(
        &site,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
    );

    let run = RunningCapture::start(
        &runner,
        Arc::new(OutboxBatchStore::new(outbox.clone(), None)),
        config(fast_config()),
    );
    run.until(
        || runner.capture_error().as_deref() == Some(codes::OUTBOX_FULL),
        "OUTBOX_FULL reported",
    );
    run.stop();

    assert!(
        !outbox.needs_resnapshot().unwrap(),
        "a resnapshot would only re-send everything into the same full queue"
    );
    assert_eq!(outbox.depth().unwrap(), 0);
}

#[test]
fn a_missing_enrollment_secret_is_reported_as_unreachable_and_clears_on_recovery() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let broken = Arc::new(AtomicBool::new(true));
    let source = {
        let site = site.clone();
        let broken = broken.clone();
        Arc::new(move || {
            if broken.load(Ordering::SeqCst) {
                Err(CaptureError::from(AgentError::new(
                    codes::REENROLL_REQUIRED,
                    "The SQL password is missing; re-enroll.",
                )))
            } else {
                Ok(site.clone() as Arc<dyn CaptureSource>)
            }
        })
    };
    let runner = runner_with(
        source,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
        None,
    );

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(
        || runner.capture_error().as_deref() == Some(codes::REENROLL_REQUIRED),
        "REENROLL_REQUIRED reported",
    );
    assert!(!runner.sql_reachable());
    assert!(
        !runner.streams().is_empty(),
        "a stopped capture shows growing lag, not an empty healthy list"
    );

    broken.store(false, Ordering::SeqCst);
    run.until(
        || {
            runner.capture_error().is_none()
                && runner.sql_reachable()
                && queue.rows("Invoice").len() == 2
        },
        "recovered",
    );
    run.stop();
}

#[test]
fn a_busy_source_is_reported_as_sql_busy_and_capture_resumes() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let runner = runner(
        &site,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
    );

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(|| queue.rows("Invoice").len() == 2, "initial load");
    site.set_fail(Some(CaptureError::busy(
        "Read of Invoice abandoned (SQL error 1222).",
        None,
    )));
    run.until(
        || runner.capture_error().as_deref() == Some(codes::SQL_BUSY),
        "SQL_BUSY reported",
    );
    assert!(
        runner.sql_reachable(),
        "a lock timeout proves the server answered"
    );

    site.set_fail(None);
    site.add("Invoice", vec![invoice(3, 0, "30")]);
    run.until(
        || runner.capture_error().is_none() && queue.rows("Invoice").len() == 3,
        "resumed",
    );
    run.stop();
}

#[test]
fn an_unexpected_failure_is_reported_retried_and_then_recovers() {
    let site = site();
    site.set_fail(Some(CaptureError::invalid_operation(
        "Table dbo.Invoice does not exist or has no columns.",
    )));
    let queue = Arc::new(MemoryBatchStore::default());
    let runner = runner(
        &site,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
    );

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(
        || runner.capture_error().as_deref() == Some(codes::CAPTURE_FAILED),
        "CAPTURE_FAILED reported",
    );
    let attempts = site.queries();
    site.set_fail(None);
    run.until(
        || runner.capture_error().is_none() && queue.rows("Invoice").len() == 2,
        "recovered",
    );
    run.stop();
    assert_eq!(attempts, 0, "every call failed, so nothing was half-read");
}

#[test]
fn a_requested_resnapshot_resends_every_row_once_and_clears_the_flag() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let blobs = Arc::new(RecordingBlobStore::new(queued()));
    let runner = runner(&site, one_state(), blobs.clone());

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(
        || queue.rows("Invoice").len() == 2 && runner.status().runs(OPEN_JOB) > 0,
        "initial load",
    );
    queue.set_needs_resnapshot(true);
    run.until(
        || {
            !queue.needs_resnapshot().unwrap()
                && queue.rows("Invoice").len() == 4
                && queue.rows("TmpInvoice").len() == 2
                && queue.rows("DefRVCList").len() == 2
        },
        "resnapshot sent",
    );
    run.stop();

    let mut k = keys(&queue.batches(), "Invoice", "check_no");
    k.sort();
    assert_eq!(
        k,
        ["1", "1", "2", "2"],
        "each closed check is re-sent exactly once"
    );
    assert_eq!(blobs.refs().len(), 2, "the picture is offered again too");
    should_be_contiguous_per_stream(&queue.batches());
}

#[test]
fn a_full_blob_queue_holds_the_row_until_the_picture_is_queued() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let blobs = Arc::new(RecordingBlobStore::new(BlobEnqueueResult::CapReached));
    let runner = runner(&site, one_state(), blobs.clone());

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(
        || runner.capture_error().as_deref() == Some(codes::BLOB_QUEUE_FULL),
        "BLOB_QUEUE_FULL reported",
    );
    assert!(
        queue.rows("DefRVCList").is_empty(),
        "a row is never sent ahead of its blob"
    );

    blobs.set_result(queued());
    run.until(
        || {
            runner.capture_error().is_none()
                && queue.rows("DefRVCList").len() == 1
                && blobs.refs().len() == 1
        },
        "recovered",
    );
    run.stop();
}

#[test]
fn a_refused_blob_is_not_retried_and_does_not_stop_capture() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let blobs = Arc::new(RecordingBlobStore::new(BlobEnqueueResult::Refused));
    let runner = runner(&site, one_state(), blobs.clone());

    let run = RunningCapture::start(&runner, queue.clone(), config(fast_config()));
    run.until(
        || queue.rows("DefRVCList").len() == 1 && runner.status().runs(MASTERS_JOB) >= 2,
        "masters scanned twice",
    );
    run.stop();

    assert_eq!(blobs.calls(), 1);
    assert_eq!(runner.capture_error(), None);
}

#[test]
fn an_allowlist_change_restarts_capture_with_the_new_table_set() {
    let site = site();
    let queue = Arc::new(MemoryBatchStore::default());
    let cfg = config(RuntimeConfig {
        table_allowlist: allow(&["Invoice"]),
        ..fast_config()
    });
    let runner = runner(
        &site,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
    );

    let run = RunningCapture::start(&runner, queue.clone(), cfg.clone());
    run.until(
        || queue.rows("Invoice").len() == 2 && runner.status().runs(OPEN_JOB) > 0,
        "allowlisted load",
    );
    assert!(queue.rows("TmpInvoice").is_empty());
    assert_eq!(streams(&runner), ["Invoice"]);

    cfg.set(fast_config());
    run.until(
        || queue.rows("TmpInvoice").len() == 1 && queue.rows("DefRVCList").len() == 1,
        "full table set",
    );
    run.stop();
    assert_eq!(
        queue.rows("Invoice").len(),
        2,
        "tables already captured are not re-sent by the restart"
    );
    let s = streams(&runner);
    for want in ["open", "Invoice", "DefRVCList"] {
        assert!(s.iter().any(|x| x == want), "{want} in {s:?}");
    }
}

#[test]
fn an_unknown_site_time_zone_is_reported() {
    let zone: ZoneFactory = Arc::new(|| {
        Err(dl_agent_pda_sync::capture::error::time_zone_not_found(
            "Not/AZone",
        ))
    });
    let site = site();
    let source = Arc::new(move || Ok(site.clone() as Arc<dyn CaptureSource>));
    let runner = runner_with(
        source,
        one_state(),
        Arc::new(RecordingBlobStore::new(queued())),
        Some(zone),
    );

    let run = RunningCapture::start(
        &runner,
        Arc::new(MemoryBatchStore::default()),
        config(fast_config()),
    );
    run.until(
        || runner.capture_error().as_deref() == Some(codes::SITE_TIME_ZONE_INVALID),
        "SITE_TIME_ZONE_INVALID reported",
    );
    run.stop();
}
