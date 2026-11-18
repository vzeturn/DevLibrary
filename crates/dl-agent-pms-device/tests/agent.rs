//! Ports of TokenFileTests.cs, ResultReporterTests.cs, CommandProcessorTests.cs and
//! DeviceAgentWorkerTests.cs over the fakes in tests/support.

mod support;

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dl_agent_pms_device::adapters::{Cx, DeviceAdapter, adapter_failed};
use dl_agent_pms_device::agent::log::{Level, Logger, NullLogger};
use dl_agent_pms_device::agent::{
    AgentError, Clock, CommandProcessor, DeviceAgentWorker, ResultOutbox, StopToken, SystemClock,
    TokenFile, TokenFileError, WorkerExit, add_duration,
};
use dl_agent_pms_device::config::{AgentOptions, TimeSpan};
use dl_agent_pms_device::contracts::{AdapterError, CommandOutcome, PendingResult, codes, kinds};
use dl_agent_pms_device::queue::{
    CommandQueue, DeviceApiError, QueueError, StaleClaim, StaleClaimReason,
};
use dl_json::json;
use dl_uuid::Uuid;
use support::{FakeAdapter, FakeQueue, ListLogger, ManualClock, TempOutbox, drive, temp_path};

const ISSUE_PAYLOAD: &str =
    r#"{"room":"101","from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00","cards":1}"#;
const LOCK_ERASE_PAYLOAD: &str = include_str!("contract/lock.erase.payload.json");

fn transport(m: &str) -> QueueError {
    QueueError::Transport(m.into())
}

fn api(status: u16, m: &str) -> QueueError {
    QueueError::Api(DeviceApiError::new(status, m))
}

fn stale(id: Uuid, reason: StaleClaimReason) -> QueueError {
    QueueError::StaleClaim(StaleClaim {
        command_id: id,
        reason,
    })
}

fn stopped() -> StopToken {
    let s = StopToken::new();
    s.stop();
    s
}

// ---- TokenFileTests ----

#[test]
fn i0_t1_reads_and_trims_token() {
    let path = temp_path("agent-token");
    std::fs::write(&path, "  abcdefghijklmnopqrstuvwxyz0123456789==\r\n").unwrap();
    let token = TokenFile::new(path.to_string_lossy().into_owned());
    assert_eq!(
        token.read().unwrap(),
        "abcdefghijklmnopqrstuvwxyz0123456789=="
    );
    // Read once: a later change of the file is not seen.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        token.read().unwrap(),
        "abcdefghijklmnopqrstuvwxyz0123456789=="
    );
}

#[test]
fn i0_t2_missing_file_fails_naming_the_path() {
    let path = temp_path("agent-token").to_string_lossy().into_owned();
    let e = TokenFile::new(path.clone()).read().unwrap_err();
    assert!(matches!(e, TokenFileError::NotFound { .. }));
    let ex = e.to_dotnet();
    assert_eq!(ex.type_name, "System.InvalidOperationException");
    assert_eq!(
        ex.message,
        format!("Device agent token file '{path}' was not found")
    );
}

#[test]
fn i0_t3_token_shorter_than_32_chars_fails_without_the_content() {
    let path = temp_path("agent-token");
    std::fs::write(&path, "short-secret").unwrap();
    let p = path.to_string_lossy().into_owned();
    let e = TokenFile::new(p.clone()).read().unwrap_err();
    std::fs::remove_file(&path).unwrap();
    let ex = e.to_dotnet();
    assert_eq!(ex.type_name, "System.InvalidOperationException");
    assert!(!ex.message.contains("short-secret"));
    assert_eq!(
        ex.message,
        format!("Device agent token in '{p}' is shorter than 32 characters; issue a new one")
    );
}

// ---- ResultReporterTests ----

struct Reporting {
    clock: Arc<ManualClock>,
    queue: Arc<FakeQueue>,
    outbox: TempOutbox,
    log: Arc<ListLogger>,
}

impl Reporting {
    fn new() -> Reporting {
        let clock = ManualClock::new();
        Reporting {
            queue: FakeQueue::new(clock.clone()),
            clock,
            outbox: TempOutbox::new(),
            log: ListLogger::new(),
        }
    }

    fn reporter(&self) -> dl_agent_pms_device::agent::ResultReporter {
        self.outbox
            .reporter(self.queue.clone(), self.clock.clone(), self.log.clone())
    }

    fn claimed_result(&self) -> PendingResult {
        self.queue.enqueue(kinds::LOCK_ISSUE, ISSUE_PAYLOAD);
        let cmd = self.queue.claim().unwrap().unwrap();
        PendingResult::from_command(&cmd, &CommandOutcome::done(json!({"card": "SIM-1"})))
    }
}

#[test]
fn i0_r1_transient_failures_are_retried_and_leave_no_outbox_entry() {
    let t = Reporting::new();
    let r = t.claimed_result();
    t.queue.fail_send(transport("connection refused"));
    t.queue.fail_send(api(503, "busy"));
    t.queue.fail_send(api(401, "token rotated"));

    let reporter = t.reporter();
    let stop = StopToken::new();
    assert!(drive(&t.clock, || reporter.report(&r, &stop)).unwrap());

    assert_eq!(t.queue.sent().len(), 4);
    assert_eq!(t.queue.read(r.command_id).status, "done");
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn i0_r2_a_stop_requested_after_the_adapter_acted_still_sends_the_result() {
    let t = Reporting::new();
    let r = t.claimed_result();
    assert!(t.reporter().report(&r, &stopped()).unwrap());
    assert_eq!(t.queue.read(r.command_id).status, "done");
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn i0_r3_api_down_for_the_whole_lease_keeps_the_result_and_the_next_flush_delivers_it() {
    let t = Reporting::new();
    let r = t.claimed_result();
    for _ in 0..100 {
        t.queue.fail_send(transport("API down"));
    }

    let reporter = t.reporter();
    let stop = StopToken::new();
    assert!(!drive(&t.clock, || reporter.report(&r, &stop)).unwrap());

    let attempts = t.queue.sent().len();
    assert!(attempts > 3, "only {attempts} attempts");
    assert!(
        t.clock.now() <= add_duration(r.lease_until, Duration::from_secs(10)),
        "retried far past the lease"
    );
    assert_eq!(t.queue.read(r.command_id).status, "claimed");
    assert_eq!(t.outbox.count(), 1);
    assert!(
        t.log
            .at(Level::Warning)
            .iter()
            .any(|e| e.message.contains("outbox"))
    );

    // A service restart: a new reporter over the same directory finds and sends it.
    t.queue.fail_sends.lock().unwrap().clear();
    t.outbox
        .reporter(t.queue.clone(), t.clock.clone(), Arc::new(NullLogger))
        .flush(&StopToken::new())
        .unwrap();

    let row = t.queue.read(r.command_id);
    assert_eq!(row.status, "done");
    assert_eq!(row.result.unwrap()["card"], "SIM-1");
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn i0_r4_flush_fails_while_a_result_is_still_pending() {
    let t = Reporting::new();
    let r = t.claimed_result();
    t.queue.fail_send(transport("API down"));
    let reporter = t.reporter();
    assert!(!reporter.report(&r, &stopped()).unwrap());

    t.queue.fail_send(api(502, "bad gateway"));
    match reporter.flush(&StopToken::new()) {
        Err(AgentError::PendingResults(p)) => assert_eq!(p.pending, 1),
        other => panic!("{other:?}"),
    }
    assert_eq!(t.outbox.count(), 1);
}

#[test]
fn i0_r5_a_refused_result_is_dropped_and_logged_by_reason() {
    let t = Reporting::new();
    let reporter = t.reporter();
    let s = StopToken::new();

    let r = t.claimed_result();
    t.queue
        .fail_send(stale(r.command_id, StaleClaimReason::NotHeld));
    assert!(reporter.report(&r, &s).unwrap());
    assert!(
        t.log
            .at(Level::Error)
            .iter()
            .any(|e| e.message.contains("newer claim"))
    );

    let conflict = t.claimed_result();
    t.queue.fail_send(stale(
        conflict.command_id,
        StaleClaimReason::OutcomeConflict,
    ));
    assert!(reporter.report(&conflict, &s).unwrap());
    assert!(
        t.log
            .at(Level::Warning)
            .iter()
            .any(|e| e.message.contains("other outcome"))
    );
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn i0_r6_an_unreadable_outbox_entry_is_moved_aside_and_logged() {
    let outbox = TempOutbox::new();
    std::fs::create_dir_all(&outbox.dir).unwrap();
    let bad = outbox.dir.join(format!("{}.json", Uuid::new_v4()));
    std::fs::write(&bad, "{not json").unwrap();
    let log = ListLogger::new();

    assert!(
        ResultOutbox::new(&outbox.dir, log.clone())
            .list()
            .unwrap()
            .is_empty()
    );

    let mut aside = bad.clone().into_os_string();
    aside.push(".corrupt");
    assert!(std::path::Path::new(&aside).exists());
    let errors = log.at(Level::Error);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].category, ResultOutbox::CATEGORY);
}

#[test]
fn reporter_extra_bad_request_is_dropped_and_token_errors_escape() {
    let t = Reporting::new();
    let reporter = t.reporter();
    let s = StopToken::new();
    let r = t.claimed_result();
    t.queue.fail_send(api(422, "invalid body"));
    assert!(reporter.report(&r, &s).unwrap());
    assert!(
        t.log
            .at(Level::Error)
            .iter()
            .any(|e| e.message.contains("as invalid; dropped:"))
    );
    assert_eq!(t.outbox.count(), 0);

    // Not a transport failure (the token file is gone): the error escapes, the entry stays.
    let r = t.claimed_result();
    t.queue.fail_send(QueueError::Other(
        dl_agent_pms_device::config::DotnetError::new(
            "System.InvalidOperationException",
            "no token",
        ),
    ));
    assert!(matches!(
        reporter.report(&r, &s),
        Err(AgentError::Queue(QueueError::Other(_)))
    ));
    assert_eq!(t.outbox.count(), 1);
}

// ---- CommandProcessorTests ----

struct Processing {
    queue: Arc<FakeQueue>,
    adapter: Arc<FakeAdapter>,
    log: Arc<ListLogger>,
    outbox: TempOutbox,
}

impl Processing {
    fn new() -> Processing {
        Processing {
            queue: FakeQueue::new(Arc::new(SystemClock)),
            adapter: FakeAdapter::new(),
            log: ListLogger::new(),
            outbox: TempOutbox::new(),
        }
    }

    fn processor(&self, timeout: Option<TimeSpan>) -> CommandProcessor {
        let options = AgentOptions {
            command_timeout: timeout.unwrap_or(TimeSpan::from_secs(30)),
            ..AgentOptions::default()
        };
        let reporter = self.outbox.reporter(
            self.queue.clone(),
            Arc::new(SystemClock),
            Arc::new(NullLogger),
        );
        CommandProcessor::new(
            self.queue.clone(),
            self.adapter.clone(),
            reporter,
            &options,
            self.log.clone(),
            None,
        )
    }

    fn run_and_get_error(
        &self,
        kind: &str,
        payload: &str,
        timeout: Option<TimeSpan>,
    ) -> dl_agent_pms_device::contracts::DeviceError {
        let id = self.queue.enqueue(kind, payload);
        assert!(self.processor(timeout).run_once(&StopToken::new()).unwrap());
        let row = self.queue.read(id);
        assert_eq!(row.status, "failed");
        row.error.unwrap()
    }
}

#[test]
fn i0_p1_unknown_kind_fails_with_device_unsupported_kind() {
    let t = Processing::new();
    let e = t.run_and_get_error("pbx.post", "{}", None);
    assert_eq!(e.code, codes::UNSUPPORTED_KIND);
    assert_eq!(
        e.message,
        "Kind 'pbx.post' is not supported by adapter 'fake'"
    );
}

#[test]
fn i03b_lock_erase_payload_reaches_the_adapter() {
    let t = Processing::new();
    let id = t.queue.enqueue(kinds::LOCK_ERASE, LOCK_ERASE_PAYLOAD);
    assert!(t.processor(None).run_once(&StopToken::new()).unwrap());
    assert_eq!(t.queue.read(id).status, "done");
}

#[test]
fn i0_p2_payload_with_unknown_member_fails_with_device_bad_payload() {
    let t = Processing::new();
    let e = t.run_and_get_error(
        kinds::LOCK_ISSUE,
        r#"{"room":"101","rooom":1,"from":"2026-10-14T14:00:00","to":"2026-10-16T12:00:00"}"#,
        None,
    );
    assert_eq!(e.code, codes::BAD_PAYLOAD);
    assert_eq!(
        e.message,
        "The JSON property 'rooom' could not be mapped to any .NET member contained in type 'SmilePms.DeviceAgent.Commands.KeyCardRequest'."
    );
}

#[test]
fn i0_p3_adapter_that_hangs_fails_with_device_timeout_within_command_timeout() {
    let t = Processing::new();
    t.adapter.on_issue(|cx: &Cx| {
        cx.sleep(Duration::from_secs(3600))?;
        Ok(dl_json::Value::Null)
    });
    let watch = Instant::now();
    let e = t.run_and_get_error(
        kinds::LOCK_ISSUE,
        ISSUE_PAYLOAD,
        Some(TimeSpan::from_millis(50)),
    );
    assert_eq!(e.code, codes::TIMEOUT);
    assert_eq!(e.message, "Adapter did not answer within 0 s");
    // Far below the adapter's hour-long sleep, with headroom for a loaded machine.
    assert!(watch.elapsed() < Duration::from_secs(30));
    assert!(
        t.log
            .at(Level::Warning)
            .iter()
            .any(|e| e.message.ends_with("timed out after 00:00:00.0500000"))
    );
}

#[test]
fn i0_p4_adapter_failure_is_device_adapter_failed_and_logged_as_error() {
    let t = Processing::new();
    t.adapter
        .on_issue(|_: &Cx| Err(adapter_failed("InvalidOperationException", "COM port busy")));
    let e = t.run_and_get_error(kinds::LOCK_ISSUE, ISSUE_PAYLOAD, None);
    assert_eq!(e.code, codes::ADAPTER_FAILED);
    assert!(e.message.contains("COM port busy"));
    let errors = t.log.at(Level::Error);
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].exception.as_ref().unwrap().type_name,
        "InvalidOperationException"
    );
    assert!(
        errors[0]
            .message
            .starts_with("Adapter fake failed on device command ")
    );
}

#[test]
fn adapter_panic_is_device_adapter_failed() {
    let t = Processing::new();
    t.adapter.on_issue(|_: &Cx| panic!("vendor bug"));
    let e = t.run_and_get_error(kinds::LOCK_ISSUE, ISSUE_PAYLOAD, None);
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::ADAPTER_FAILED, "Panic: vendor bug")
    );
    assert_eq!(t.log.at(Level::Error).len(), 1);
}

#[test]
fn i0_p5_adapter_code_is_kept() {
    let t = Processing::new();
    t.adapter
        .on_issue(|_: &Cx| Err(AdapterError::new(codes::NO_CARD, "no card")));
    let e = t.run_and_get_error(kinds::LOCK_ISSUE, ISSUE_PAYLOAD, None);
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (codes::NO_CARD, "no card")
    );
    let warn = t.log.at(Level::Warning);
    assert!(warn.iter().any(|w| {
        w.message
            .starts_with("Adapter fake refused device command ")
            && w.message.ends_with(": DEVICE_NO_CARD")
    }));
}

#[test]
fn pos_kinds_without_a_router_are_not_connected() {
    let t = Processing::new();
    let e = t.run_and_get_error(kinds::POS_KICK, "{\"bad\":1}", None);
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        (
            codes::NOT_CONNECTED,
            "POS devices are not set up on this agent"
        )
    );
}

#[test]
fn i0_p6_service_stop_during_execution_propagates_and_posts_nothing() {
    let t = Processing::new();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let started_tx = std::sync::Mutex::new(started_tx);
    t.adapter.on_issue(move |cx: &Cx| {
        let _ = started_tx.lock().unwrap().send(());
        cx.sleep(Duration::from_secs(3600))?;
        Ok(dl_json::Value::Null)
    });
    t.queue.enqueue(kinds::LOCK_ISSUE, ISSUE_PAYLOAD);
    let processor = t.processor(None);
    let stop = StopToken::new();
    let result = std::thread::scope(|s| {
        let run = s.spawn(|| processor.run_once(&stop));
        started_rx.recv_timeout(Duration::from_secs(20)).unwrap();
        stop.stop();
        run.join().unwrap()
    });
    assert_eq!(result, Err(AgentError::Stopped));
    assert!(t.queue.sent().is_empty());
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn i0_p7_stale_claim_on_result_does_not_fail_and_leaves_no_outbox_entry() {
    let t = Processing::new();
    let id = t.queue.enqueue(kinds::LOCK_ISSUE, ISSUE_PAYLOAD);
    t.queue.fail_send(stale(id, StaleClaimReason::NotHeld));
    assert!(t.processor(None).run_once(&StopToken::new()).unwrap());
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn i0_p8_a_result_left_in_the_outbox_is_sent_before_the_next_claim() {
    let t = Processing::new();
    let first = t.queue.enqueue(kinds::LOCK_ISSUE, ISSUE_PAYLOAD);
    // The result send fails and the service is stopping, so the result stays in the outbox.
    t.queue.fail_send(transport("API down"));
    let cmd = t.queue.claim().unwrap().unwrap();
    let reporter = t
        .outbox
        .reporter(t.queue.clone(), Arc::new(SystemClock), Arc::new(NullLogger));
    let r = PendingResult::from_command(&cmd, &CommandOutcome::done(json!({"card": "SIM-1"})));
    assert!(!reporter.report(&r, &stopped()).unwrap());
    assert_eq!(t.outbox.count(), 1);
    let second = t.queue.enqueue(kinds::LOCK_READ, "{}");

    assert!(t.processor(None).run_once(&StopToken::new()).unwrap());

    assert_eq!(
        t.queue.calls(),
        vec![
            "claim".to_owned(),
            format!("send:{first}"),
            format!("send:{first}"),
            "claim".to_owned(),
            format!("send:{second}"),
        ]
    );
    assert_eq!(t.queue.read(first).status, "done");
    assert_eq!(t.queue.read(second).status, "done");
    assert_eq!(t.outbox.count(), 0);
}

#[test]
fn processor_logs_claim_and_finish_like_the_csharp_agent() {
    let t = Processing::new();
    let id = t.queue.enqueue(kinds::LOCK_READ, "{}");
    assert!(t.processor(None).run_once(&StopToken::new()).unwrap());
    let info: Vec<String> = t
        .log
        .at(Level::Information)
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert_eq!(
        info,
        vec![
            format!("Claimed device command {id} lock.read attempt 1"),
            format!("Device command {id} finished done (null)"),
        ]
    );
    assert!(
        t.log
            .entries()
            .iter()
            .all(|e| e.category == CommandProcessor::CATEGORY)
    );
}

// ---- DeviceAgentWorkerTests ----

const READ_PAYLOAD: &str = "{}";

struct Working {
    queue: Arc<FakeQueue>,
    clock: Arc<ManualClock>,
    log: Arc<ListLogger>,
    outbox: TempOutbox,
}

impl Working {
    fn new() -> Working {
        let clock = ManualClock::new();
        Working {
            queue: FakeQueue::new(clock.clone()),
            clock,
            log: ListLogger::new(),
            outbox: TempOutbox::new(),
        }
    }

    fn worker(&self, run_once: bool) -> DeviceAgentWorker {
        let options = AgentOptions {
            agent_id: "FD01".into(),
            run_once,
            ..AgentOptions::default()
        };
        let adapter: Arc<dyn DeviceAdapter> = FakeAdapter::new();
        let null: Arc<dyn Logger> = Arc::new(NullLogger);
        let reporter = self
            .outbox
            .reporter(self.queue.clone(), self.clock.clone(), null.clone());
        let processor =
            CommandProcessor::new(self.queue.clone(), adapter, reporter, &options, null, None);
        DeviceAgentWorker::new(processor, &options, self.clock.clone(), self.log.clone())
    }
}

#[test]
fn i0_w1_run_once_drains_the_queue_then_stops() {
    let t = Working::new();
    let a = t.queue.enqueue(kinds::LOCK_READ, READ_PAYLOAD);
    let b = t.queue.enqueue(kinds::LOCK_READ, READ_PAYLOAD);
    let exit = t.worker(true).run(&StopToken::new());
    assert_eq!(exit, WorkerExit::QueueEmpty);
    assert_eq!(exit.exit_code(), 0);
    assert_eq!(t.queue.read(a).status, "done");
    assert_eq!(t.queue.read(b).status, "done");
    let info: Vec<String> = t
        .log
        .at(Level::Information)
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert_eq!(
        info,
        vec![
            "Device agent FD01 polling https://localhost:3001".to_owned(),
            "Device agent FD01: queue empty, run-once mode stops the service".to_owned(),
        ]
    );
}

#[test]
fn i0_w2_poll_failure_is_logged_and_retried_with_backoff() {
    let t = Working::new();
    t.queue.fail_claim(transport("API down"));
    let id = t.queue.enqueue(kinds::LOCK_READ, READ_PAYLOAD);
    let worker = t.worker(false);
    let stop = StopToken::new();
    let exit = std::thread::scope(|s| {
        let h = s.spawn(|| worker.run(&stop));
        support::wait_until("an error is logged", || !t.log.at(Level::Error).is_empty());
        assert_eq!(t.queue.read(id).status, "queued");

        // The worker logs before it starts the 2 s wait, so advance until it has run.
        support::wait_until("the command is done", || {
            t.clock.advance(Duration::from_secs(2));
            t.queue.read(id).status == "done"
        });
        stop.stop();
        h.join().unwrap()
    });
    assert_eq!(exit, WorkerExit::Stopped);

    let errors = t.log.at(Level::Error);
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].exception.as_ref().unwrap().type_name,
        "System.Net.Http.HttpRequestException"
    );
    assert_eq!(
        errors[0].message,
        "Device agent poll failed (1 in a row); retrying in 00:00:02"
    );
}

#[test]
fn i0_w4_run_once_stops_with_a_non_zero_exit_code_on_an_error() {
    let t = Working::new();
    t.queue
        .fail_claim(api(401, "Invalid device agent credentials"));
    let exit = t.worker(true).run(&StopToken::new());
    assert_eq!(exit, WorkerExit::RunOnceFailed);
    assert_eq!(exit.exit_code(), 1);
    let errors = t.log.at(Level::Error);
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].exception.as_ref().unwrap().type_name,
        "SmilePms.DeviceAgent.Queue.DeviceApiException"
    );
}

#[test]
fn i0_w3_stop_exits_cleanly_while_idle() {
    let t = Working::new();
    let worker = t.worker(false);
    let stop = StopToken::new();
    let exit = std::thread::scope(|s| {
        let h = s.spawn(|| worker.run(&stop));
        std::thread::sleep(Duration::from_millis(50));
        stop.stop();
        h.join().unwrap()
    });
    assert_eq!(exit, WorkerExit::Stopped);
    assert!(t.log.entries().iter().all(|e| e.level < Level::Error));
}

#[test]
fn backoff_doubles_up_to_30_seconds() {
    let t = Working::new();
    let worker = t.worker(false);
    let stop = StopToken::new();
    // Fail six polls in a row: delays 2, 4, 8, 16, 30, 30.
    t.queue.fail_claim(transport("API down"));
    std::thread::scope(|s| {
        let h = s.spawn(|| worker.run(&stop));
        support::wait_until("the first failure", || t.log.at(Level::Error).len() == 1);
        for n in 2..=6usize {
            t.queue.fail_claim(transport("API down"));
            support::wait_until("the next failure", || {
                t.clock.advance(Duration::from_secs(1));
                t.log.at(Level::Error).len() >= n
            });
        }
        stop.stop();
        h.join().unwrap();
    });
    let delays: Vec<String> = t
        .log
        .at(Level::Error)
        .into_iter()
        .map(|e| e.message.rsplit(' ').next().unwrap().to_owned())
        .collect();
    assert_eq!(
        delays,
        [
            "00:00:02", "00:00:04", "00:00:08", "00:00:16", "00:00:30", "00:00:30"
        ]
    );
}
