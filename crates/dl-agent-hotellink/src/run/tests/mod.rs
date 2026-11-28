//! Port of Go `run_test.go`, `supervise_test.go` and the start-of-day regression: a fake
//! hub on a local socket and the reconcile fake repository extended with the mapping and
//! availability calls (`fullFakeRepo`).

mod audit;
mod jobs;
mod loops;

use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dl_json::Value;
use dl_time::{Date, OffsetDateTime};

use super::{Parts, Runner, Timings};
use crate::error::{Error, Kind, Result};
use crate::hubclient::HubClient;
use crate::jobmodel::{AvailRow, Job, MappingCmd, MappingRow};
use crate::jobqueue::FileQueue;
use crate::jobqueue::tests::temp_dir;
use crate::pms::FullRepo;
use crate::reconcile::Clock;
use crate::reconcile::tests::FakeRepo;
use crate::testhub::{FakeHub, Req};

impl FullRepo for FakeRepo {
    fn audit_folios(&self, q: &crate::pms::AuditQuery) -> Result<crate::pms::AuditResult> {
        let mut st = self.st();
        st.audit_queries.push(q.clone());
        match &st.audit_result {
            Some(r) => Ok(r.clone()),
            None => Err(Error::other("this agent build cannot read the PMS audit")),
        }
    }
    fn test_connection(&self) -> Result<()> {
        let mut st = self.st();
        if st.sql_down_calls > 0 {
            st.sql_down_calls -= 1;
            return Err(Error::new(Kind::PmsDown, "fake: SQL down"));
        }
        Ok(())
    }
    fn ensure_room_type_mapped(&self) -> Result<i64> {
        Ok(0)
    }
    fn get_room_type_mapped(&self) -> Result<Vec<MappingRow>> {
        Ok(self.st().mapping.clone())
    }
    fn get_availability(&self, _from: Date, _days: i64) -> Result<Vec<AvailRow>> {
        Ok(self.st().avail.clone())
    }
    fn insert_mapping(&self, m: &MappingRow) -> Result<()> {
        let mut st = self.st();
        if let Some(e) = &st.mapping_write_err {
            return Err(e.clone());
        }
        st.mapping_inserted.push(m.clone());
        Ok(())
    }
    fn update_mapping(&self, orig: &MappingRow, m: &MappingRow) -> Result<()> {
        let mut st = self.st();
        if let Some(e) = &st.mapping_write_err {
            return Err(e.clone());
        }
        st.mapping_updated.push((orig.clone(), m.clone()));
        Ok(())
    }
    fn delete_mapping(&self, orig: &MappingRow) -> Result<()> {
        let mut st = self.st();
        if let Some(e) = &st.mapping_write_err {
            return Err(e.clone());
        }
        st.mapping_deleted.push(orig.clone());
        Ok(())
    }
}

/// `ackRecord`.
#[derive(Debug, Clone)]
pub(crate) struct Ack {
    pub job_id: String,
    pub ok: bool,
    pub error: String,
    pub pms_action: String,
    pub needs_manual: bool,
}

/// `fakeHub`'s fields.
#[derive(Default)]
pub(crate) struct HubSt {
    pub jobs: Vec<Job>,
    pub want_mapping: bool,
    pub mapping_cmds: Vec<MappingCmd>,
    pub map_cmd_acks: Vec<(String, bool, String)>,
    pub fail_polls: usize,
    pub supersede: bool,
    pub unauthorized: bool,
    pub poll_count: usize,
    pub acks: Vec<Ack>,
    /// (room type codes, hash) per push.
    pub mappings: Vec<(Vec<String>, String)>,
    pub availability: Vec<Vec<AvailRow>>,
    pub inv_sync_headers: Vec<String>,
    pub config: String,
    /// `pmsAudits` handed out with the next poll (raw JSON objects).
    pub audits: Vec<String>,
}

/// `newFakeHub`.
pub(crate) struct Hub {
    pub fake: FakeHub,
    st: Arc<Mutex<HubSt>>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn segment(path: &str, prefix: &str) -> String {
    path.trim_start_matches(prefix)
        .trim_end_matches("/ack")
        .to_owned()
}

fn handle(st: &Mutex<HubSt>, r: &Req) -> (u16, String) {
    let mut st = st.lock().unwrap_or_else(|p| p.into_inner());
    if st.supersede {
        return (403, "agent superseded".into());
    }
    if st.unauthorized {
        return (401, "bad token".into());
    }
    let body = dl_json::parse(&r.body).unwrap_or(Value::Null);
    let path = r.path.split('?').next().unwrap_or("");
    if r.method == "GET" && path == "/api/agent/jobs" {
        st.poll_count += 1;
        if st.poll_count <= st.fail_polls {
            return (500, "boom".into());
        }
        let jobs: Vec<String> = std::mem::take(&mut st.jobs)
            .iter()
            .map(Job::to_json)
            .collect();
        let cmds: Vec<String> = std::mem::take(&mut st.mapping_cmds)
            .iter()
            .map(MappingCmd::to_json)
            .collect();
        let want = std::mem::take(&mut st.want_mapping);
        let audits = std::mem::take(&mut st.audits);
        return (
            200,
            format!(
                r#"{{"jobs":[{}],"wantMapping":{want},"mappingCmds":[{}],"pmsAudits":[{}]}}"#,
                jobs.join(","),
                cmds.join(","),
                audits.join(",")
            ),
        );
    }
    if path.starts_with("/api/agent/mapping-cmds/") {
        let id = segment(path, "/api/agent/mapping-cmds/");
        st.map_cmd_acks
            .push((id, b(&body, "ok"), s(&body, "error")));
    } else if path.starts_with("/api/agent/jobs/") {
        let res = body.get("pmsResult").filter(|v| !v.is_null());
        st.acks.push(Ack {
            job_id: segment(path, "/api/agent/jobs/"),
            ok: b(&body, "ok"),
            error: s(&body, "error"),
            pms_action: res.map(|v| s(v, "action")).unwrap_or_default(),
            needs_manual: res.is_some_and(|v| b(v, "needsManual")),
        });
    } else if path == "/api/agent/mapping" {
        let codes = arr(&body, "rows")
            .iter()
            .map(|r| s(r, "roomTypeCode"))
            .collect();
        st.mappings.push((codes, s(&body, "hash")));
    } else if path == "/api/agent/availability" {
        let rows = arr(&body, "rows")
            .iter()
            .map(|r| AvailRow {
                room_type_code: s(r, "roomTypeCode"),
                date: s(r, "date"),
                available: r.get("available").and_then(Value::as_i64).unwrap_or(0),
            })
            .collect();
        st.availability.push(rows);
        st.inv_sync_headers
            .push(r.header("x-inventory-sync").unwrap_or("").to_owned());
        return (200, r#"{"pushed":1}"#.into());
    } else if path == "/api/agent/config" {
        return (200, st.config.clone());
    }
    (200, r#"{"ok":true}"#.into())
}

impl Hub {
    pub fn new() -> Hub {
        let st = Arc::new(Mutex::new(HubSt {
            config: r#"{"enabled":true,"availabilityIntervalMin":30,"availabilityDays":60}"#.into(),
            ..HubSt::default()
        }));
        let h = st.clone();
        Hub {
            fake: FakeHub::start(move |r| handle(&h, r)),
            st,
        }
    }

    pub fn st(&self) -> MutexGuard<'_, HubSt> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A runner thread stopped and joined on drop.
pub(super) struct Running {
    pub r: Arc<Runner>,
    h: Option<JoinHandle<()>>,
}

impl Running {
    /// Waits for `run` to return by itself.
    pub fn finished_within(&mut self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        while Instant::now() < deadline {
            if self.h.as_ref().is_some_and(JoinHandle::is_finished) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.r.stop();
        if let Some(h) = self.h.take()
            && h.join().is_err()
            && !std::thread::panicking()
        {
            panic!("runner thread panicked");
        }
    }
}

pub(crate) fn fast() -> Timings {
    Timings {
        poll_wait_sec: 0,
        backoff_start: Duration::from_millis(1),
        backoff_max: Duration::from_millis(1),
        heartbeat: Duration::from_millis(20),
        cfg_refresh: Duration::from_secs(300),
        supervise: Duration::from_millis(1),
        avail_interval: |_| Duration::from_millis(10),
        ..Timings::default()
    }
}

/// `newTestRunner` (a file queue in a fresh directory).
pub(super) fn runner(url: &str, repo: &Arc<FakeRepo>) -> Arc<Runner> {
    runner_at(
        url,
        repo,
        &temp_dir("run"),
        Arc::new(OffsetDateTime::now_utc),
    )
}

pub(super) fn runner_at(
    url: &str,
    repo: &Arc<FakeRepo>,
    dir: &std::path::Path,
    now: Clock,
) -> Arc<Runner> {
    let parts = Parts {
        hub: HubClient::new(url, "h1", "tok", "test", ""),
        repo: repo.clone(),
        queue: FileQueue::open(&dir.join("queue.jsonl")).expect("queue"),
        state_dir: dir.to_owned(),
        legacy_policy_dir: None,
    };
    Arc::new(Runner::with(parts, fast(), now))
}

pub(super) fn start(r: Arc<Runner>) -> Running {
    let me = r.clone();
    let h = std::thread::spawn(move || me.run());
    Running { r, h: Some(h) }
}

/// `waitUntil`: polls for five seconds.
pub(super) fn wait_until(cond: impl Fn() -> bool, msg: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{msg}");
}

/// A hub whose job poll stays open (up to 30 s, until `release`); everything else answers
/// `{"ok":true}` at once. Returns the hub, the poll count and the release flag.
pub(crate) fn holding_hub() -> (
    FakeHub,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let polls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let (p, r) = (polls.clone(), release.clone());
    let hub = FakeHub::start(move |req| {
        if req.method == "GET" && req.path.starts_with("/api/agent/jobs?") {
            p.fetch_add(1, Ordering::SeqCst);
            let end = Instant::now() + Duration::from_secs(30);
            while !r.load(Ordering::SeqCst) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            return (200, r#"{"jobs":[]}"#.into());
        }
        (200, r#"{"ok":true}"#.into())
    });
    (hub, polls, release)
}
