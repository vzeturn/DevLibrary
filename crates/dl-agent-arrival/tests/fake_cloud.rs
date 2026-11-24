//! The whole link (src/agent/runtime.rs) against a fake cloud on a loopback socket and a
//! fake PMS: answers go back sealed, retried writes come from the journal, a wrong clock and
//! a refused key turn the module degraded with the right report.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::cloud::{FakeCloud, Reply, wait_until};
use common::{TempDir, folios, new_tenant_key};
use dl_agent_arrival::Error;
use dl_agent_arrival::agent::clock::{Clock, SkewedClock, SystemClock};
use dl_agent_arrival::agent::executor::FakeExecutor;
use dl_agent_arrival::agent::health::{self, Health, RecordingSink};
use dl_agent_arrival::agent::layout::Layout;
use dl_agent_arrival::agent::link_status;
use dl_agent_arrival::agent::runtime::{Parts, Wiring};
use dl_agent_arrival::agent::secrets::Identity;
use dl_agent_arrival::agent::settings::Settings;
use dl_agent_arrival::contracts::values::to_base64;
use dl_agent_arrival::contracts::{Params, PmsRequest, PmsResponse, Status, errors};
use dl_time::Offset;
use dl_winsvc::StopSignal;

const LIMIT: Duration = Duration::from_secs(15);

fn identity() -> Identity {
    Identity {
        tenant_id: "0f8fad5b-d9cb-469f-a165-70867728950e".into(),
        agent_id: "agt-test".into(),
        kid: "k-test".into(),
        secret: to_base64(&dl_rand::secure_bytes::<32>().expect("random")),
    }
}

fn request(id: &str, operation: &str, params: &[(&str, &str)], key: Option<&str>) -> PmsRequest {
    let mut parameters = Params::new();
    for (k, v) in params {
        parameters.insert(*k, Some((*v).to_string()));
    }
    PmsRequest {
        request_id: id.into(),
        operation: operation.into(),
        parameters,
        base_etag: None,
        idempotency_key: key.map(str::to_string),
        projection: None,
        ..PmsRequest::default()
    }
}

struct Agent {
    stop: StopSignal,
    thread: JoinHandle<Result<(), Error>>,
    sink: Arc<RecordingSink>,
    health: Arc<Health>,
    pms: Arc<FakeExecutor>,
}

impl Agent {
    fn start(dir: &Path, url: &str, id: &Identity, clock: Arc<dyn Clock>) -> Agent {
        let pms = FakeExecutor::new(vec![Ok(vec![folios(&[(7, "Guest")])])]);
        Agent::start_with(dir, url, id, clock, pms)
    }

    /// [`start`](Self::start) over `pms`, with two PMS slots.
    fn start_with(
        dir: &Path,
        url: &str,
        id: &Identity,
        clock: Arc<dyn Clock>,
        pms: Arc<FakeExecutor>,
    ) -> Agent {
        let settings = Settings {
            cloud_base_url: url.to_string(),
            tenant_key: new_tenant_key(),
            max_concurrency: 2,
            ..Settings::default()
        };
        let sink = Arc::new(RecordingSink::default());
        let health = Arc::new(Health::new(sink.clone(), health::REPORT_INTERVAL));
        let wiring = Wiring {
            executor: pms.clone(),
            clock,
            health: Arc::clone(&health),
            utc_offset: Offset::from_seconds(7 * 3600).expect("offset"),
            installer: None,
            problems: None,
            installations: None,
        };
        let parts = Parts::assemble(&settings, &Layout::new(dir.to_path_buf(), None), id, wiring)
            .expect("assemble");
        let stop = StopSignal::new();
        let s = stop.clone();
        let thread = std::thread::spawn(move || parts.run(&s, &|| {}));
        Agent {
            stop,
            thread,
            sink,
            health,
            pms,
        }
    }

    /// Stops the link and checks it ends promptly and cleanly.
    fn stop(self) -> (Arc<RecordingSink>, Arc<FakeExecutor>) {
        let started = Instant::now();
        self.stop.stop();
        self.thread.join().expect("link thread").expect("link run");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "stop took {:?}",
            started.elapsed()
        );
        (self.sink, self.pms)
    }

    fn has_code(&self, code: &str) -> bool {
        self.sink.codes().iter().any(|c| c == code)
    }
}

#[test]
fn poll_brings_a_request_and_the_answer_goes_back_sealed() {
    let dir = TempDir::new("cloud-poll");
    let id = identity();
    let sent = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&sent);
    let cloud = FakeCloud::start(&id, move |seen| {
        let first = seen.exchange.path.ends_with("/poll") && !s.swap(true, Ordering::SeqCst);
        Reply::Requests(if first {
            vec![request("r1", "booking.list-checkin", &[], None)]
        } else {
            Vec::new()
        })
    });
    let agent = Agent::start(dir.path(), &cloud.url, &id, Arc::new(SystemClock));
    assert!(cloud.wait(LIMIT, |c| c.answer("r1").is_some()), "no answer");
    assert!(wait_until(LIMIT, || agent
        .sink
        .last_state()
        .is_some_and(|s| s.0)));

    let answer = cloud.answer("r1").expect("answer");
    assert_eq!(answer.status, Status::Ok);
    assert_eq!(
        answer.result_sets[0].rows[0].get("FolioNum"),
        Some(&dl_json::Value::from(7))
    );
    let hello = cloud.exchanges()[0]
        .body
        .get("hello")
        .cloned()
        .expect("hello on the first poll");
    assert!(
        hello
            .get("agentVersion")
            .and_then(|v| v.as_str())
            .is_some_and(|v| v.starts_with("dl-agent/"))
    );
    assert!(
        cloud
            .exchanges()
            .iter()
            .any(|e| e.path.ends_with("/api/agent/results"))
    );
    assert_eq!(agent.health.metrics.requests_ok.load(Ordering::Relaxed), 1);
    assert!(agent.health.metrics.last_poll.load(Ordering::Relaxed) > 0);
    assert!(dir.path().join(link_status::FILE_NAME).exists());

    let (sink, pms) = agent.stop();
    assert_eq!(pms.call_count(), 1);
    assert!(sink.codes().is_empty(), "{:?}", sink.codes());
    assert!(
        sink.metrics
            .lock()
            .expect("metrics")
            .contains_key("pms_latency_ms")
    );
}

#[test]
fn a_replayed_write_is_answered_from_the_journal_even_after_a_restart() {
    let dir = TempDir::new("cloud-replay");
    let id = identity();
    let write = |rid: &str| {
        request(
            rid,
            "checkin.online",
            &[("FolioNum", "7")],
            Some("checkin-7"),
        )
    };
    let script = move |seen: &common::cloud::Seen<'_>| {
        if !seen.exchange.path.ends_with("/poll") {
            return Reply::Requests(Vec::new());
        }
        let done = |rid: &str| seen.answered.iter().any(|r| r.request_id == rid);
        let next = match () {
            () if seen.polls == 0 => Some("r1"),
            () if done("r1") && !done("r2") && seen.polls.is_multiple_of(5) => Some("r2"),
            () => None,
        };
        Reply::Requests(next.map(|rid| vec![write(rid)]).unwrap_or_default())
    };
    let cloud = FakeCloud::start(&id, script);
    let agent = Agent::start(dir.path(), &cloud.url, &id, Arc::new(SystemClock));
    assert!(
        cloud.wait(LIMIT, |c| c.answer("r2").is_some()),
        "no replay answer"
    );
    let (_, pms) = agent.stop();
    let (first, replay) = (
        cloud.answer("r1").expect("r1"),
        cloud.answer("r2").expect("r2"),
    );
    assert_eq!(pms.call_count(), 1, "the PMS ran the write twice");
    assert_eq!(replay.status, first.status);
    assert_eq!(replay.result_sets, first.result_sets);
    let journal =
        std::fs::read_to_string(dir.path().join("agent-idempotency.jsonl")).expect("journal");
    assert!(journal.contains("\"key\":\"checkin-7\""));
    drop(cloud);

    // A new agent process reads the journal the old one flushed.
    let script = move |seen: &common::cloud::Seen<'_>| {
        Reply::Requests(
            if seen.exchange.path.ends_with("/poll") && seen.polls == 0 {
                vec![write("r3")]
            } else {
                Vec::new()
            },
        )
    };
    let cloud = FakeCloud::start(&id, script);
    let agent = Agent::start(dir.path(), &cloud.url, &id, Arc::new(SystemClock));
    assert!(
        cloud.wait(LIMIT, |c| c.answer("r3").is_some()),
        "no answer after restart"
    );
    let (_, pms) = agent.stop();
    assert_eq!(pms.call_count(), 0);
    assert_eq!(
        cloud.answer("r3").expect("r3").result_sets,
        first.result_sets
    );
}

#[test]
fn a_wrong_clock_reports_clock_skew() {
    // This machine runs 10 minutes fast: the cloud refuses it.
    let dir = TempDir::new("cloud-skew");
    let id = identity();
    let cloud = FakeCloud::start(&id, |_| Reply::Requests(Vec::new()));
    let fast = Arc::new(SkewedClock { offset_ms: 600_000 });
    let agent = Agent::start(dir.path(), &cloud.url, &id, fast);
    assert!(
        wait_until(LIMIT, || agent.has_code(health::CLOCK_SKEW)),
        "{:?}",
        agent.sink.codes()
    );
    let state = agent.sink.last_state().expect("state");
    assert!(!state.0 && state.1.contains("date and time"), "{state:?}");
    agent.stop();

    // The cloud's reply is stamped outside the window: the agent refuses it.
    let dir = TempDir::new("cloud-skew-reply");
    let cloud = FakeCloud::start(&id, |_| Reply::Skewed(-600_000));
    let agent = Agent::start(dir.path(), &cloud.url, &id, Arc::new(SystemClock));
    assert!(
        wait_until(LIMIT, || agent.has_code(health::CLOCK_SKEW)),
        "{:?}",
        agent.sink.codes()
    );
    let state = agent.sink.last_state().expect("state");
    assert!(!state.0 && state.1.contains("clock"), "{state:?}");
    agent.stop();
}

#[test]
fn a_refused_key_turns_the_module_degraded() {
    let dir = TempDir::new("cloud-401");
    let id = identity();
    let cloud = FakeCloud::start(&id, |_| Reply::Refuse {
        status: 401,
        clock: false,
    });
    let agent = Agent::start(dir.path(), &cloud.url, &id, Arc::new(SystemClock));
    assert!(
        wait_until(LIMIT, || agent.has_code(health::AUTH_FAILED)),
        "{:?}",
        agent.sink.codes()
    );
    let state = agent.sink.last_state().expect("state");
    assert!(
        !state.0 && state.1.contains("agt-test (k-test)"),
        "{state:?}"
    );
    assert!(
        agent
            .sink
            .states
            .lock()
            .expect("states")
            .iter()
            .all(|s| !s.0)
    );
    let status =
        std::fs::read_to_string(dir.path().join(link_status::FILE_NAME)).expect("link status");
    assert!(status.contains("\"state\": \"failed\""), "{status}");
    let (_, pms) = agent.stop();
    assert_eq!(pms.call_count(), 0);
}

#[test]
fn an_unreachable_cloud_is_degraded_after_one_backoff() {
    let dir = TempDir::new("cloud-down");
    let id = identity();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let agent = Agent::start(
        dir.path(),
        &format!("http://127.0.0.1:{port}"),
        &id,
        Arc::new(SystemClock),
    );
    // One failure is a blip; the second, after a backoff, is reported.
    assert!(
        wait_until(LIMIT, || agent.has_code(health::CLOUD_FAILED)),
        "{:?}",
        agent.sink.codes()
    );
    let state = agent.sink.last_state().expect("state");
    assert!(
        !state.0 && state.1.contains("Cannot reach the cloud"),
        "{state:?}"
    );
    agent.stop();
}

/// One poll carrying three cache-warm reads, then a reception read, then a warm read with a
/// 300 ms deadline, against a PMS that takes 400 ms a call and two PMS slots. Returns the
/// answers once all five are back.
fn warm_then_reception(dir: &Path, background: bool) -> Vec<PmsResponse> {
    let id = identity();
    let sent = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&sent);
    let cloud = FakeCloud::start(&id, move |seen| {
        let first = seen.exchange.path.ends_with("/poll") && !s.swap(true, Ordering::SeqCst);
        Reply::Requests(if first {
            let mut batch: Vec<PmsRequest> = ["w1", "w2", "w3", "reception", "w-late"]
                .iter()
                .map(|r| request(r, "booking.list-checkin", &[], None))
                .collect();
            for r in &mut batch {
                r.background = background && r.request_id != "reception";
                r.deadline_ms = Some(20_000);
            }
            batch[4].deadline_ms = Some(300);
            batch
        } else {
            Vec::new()
        })
    });
    let pms = Arc::new(FakeExecutor {
        answers: std::sync::Mutex::new(vec![Ok(vec![folios(&[(7, "Guest")])])]),
        delay: Duration::from_millis(400),
        ..FakeExecutor::default()
    });
    let agent = Agent::start_with(dir, &cloud.url, &id, Arc::new(SystemClock), pms);
    assert!(
        cloud.wait(LIMIT, |c| c.answered().len() >= 5),
        "{:?}",
        cloud.answered()
    );
    let answers = cloud.answered();
    agent.stop();
    answers
}

fn by_id<'a>(answers: &'a [PmsResponse], id: &str) -> &'a PmsResponse {
    answers
        .iter()
        .find(|r| r.request_id == id)
        .expect("answered")
}

/// RT-0755: cache warming holds at most `min(2, MaxConcurrency - 1)` PMS slots, so reception
/// never waits for a slot behind it; a request whose deadline passed in the queue is skipped.
#[test]
fn reception_never_queues_behind_cache_warming() {
    let before_dir = TempDir::new("cloud-prio-old");
    let before = warm_then_reception(before_dir.path(), false);
    let after_dir = TempDir::new("cloud-prio");
    let after = warm_then_reception(after_dir.path(), true);

    let old = by_id(&before, "reception");
    let new = by_id(&after, "reception");
    eprintln!(
        "reception queue wait: no background flag (older cloud) {} ms, flagged warming {} ms",
        old.queue_ms, new.queue_ms
    );
    assert_eq!(new.status, Status::Ok);
    assert!(new.queue_ms < 200, "reception waited {} ms", new.queue_ms);
    assert!(
        old.queue_ms >= 300,
        "the unflagged run did not queue: {old:?}"
    );
    assert!(new.elapsed_ms >= new.queue_ms + 350, "{new:?}");

    // Warming ran one at a time (one background slot of two): each waited for the one before.
    let waits: Vec<i32> = ["w1", "w2", "w3"]
        .iter()
        .map(|w| by_id(&after, w).queue_ms)
        .collect();
    eprintln!("warm queue waits (one background slot): {waits:?} ms");
    assert!(waits[1] >= 350 && waits[2] >= 750, "{waits:?}");

    for answers in [&before, &after] {
        let late = by_id(answers, "w-late");
        assert_eq!(late.status, Status::Failed, "{late:?}");
        assert_eq!(late.error.as_deref(), Some(errors::EXPIRED_MESSAGE));
        assert!(late.queue_ms > 300, "{late:?}");
    }
}
