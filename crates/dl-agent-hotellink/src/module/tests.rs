//! The module against the fake hub and the fake repository: poll → running, 401 →
//! degraded, the Go queue import and a prompt stop with a job in flight.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_agent::supervisor::{ModuleContext, ModuleState};
use dl_json::Value;

use super::{LEGACY_MARKER, NAME, QUEUE_FILE, Wiring, run_configured, usable};
use crate::config::{self, LocalConfig};
use crate::jobqueue::FileQueue;
use crate::jobqueue::tests::temp_dir;
use crate::reconcile::tests::{FakeRepo, day, save_job};
use crate::run::tests::{Hub, fast};

fn write_config(dir: &Path, hub_url: &str) -> PathBuf {
    let path = dir.join("config.json");
    let body = format!(
        r#"{{"hubUrl":"{hub_url}","hotelId":"h1","token":"tok","instanceId":"i-1",
            "sql":{{"server":".","database":"SMILE_FO","trustedConnection":true}}}}"#
    );
    std::fs::write(&path, body).expect("write config");
    path
}

fn load(path: &Path) -> LocalConfig {
    config::load(path).expect("load").expect("config")
}

struct Started {
    ctx: ModuleContext,
    h: Option<JoinHandle<Result<(), dl_agent::Error>>>,
}

impl Started {
    fn stop(&mut self) -> Duration {
        let t = Instant::now();
        self.ctx.stop().stop();
        if let Some(h) = self.h.take() {
            h.join().expect("module thread").expect("module result");
        }
        t.elapsed()
    }
}

impl Drop for Started {
    fn drop(&mut self) {
        if self.h.is_some() && !std::thread::panicking() {
            self.stop();
        }
    }
}

fn start(dir: &Path, hub_url: &str, repo: &Arc<FakeRepo>, grace: Duration) -> Started {
    let path = write_config(dir, hub_url);
    let cfg = load(&path);
    let ctx = ModuleContext::standalone(NAME, Value::Null, dir.join("data"));
    let c = ctx.clone();
    let repo = repo.clone();
    let h = std::thread::spawn(move || {
        let w = Wiring {
            repo,
            timings: fast(),
            grace,
        };
        run_configured(&c, &cfg, &path, w)
    });
    Started { ctx, h: Some(h) }
}

fn wait_for(ctx: &ModuleContext, what: &str, cond: impl Fn(&ModuleContext) -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if cond(ctx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}: {:?}", ctx.status());
}

fn metric(ctx: &ModuleContext, k: &str) -> i64 {
    ctx.status()
        .metrics
        .get(k)
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

#[test]
fn polls_applies_acks_and_runs() {
    let hub = Hub::new();
    hub.st().jobs.push(save_job());
    let repo = FakeRepo::new();
    repo.st().hotel_date = Some(day(2026, 6, 1));
    let dir = temp_dir("mod-run");
    let m = start(&dir, &hub.fake.url, &repo, Duration::from_secs(5));
    wait_for(&m.ctx, "running", |c| {
        c.status().state == ModuleState::Running
    });
    wait_for(&m.ctx, "ack metric", |c| metric(c, "jobs_acked") == 1);
    assert_eq!(hub.st().acks.len(), 1);
    assert!(hub.st().acks[0].ok);
    assert_eq!(repo.st().inserted.len(), 1);
    assert_eq!(metric(&m.ctx, "jobs_polled"), 1);
    assert_eq!(metric(&m.ctx, "queue_depth"), 0);
    let ua = hub
        .fake
        .reqs
        .lock()
        .expect("reqs")
        .iter()
        .find_map(|r| r.header("x-agent-version").map(str::to_owned));
    assert_eq!(ua.as_deref(), Some(super::agent_version_header().as_str()));
    assert!(super::agent_version_header().starts_with("dl-agent-"));
}

#[test]
fn unauthorized_hub_degrades_and_reports() {
    let hub = Hub::new();
    hub.st().unauthorized = true;
    let repo = FakeRepo::new();
    let dir = temp_dir("mod-401");
    let m = start(&dir, &hub.fake.url, &repo, Duration::from_secs(5));
    wait_for(&m.ctx, "degraded", |c| {
        let s = c.status();
        s.state == ModuleState::Degraded && s.detail.contains("401")
    });
}

#[test]
fn stops_promptly_with_a_job_in_flight_and_keeps_it_queued() {
    let hub = Hub::new();
    hub.st().jobs.push(save_job());
    let repo = FakeRepo::new();
    repo.st().hotel_date = Some(day(2026, 6, 1));
    repo.st().insert_delay = Duration::from_secs(3);
    let dir = temp_dir("mod-stop");
    let mut m = start(&dir, &hub.fake.url, &repo, Duration::from_millis(300));
    wait_for(&m.ctx, "job queued", |c| metric(c, "jobs_polled") == 1);
    let took = m.stop();
    assert!(took < Duration::from_secs(2), "stop took {took:?}");
    assert!(hub.st().acks.is_empty(), "no ack before the job finished");
    let q = FileQueue::open(&dir.join("data").join(NAME).join(QUEUE_FILE)).expect("queue");
    assert_eq!(
        q.pending().len(),
        1,
        "the unfinished job stays in the durable queue"
    );
}

#[test]
fn imports_the_go_queue_once() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().hotel_date = Some(day(2026, 6, 1));
    let dir = temp_dir("mod-legacy");
    {
        let go = FileQueue::open(&dir.join(QUEUE_FILE)).expect("go queue");
        go.enqueue(&save_job()).expect("enqueue");
    }
    let m = start(&dir, &hub.fake.url, &repo, Duration::from_secs(5));
    wait_for(&m.ctx, "imported job acked", |c| {
        metric(c, "jobs_acked") == 1
    });
    assert_eq!(hub.st().acks[0].job_id, save_job().job_id);
    assert!(dir.join("data").join(NAME).join(LEGACY_MARKER).exists());
}

#[test]
fn not_configured_reasons() {
    let dir = temp_dir("mod-cfg");
    let missing = dir.join("none.json");
    assert!(
        usable(&missing)
            .err()
            .unwrap_or_default()
            .starts_with("not configured: no config")
    );
    let path = dir.join("config.json");
    std::fs::write(&path, r#"{"hubUrl":"http://x","hotelId":"h1","token":""}"#).expect("write");
    let e = usable(&path).err().unwrap_or_default();
    assert!(e.starts_with("not configured: config has no token"), "{e}");
}
