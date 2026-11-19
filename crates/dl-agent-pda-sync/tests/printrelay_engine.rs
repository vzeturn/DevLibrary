//! Port of `PrintRelayEngineTests.cs` and `PrintRelayEngineSpoolerTests.cs` (with the
//! fakes of `PrintRelayFakes.cs`): the journaled job flow, acks and spooler routing.

use dl_agent_pda_sync::config::PrintRelayOptions;
use dl_agent_pda_sync::config::time::{ManualClock, Timestamp};
use dl_agent_pda_sync::contracts::time::DateTimeOffset;
use dl_agent_pda_sync::contracts::wire::{PrintQueueInfo, print_queue_kinds};
use dl_agent_pda_sync::printrelay::engine::{EXPIRED, STOPPED_WHILE_PRINTING, invalid};
use dl_agent_pda_sync::printrelay::spooler::{spooler_error, win32};
use dl_agent_pda_sync::printrelay::{
    PrintError, PrintJournal, PrintJournalEntry, PrintRelayEngine, PrintRelayEngineParts,
    PrintRelayJob, PrintRelayLink, PrintSendError, PrinterTargetPolicy, RawPrinter, SpoolerPrinter,
    SpoolerQueues, print_job_states,
};
use dl_agent_pda_sync::reporting::{Exception, Logger, SiteEvents};
use dl_agent_pda_sync::util::ctx::Ctx;
use dl_json::Map;
use dl_uuid::Uuid;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NOW: &str = "2026-11-15T08:00:00+00:00";

fn at(s: &str) -> DateTimeOffset {
    DateTimeOffset::parse(s).unwrap()
}

#[derive(Default)]
struct FakeRawPrinter {
    printed: Mutex<Vec<(SocketAddr, Vec<u8>)>>,
    fail: Mutex<Option<PrintSendError>>,
}

impl RawPrinter for FakeRawPrinter {
    fn print(&self, target: SocketAddr, payload: &[u8], _: &Ctx) -> Result<(), PrintError> {
        if let Some(f) = self.fail.lock().unwrap().clone() {
            return Err(PrintError::Send(f));
        }
        self.printed
            .lock()
            .unwrap()
            .push((target, payload.to_vec()));
        Ok(())
    }
}

#[derive(Default)]
struct FakeSpoolerPrinter {
    printed: Mutex<Vec<(String, String, Vec<u8>)>>,
    fail: Mutex<Option<PrintSendError>>,
}

impl SpoolerPrinter for FakeSpoolerPrinter {
    fn print(&self, queue: &str, doc: &str, payload: &[u8], _: &Ctx) -> Result<(), PrintError> {
        if let Some(f) = self.fail.lock().unwrap().clone() {
            return Err(PrintError::Send(f));
        }
        self.printed
            .lock()
            .unwrap()
            .push((queue.to_owned(), doc.to_owned(), payload.to_vec()));
        Ok(())
    }
}

type Ack = (Uuid, bool, Option<String>, bool);

#[derive(Default)]
struct FakeRelayLink {
    acks: Mutex<Vec<Ack>>,
    fail: Mutex<Option<String>>,
}

impl FakeRelayLink {
    fn acks(&self) -> Vec<Ack> {
        self.acks.lock().unwrap().clone()
    }
}

impl PrintRelayLink for FakeRelayLink {
    fn ack(
        &self,
        job_id: Uuid,
        ok: bool,
        error: Option<&str>,
        retryable: bool,
        _: &Ctx,
    ) -> Result<bool, Box<Exception>> {
        if let Some(f) = self.fail.lock().unwrap().clone() {
            return Err(Box::new(Exception::new(
                "System.Net.Http.HttpRequestException",
                f,
            )));
        }
        self.acks
            .lock()
            .unwrap()
            .push((job_id, ok, error.map(str::to_owned), retryable));
        Ok(true)
    }
}

/// `RecordingEvents`.
#[derive(Default)]
struct RecordingEvents {
    reported: Mutex<Vec<(String, String, Option<Map>)>>,
}

impl RecordingEvents {
    fn codes(&self) -> Vec<String> {
        self.reported
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.1.clone())
            .collect()
    }
}

impl SiteEvents for RecordingEvents {
    fn report(&self, level: &str, code: &str, _message: &str, detail: Option<Map>) {
        self.reported
            .lock()
            .unwrap()
            .push((level.to_owned(), code.to_owned(), detail));
    }
}

struct Fixture {
    dir: PathBuf,
    clock: Arc<ManualClock>,
    printer: Arc<FakeRawPrinter>,
    spooler: Arc<FakeSpoolerPrinter>,
    queues: Arc<SpoolerQueues>,
    link: Arc<FakeRelayLink>,
    events: Arc<RecordingEvents>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    fn new() -> Fixture {
        let dir = std::env::temp_dir()
            .join("pda-sync-agent-tests")
            .join(format!("pr-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        Fixture {
            dir,
            clock: Arc::new(ManualClock::new(Timestamp::parse_iso(NOW).unwrap())),
            printer: Arc::default(),
            spooler: Arc::default(),
            queues: Arc::default(),
            link: Arc::default(),
            events: Arc::default(),
        }
    }

    /// With the spooler test class's reported queues.
    fn with_queues() -> Fixture {
        let f = Fixture::new();
        let q = |name: &str, kind: &str, port: &str, host: Option<&str>| PrintQueueInfo {
            name: name.into(),
            kind: kind.into(),
            port_name: port.into(),
            host: host.map(str::to_owned),
            port: host.map(|_| 9100),
        };
        f.queues.replace(&[
            q("POS-80 Cashier", print_queue_kinds::USB, "USB001", None),
            q("Bar WSD", print_queue_kinds::WSD, "WSD-1", None),
            q(
                "Microsoft Print to PDF",
                print_queue_kinds::OTHER,
                "PORTPROMPT:",
                None,
            ),
            q(
                "Kitchen",
                print_queue_kinds::TCP,
                "IP_1",
                Some("192.168.1.50"),
            ),
        ]);
        f
    }

    fn journal(&self) -> PrintJournal {
        PrintJournal::new(&self.dir, &Logger::none()).unwrap()
    }

    fn engine_with(&self, connect: bool) -> PrintRelayEngine {
        let options = PrintRelayOptions {
            printer_subnets: vec![Some("192.168.1.0/24".into())],
            ..PrintRelayOptions::default()
        };
        let policy = PrinterTargetPolicy::create(&options, [], None).unwrap();
        let engine = PrintRelayEngine::new(
            PrintRelayEngineParts {
                journal: self.journal(),
                policy,
                printer: self.printer.clone(),
                events: self.events.clone(),
                clock: self.clock.clone(),
                spooler: self.spooler.clone(),
                spooler_queues: self.queues.clone(),
            },
            &Logger::none(),
        );
        if connect {
            engine.connect(Some(self.link.clone()));
        }
        engine
    }

    fn engine(&self) -> PrintRelayEngine {
        self.engine_with(true)
    }

    fn journal_files(&self) -> usize {
        std::fs::read_dir(&self.dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".json")
            })
            .count()
    }

    fn entry(job: &PrintRelayJob, state: &str) -> PrintJournalEntry {
        PrintJournalEntry {
            job_id: job.job_id,
            rvc_no: job.rvc_no,
            host: job.host.clone(),
            port: job.port,
            expires_at: job.expires_at,
            attempts: 1,
            state: state.into(),
            payload: Some(job.payload.clone()),
            ok: false,
            error: None,
            retryable: false,
        }
    }
}

fn job_full(host: &str, port: i32, payload: &[u8], expires: &str) -> PrintRelayJob {
    PrintRelayJob {
        job_id: Uuid::new_v4(),
        rvc_no: 3,
        host: host.into(),
        port,
        payload: payload.to_vec(),
        expires_at: at(expires),
        attempts: 1,
    }
}

const IN_5_MIN: &str = "2026-11-15T08:05:00+00:00";

fn job() -> PrintRelayJob {
    job_full("192.168.1.50", 9100, &[0x1B, 0x40], IN_5_MIN)
}

fn spooler_job(host: &str) -> PrintRelayJob {
    job_full(host, 9100, &[0x1B, 0x40], IN_5_MIN)
}

#[test]
fn accept_prints_acks_ok_and_clears_the_journal() {
    let f = Fixture::new();
    let engine = f.engine();
    let job = job();
    engine.accept(&job);
    engine.drain();
    let printed = f.printer.printed.lock().unwrap().clone();
    assert_eq!(printed.len(), 1);
    assert_eq!(printed[0].0, "192.168.1.50:9100".parse().unwrap());
    assert_eq!(f.link.acks(), [(job.job_id, true, None, false)]);
    assert_eq!(f.journal_files(), 0);
    assert_eq!(engine.inflight(), 0);
}

#[test]
fn accept_ignores_a_job_pushed_again_after_it_printed() {
    let f = Fixture::new();
    let engine = f.engine();
    let job = job();
    engine.accept(&job);
    engine.drain();
    engine.accept(&job);
    engine.drain();
    assert_eq!(f.printer.printed.lock().unwrap().len(), 1);
    assert_eq!(f.link.acks().len(), 1);
}

#[test]
fn accept_refuses_a_target_outside_the_gate_without_printing() {
    let f = Fixture::new();
    let engine = f.engine();
    engine.accept(&job_full("8.8.8.8", 9100, &[0x1B, 0x40], IN_5_MIN));
    engine.drain();
    assert!(f.printer.printed.lock().unwrap().is_empty());
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(!acks[0].1 && !acks[0].3);
    assert!(
        acks[0]
            .2
            .as_deref()
            .unwrap()
            .starts_with("printer target refused")
    );
    assert!(f.events.codes().contains(&"PRINT_JOB_FAILED".to_owned()));
}

#[test]
fn accept_acks_an_invalid_or_expired_job_as_failed() {
    let f = Fixture::new();
    let engine = f.engine();
    engine.accept(&job_full("192.168.1.50", 9100, &[], IN_5_MIN));
    engine.accept(&job_full(
        "192.168.1.50",
        9100,
        &[1],
        "2026-11-15T07:59:59+00:00",
    ));
    engine.drain();
    assert!(f.printer.printed.lock().unwrap().is_empty());
    let acks = f.link.acks();
    let mut errors: Vec<String> = acks.iter().map(|a| a.2.clone().unwrap()).collect();
    errors.sort();
    let mut expected = vec![
        "invalid print job: payload is empty".to_owned(),
        EXPIRED.to_owned(),
    ];
    expected.sort();
    assert_eq!(errors, expected);
    assert!(acks.iter().all(|a| !a.1 && !a.3));
}

#[test]
fn print_not_sent_is_retryable() {
    let f = Fixture::new();
    *f.printer.fail.lock().unwrap() = Some(PrintSendError::new("connection refused", true));
    let engine = f.engine();
    engine.accept(&job());
    engine.drain();
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(acks[0].3);
    assert_eq!(acks[0].2.as_deref(), Some("connection refused"));
}

#[test]
fn print_a_failure_part_way_is_in_doubt_and_never_retried() {
    let f = Fixture::new();
    *f.printer.fail.lock().unwrap() = Some(PrintSendError::new("reset", false));
    let engine = f.engine();
    engine.accept(&job());
    engine.drain();
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(!acks[0].3);
    assert_eq!(acks[0].2.as_deref(), Some("outcome unknown: reset"));
}

#[test]
fn recover_a_job_left_printing_is_failed_closed_and_never_reprinted() {
    let f = Fixture::new();
    let job = job();
    f.journal()
        .write(&Fixture::entry(&job, print_job_states::PRINTING))
        .unwrap();
    let engine = f.engine();
    engine.recover().unwrap();
    engine.drain();
    assert!(f.printer.printed.lock().unwrap().is_empty());
    assert_eq!(
        f.link.acks(),
        [(
            job.job_id,
            false,
            Some(STOPPED_WHILE_PRINTING.to_owned()),
            false
        )]
    );
    assert_eq!(f.journal_files(), 0);
}

#[test]
fn recover_a_received_job_is_printed_and_an_expired_one_is_not() {
    let f = Fixture::new();
    let fresh = job();
    let stale = job_full("192.168.1.50", 9100, &[1], "2026-11-15T07:59:00+00:00");
    let journal = f.journal();
    for j in [&fresh, &stale] {
        journal
            .write(&Fixture::entry(j, print_job_states::RECEIVED))
            .unwrap();
    }
    let engine = f.engine();
    engine.recover().unwrap();
    engine.drain();
    assert_eq!(f.printer.printed.lock().unwrap().len(), 1);
    let acks = f.link.acks();
    assert!(acks.contains(&(fresh.job_id, true, None, false)));
    assert!(acks.contains(&(stale.job_id, false, Some(EXPIRED.to_owned()), false)));
}

#[test]
fn recover_reports_corrupt_journal_files() {
    let f = Fixture::new();
    std::fs::write(f.dir.join(format!("{}.json", Uuid::new_v4())), "{").unwrap();
    f.engine().recover().unwrap();
    assert!(
        f.events
            .codes()
            .contains(&"PRINT_JOURNAL_CORRUPT".to_owned())
    );
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ack_is_kept_on_disk_while_the_cloud_is_unreachable_and_sent_after_reconnect() {
    let f = Fixture::new();
    *f.link.fail.lock().unwrap() = Some("offline".into());
    let engine = f.engine();
    let job = job();
    engine.accept(&job);
    engine.drain();
    assert_eq!(
        f.journal_files(),
        1,
        "the decided outcome waits on disk for its ack"
    );

    *f.link.fail.lock().unwrap() = None;
    let ctx = Ctx::background().with_timeout(Duration::from_secs(10));
    let (e, c) = (engine.clone(), ctx.clone());
    let lp = std::thread::spawn(move || e.run_ack_loop(&c));
    engine.connect(Some(f.link.clone()));
    wait_until(|| !f.link.acks().is_empty());
    ctx.cancel();
    lp.join().unwrap();

    assert_eq!(f.link.acks(), [(job.job_id, true, None, false)]);
    assert_eq!(f.journal_files(), 0);
    assert_eq!(
        f.printer.printed.lock().unwrap().len(),
        1,
        "an ack retry never prints again"
    );
}

#[test]
fn ack_is_abandoned_an_hour_past_expiry() {
    let f = Fixture::new();
    *f.link.fail.lock().unwrap() = Some("offline".into());
    let engine = f.engine();
    engine.accept(&job());
    engine.drain();

    f.clock
        .set(Timestamp::parse_iso("2026-11-15T10:00:00+00:00").unwrap());
    let ctx = Ctx::background().with_timeout(Duration::from_secs(10));
    let (e, c) = (engine.clone(), ctx.clone());
    let lp = std::thread::spawn(move || e.run_ack_loop(&c));
    engine.kick();
    wait_until(|| f.journal_files() == 0);
    ctx.cancel();
    lp.join().unwrap();

    assert_eq!(f.journal_files(), 0);
    assert!(f.events.codes().contains(&"PRINT_ACK_ABANDONED".to_owned()));
}

#[test]
fn invalid_names_the_problem() {
    for (host, port, expected) in [
        ("", 9100, "printer host is empty"),
        ("h", 0, "printer port is out of range"),
    ] {
        let j = job_full(host, port, &[1], IN_5_MIN);
        assert_eq!(invalid(&j), Some(expected));
    }
}

// --- PrintRelayEngineSpoolerTests (ADR 0105) ---

#[test]
fn spooler_accept_prints_to_the_exact_reported_queue_name() {
    let f = Fixture::with_queues();
    let engine = f.engine();
    let job = spooler_job("spooler:pos-80 cashier");
    engine.accept(&job);
    engine.drain();
    let printed = f.spooler.printed.lock().unwrap().clone();
    assert_eq!(printed.len(), 1);
    assert_eq!(printed[0].0, "POS-80 Cashier");
    assert_eq!(printed[0].1, format!("PDA print {}", job.job_id));
    assert_eq!(printed[0].2, job.payload);
    assert!(f.printer.printed.lock().unwrap().is_empty());
    assert_eq!(f.link.acks(), [(job.job_id, true, None, false)]);
    assert_eq!(f.journal_files(), 0);
}

#[test]
fn spooler_accept_refuses_a_queue_that_was_not_reported_as_usb_or_wsd() {
    for host in [
        "spooler:Microsoft Print to PDF",
        "spooler:Kitchen",
        "spooler:\\\\office\\HP LaserJet",
        "spooler:",
    ] {
        let f = Fixture::with_queues();
        let engine = f.engine();
        engine.accept(&spooler_job(host));
        engine.drain();
        assert!(f.spooler.printed.lock().unwrap().is_empty(), "{host}");
        assert!(f.printer.printed.lock().unwrap().is_empty(), "{host}");
        let acks = f.link.acks();
        assert_eq!(acks.len(), 1, "{host}");
        assert!(!acks[0].1 && !acks[0].3, "{host}");
        assert!(
            acks[0]
                .2
                .as_deref()
                .unwrap()
                .starts_with("printer target refused"),
            "{host}"
        );
    }
}

#[test]
fn spooler_accept_refuses_every_spooler_job_before_discovery_ran() {
    let f = Fixture::new();
    let engine = f.engine();
    engine.accept(&spooler_job("spooler:pos-80 cashier"));
    engine.drain();
    assert!(f.spooler.printed.lock().unwrap().is_empty());
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(acks[0].2.as_deref().unwrap().contains("(0 reported)"));
}

#[test]
fn spooler_print_not_sent_is_retryable() {
    let f = Fixture::with_queues();
    *f.spooler.fail.lock().unwrap() = Some(spooler_error(
        "spooler did not accept the queue in time",
        true,
        0,
        false,
    ));
    let engine = f.engine();
    engine.accept(&spooler_job("spooler:pos-80 cashier"));
    engine.drain();
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(acks[0].3);
    assert_eq!(
        acks[0].2.as_deref(),
        Some("spooler did not accept the queue in time")
    );
}

#[test]
fn spooler_print_a_permanent_refusal_fails_at_once_and_raises_the_access_event() {
    let f = Fixture::with_queues();
    *f.spooler.fail.lock().unwrap() = Some(spooler_error(
        "could not be opened: Win32 error 5",
        true,
        win32::ACCESS_DENIED,
        true,
    ));
    let engine = f.engine();
    let job = spooler_job("spooler:pos-80 cashier");
    engine.accept(&job);
    engine.drain();
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(!acks[0].3);
    let reported = f.events.reported.lock().unwrap().clone();
    let problems: Vec<_> = reported
        .iter()
        .filter(|r| r.1 == "PRINT_SPOOLER_ACCESS_DENIED")
        .collect();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].0, "error");
    let detail = problems[0].2.as_ref().unwrap();
    assert_eq!(
        detail.get("win32_error").and_then(dl_json::Value::as_i64),
        Some(5)
    );
    assert_eq!(
        detail.get("target").and_then(dl_json::Value::as_str),
        Some(job.host.as_str())
    );
    assert!(f.events.codes().contains(&"PRINT_JOB_FAILED".to_owned()));
}

#[test]
fn spooler_print_a_failure_after_the_first_write_is_in_doubt_and_never_retried() {
    let f = Fixture::with_queues();
    *f.spooler.fail.lock().unwrap() = Some(spooler_error(
        "writing failed after 0 of 2 bytes: Win32 error 31",
        false,
        31,
        false,
    ));
    let engine = f.engine();
    engine.accept(&spooler_job("spooler:pos-80 cashier"));
    engine.drain();
    let acks = f.link.acks();
    assert_eq!(acks.len(), 1);
    assert!(!acks[0].3);
    assert!(
        acks[0]
            .2
            .as_deref()
            .unwrap()
            .starts_with("outcome unknown: ")
    );
}

#[test]
fn spooler_recover_a_spooler_job_left_printing_is_never_reprinted() {
    let f = Fixture::with_queues();
    let job = spooler_job("spooler:pos-80 cashier");
    f.journal()
        .write(&Fixture::entry(&job, print_job_states::PRINTING))
        .unwrap();
    let engine = f.engine();
    engine.recover().unwrap();
    engine.drain();
    assert!(f.spooler.printed.lock().unwrap().is_empty());
    assert_eq!(
        f.link.acks(),
        [(
            job.job_id,
            false,
            Some(STOPPED_WHILE_PRINTING.to_owned()),
            false
        )]
    );
}

#[test]
fn a_disconnected_engine_keeps_the_outcome_until_a_link_connects() {
    let f = Fixture::new();
    let engine = f.engine_with(false);
    engine.accept(&job());
    engine.drain();
    assert!(f.link.acks().is_empty());
    assert_eq!(f.journal_files(), 1);
    assert_eq!(engine.inflight(), 1);
}
