//! The write-back worker (MMEI RT-0785): long-polls the hub for write-back commands and
//! applies each batch as soon as it arrives, so an issued invoice number reaches SMILE in
//! seconds instead of at the next collect cycle.
//!
//! It shares the collector's [`Runner`] and locks it only to apply (SQL, outbox, ack);
//! the long poll itself runs on its own hub client without the lock. Against a hub built
//! before RT-0785 (no `X-MMEI-Long-Poll` on the answer) it polls once per interval, the
//! old cadence.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dl_winsvc::StopSignal;

use super::collector::{Shared, exclusive};
use crate::log::{self, V};
use crate::platform::{ApiError, Platform};
use crate::runner::Runner;

/// How long each poll asks the hub to wait. Short enough that a stop is not held long
/// by a call in flight (a blocked HTTP call cannot be interrupted).
const LONG_POLL: Duration = Duration::from_secs(10);
/// Extra time over the wait for the answer to arrive.
const POLL_SLACK: Duration = Duration::from_secs(10);
/// The shortest gap between two empty polls, so a hub that answers a waited poll at once
/// can never spin the loop; also the first retry delay after a failed poll.
const MIN_GAP: Duration = Duration::from_secs(1);

/// One write-back worker per process: one left behind by a stop that timed out ends
/// before a new one polls.
static WRITEBACK_BUSY: Mutex<()> = Mutex::new(());

/// The write-back loop.
pub struct WritebackWorker {
    runner: Arc<Mutex<Runner>>,
    cli: Platform,
    interval: Duration,
    stop: StopSignal,
    shared: Arc<Shared>,
}

impl WritebackWorker {
    /// A worker polling with `cli` and applying through `runner`; `interval` is the
    /// collector's, used against an older hub and to back off.
    pub fn new(
        runner: Arc<Mutex<Runner>>,
        cli: Platform,
        interval: Duration,
        stop: StopSignal,
        shared: Arc<Shared>,
    ) -> WritebackWorker {
        WritebackWorker {
            runner,
            cli,
            interval,
            stop,
            shared,
        }
    }

    /// Runs until stopped or replaced.
    pub fn run(self) {
        let Some(_one) = exclusive(&WRITEBACK_BUSY, &self.stop) else {
            return;
        };
        let mut backoff = MIN_GAP;
        while !self.stop.is_stopped() {
            let started = Instant::now();
            match self
                .cli
                .poll_wait(LONG_POLL, started + LONG_POLL + POLL_SLACK)
            {
                Err(e) => {
                    let replaced = e == ApiError::DeviceReplaced;
                    let msg = e.to_string();
                    self.runner
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .note_writeback_poll_error(e);
                    if replaced {
                        self.shared.replaced.store(true, Ordering::Relaxed);
                        return;
                    }
                    log::warn(
                        "writeback poll failed",
                        &[
                            ("error", V::S(&msg)),
                            ("retry_in_s", V::I(backoff.as_secs() as i64)),
                        ],
                    );
                    if self.stop.wait_timeout(backoff) {
                        return;
                    }
                    backoff = (backoff * 2).min(self.interval.max(MIN_GAP));
                }
                Ok((cmds, long_poll)) => {
                    backoff = MIN_GAP;
                    if !cmds.is_empty() {
                        let mut runner = self.runner.lock().unwrap_or_else(PoisonError::into_inner);
                        if self.stop.is_stopped() {
                            // A newer module may own the outbox now; the hub re-delivers.
                            return;
                        }
                        let res = runner.apply_polled_writeback(cmds);
                        drop(runner);
                        log::info(
                            "writeback applied",
                            &[
                                ("polled", V::I(res.polled)),
                                ("applied", V::I(res.applied)),
                                ("skipped", V::I(res.skipped)),
                                ("acked", V::I(res.acked)),
                                ("errors", V::I(res.writeback_errors.list.len() as i64)),
                                ("writeback_ms", V::I(res.writeback_ms)),
                            ],
                        );
                        if res.replaced() {
                            self.shared.replaced.store(true, Ordering::Relaxed);
                            return;
                        }
                        // Nothing went through (SQL down, procedure failing): the hub
                        // re-delivers after its lease, so do not hammer it meanwhile.
                        if res.acked == 0 && self.stop.wait_timeout(self.interval) {
                            return;
                        }
                    } else if !long_poll {
                        // An older hub: keep the old cadence.
                        if self.stop.wait_timeout(self.interval) {
                            return;
                        }
                    } else if started.elapsed() < MIN_GAP && self.stop.wait_timeout(MIN_GAP) {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use dl_uuid::Uuid;

    use crate::agent_config::AgentConfig;
    use crate::outbox::Outbox;
    use crate::sql::Sql;

    const CMD_ID: &str = "11111111-1111-1111-1111-111111111111";

    /// A hub: `/poll` answers `poll_body(n)` for the n-th poll (with the long-poll header
    /// when `long_poll`); `/writeback:ack` is recorded on `acks`; polls are counted.
    fn fake_hub(
        long_poll: bool,
        status: u16,
        poll_body: fn(usize) -> String,
    ) -> (String, Arc<AtomicUsize>, mpsc::Receiver<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let polls = Arc::new(AtomicUsize::new(0));
        let n = polls.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => req.extend_from_slice(&buf[..k]),
                    }
                }
                let head = String::from_utf8_lossy(&req).into_owned();
                let line = head.lines().next().unwrap_or_default().to_owned();
                if let Some(len) = head
                    .to_ascii_lowercase()
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").map(str::to_owned))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                {
                    let have = req.len() - head.find("\r\n\r\n").map_or(req.len(), |i| i + 4);
                    let mut rest = vec![0u8; len.saturating_sub(have)];
                    let _ = s.read_exact(&mut rest);
                }
                let body = if line.contains("/poll") {
                    let k = n.fetch_add(1, Ordering::SeqCst);
                    poll_body(k)
                } else {
                    let _ = tx.send(line);
                    "{}".to_owned()
                };
                let extra = if long_poll {
                    "X-MMEI-Long-Poll: 10\r\n"
                } else {
                    ""
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, polls, rx)
    }

    fn worker(url: &str, interval: Duration, stop: &StopSignal) -> (WritebackWorker, Arc<Shared>) {
        let dir = std::env::temp_dir().join(format!("dl-einvoice-wb-{}", Uuid::new_v4()));
        let mut outbox = Outbox::open(&dir.join("outbox.json")).unwrap();
        // Already applied: the worker acks it without touching SQL.
        outbox.mark_applied(CMD_ID).unwrap();
        let cfg = AgentConfig::default();
        let sql = Sql::new("", "", cfg.sp_names());
        let cli = Platform::new(url, "k", Uuid::new_v4());
        let mut runner = Runner::new(sql, cli, outbox, cfg);
        runner.hand_writeback_to_worker();
        let shared = Arc::new(Shared::default());
        let w = WritebackWorker::new(
            Arc::new(Mutex::new(runner)),
            Platform::new(url, "k", Uuid::new_v4()),
            interval,
            stop.clone(),
            shared.clone(),
        );
        (w, shared)
    }

    #[test]
    fn a_polled_command_is_acked_at_once_and_reported_to_the_cycle() {
        fn body(k: usize) -> String {
            if k == 0 {
                format!(
                    r#"{{"commands":[{{"command_id":"{CMD_ID}","type":"SET_INVOICE_NUMBER","source_ref":"FO:1","invoice_number":"7","ky_hieu":"1C26TAA"}}]}}"#
                )
            } else {
                std::thread::sleep(Duration::from_millis(200));
                r#"{"commands":[]}"#.to_owned()
            }
        }
        let (url, _polls, acks) = fake_hub(true, 200, body);
        let stop = StopSignal::new();
        let (w, _) = worker(&url, Duration::from_secs(30), &stop);
        let runner = w.runner.clone();
        let t = std::thread::spawn(move || w.run());
        let line = acks.recv_timeout(Duration::from_secs(5)).expect("no ack");
        assert!(
            line.starts_with("POST /api/v1/agent/writeback:ack"),
            "{line}"
        );
        stop.stop();
        t.join().unwrap();
        let res = runner.lock().unwrap().take_writeback_pending();
        assert_eq!((res.polled, res.skipped, res.acked), (1, 1, 1));
    }

    #[test]
    fn against_an_old_hub_it_keeps_the_interval() {
        fn body(_: usize) -> String {
            r#"{"commands":[]}"#.to_owned()
        }
        let (url, polls, _) = fake_hub(false, 200, body);
        let stop = StopSignal::new();
        let (w, _) = worker(&url, Duration::from_millis(250), &stop);
        let t = std::thread::spawn(move || w.run());
        std::thread::sleep(Duration::from_millis(1100));
        stop.stop();
        t.join().unwrap();
        let n = polls.load(Ordering::SeqCst);
        assert!((3..=6).contains(&n), "polls in 1.1 s at 250 ms = {n}");
    }

    #[test]
    fn device_replaced_stops_the_worker_and_flags_the_module() {
        fn body(_: usize) -> String {
            r#"{"error":{"code":"device_replaced","message":"replaced"}}"#.to_owned()
        }
        let (url, _, _) = fake_hub(false, 401, body);
        let stop = StopSignal::new();
        let (w, shared) = worker(&url, Duration::from_secs(30), &stop);
        let t = std::thread::spawn(move || w.run());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !t.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(t.is_finished(), "worker kept running after device_replaced");
        assert!(shared.replaced.load(Ordering::Relaxed));
    }
}
