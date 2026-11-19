//! The sync and scan workers over the real `HubClient` against a fake hub on a local
//! socket and a fake SMILE: heartbeat and sync-config, the job count on an older hub (never
//! acked), the token gate, the back-off report, every scanner posting, the folio cursor
//! moving only after a 2xx, and the scans stopping on another database.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dl_agent_membership::folio::cursor::FILE_NAME as CURSOR_FILE;
use dl_agent_membership::hub::gate::TokenGate;
use dl_agent_membership::hub::{HubApi, HubClient};
use dl_agent_membership::identity::Tracker;
use dl_agent_membership::module::TOKEN_RETRY;
use dl_agent_membership::module::scan::{ScanDeps, ScanLoop};
use dl_agent_membership::module::state::{PendingReport, Shared, codes};
use dl_agent_membership::settings::Settings;
use dl_agent_membership::sync::{SyncSettings, SyncWorker};
use dl_json::json;
use support::{FakeHub, FakeSmile, HOTEL, TOKEN, TestDir};

const VERSION: &str = "dl-agent/test";

struct Rig {
    hub: FakeHub,
    smile: Arc<FakeSmile>,
    tracker: Arc<Tracker>,
    shared: Arc<Shared>,
    client: Arc<dyn HubApi>,
    dir: TestDir,
}

fn rig() -> Rig {
    let hub = FakeHub::start();
    let smile = Arc::new(FakeSmile::default());
    let client: Arc<dyn HubApi> = Arc::new(
        HubClient::new(
            &hub.url(),
            TOKEN,
            HOTEL.parse().unwrap(),
            VERSION,
            Arc::new(TokenGate::new(TOKEN_RETRY)),
        )
        .unwrap(),
    );
    Rig {
        tracker: Arc::new(Tracker::new(smile.clone())),
        shared: Arc::new(Shared::default()),
        hub,
        smile,
        client,
        dir: TestDir::new("hub"),
    }
}

impl Rig {
    fn sync(&self, pull_jobs: bool) -> SyncWorker {
        SyncWorker::new(
            self.client.clone(),
            self.smile.clone(),
            self.tracker.clone(),
            self.shared.clone(),
            SyncSettings {
                pull_jobs,
                max_jobs_per_pull: 20,
                heartbeat: Duration::from_secs(30),
                capabilities: vec!["availability".into()],
            },
            VERSION,
        )
    }

    fn scans(&self) -> ScanLoop {
        let settings = Settings::from_config(
            &json!({
                "program_data_dir": self.dir.0.display().to_string(),
                "earn_charge_codes": ["RM"],
            }),
            &self.dir.0,
        );
        let deps = ScanDeps {
            src: self.smile.clone(),
            hub: self.client.clone(),
            tracker: self.tracker.clone(),
            shared: self.shared.clone(),
        };
        ScanLoop::new(deps, &settings, HOTEL, VERSION)
    }

    fn reports(&self) -> Vec<PendingReport> {
        self.shared.take_reports()
    }
}

/// No report message or context value carries the token or the SMILE data.
fn assert_clean(reports: &[PendingReport]) {
    for r in reports {
        let all = format!("{} {:?}", r.message, r.context);
        assert!(!all.contains(TOKEN), "token in report {}", r.code);
        assert!(!all.contains("Password"), "secret in report {}", r.code);
    }
}

#[test]
fn heartbeat_and_sync_config_carry_the_token_and_the_hotel() {
    let r = rig();
    let mut sync = r.sync(false);
    sync.cycle(Instant::now());
    assert_eq!(
        r.hub.lines(),
        ["POST /api/agent/heartbeat", "GET /api/agent/sync-config"]
    );
    for req in r.hub.requests() {
        assert_eq!(
            req.header("Authorization"),
            Some(&*format!("Bearer {TOKEN}"))
        );
        assert_eq!(req.header("X-Hotel-Id"), Some(HOTEL));
    }
    let hb = &r.hub.to("/api/agent/heartbeat")[0];
    assert!(
        hb.header("Content-Type")
            .unwrap()
            .starts_with("application/json")
    );
    let body = dl_json::parse(&hb.body).unwrap();
    assert_eq!(
        body.get("AgentVersion").and_then(|v| v.as_str()),
        Some(VERSION)
    );
    assert_eq!(body.get("SmileOk").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        body.get("DbIdentity")
            .and_then(|d| d.get("Database"))
            .and_then(|v| v.as_str()),
        Some("SMILE_FO")
    );
    let cfg = r.shared.sync_config().expect("sync-config kept");
    assert!(cfg.availability_sync_enabled && cfg.room_inventory_sync_enabled);
    let health = r.shared.health();
    assert!(
        health.heartbeat_ok && health.problem().is_none(),
        "{health:?}"
    );
    assert_eq!(r.shared.stats().heartbeats, 1);
    assert!(r.reports().is_empty());

    // Inside the heartbeat interval and the sync-config refresh: nothing more is sent.
    sync.cycle(Instant::now() + Duration::from_secs(5));
    assert_eq!(r.hub.requests().len(), 2);
}

#[test]
fn an_older_hub_lists_jobs_that_are_counted_and_never_acked() {
    let r = rig();
    r.hub.reply("/api/agent/jobs/pull", 404, "");
    r.hub.reply(
        "/api/agent/jobs",
        200,
        r#"[{"JobId":"8d1c0f7e-0000-4000-8000-000000000001","Action":"CreateBooking","Attempts":1},
            {"JobId":"8d1c0f7e-0000-4000-8000-000000000002","Action":4,"Attempts":3}]"#,
    );
    let mut sync = r.sync(true);
    let t0 = Instant::now();
    sync.cycle(t0);
    sync.cycle(t0 + Duration::from_secs(5));
    let lines = r.hub.lines();
    assert_eq!(
        lines,
        [
            "POST /api/agent/jobs/pull",
            "GET /api/agent/jobs",
            "POST /api/agent/heartbeat",
            "GET /api/agent/sync-config",
            "POST /api/agent/jobs/pull",
            "GET /api/agent/jobs",
        ]
    );
    let pull = &r.hub.to("/api/agent/jobs/pull")[0];
    assert_eq!(pull.target, "/api/agent/jobs/pull?max=20");
    assert!(pull.body.is_empty());
    assert_eq!(
        r.hub.to("/api/agent/jobs")[0].target,
        "/api/agent/jobs?max=20"
    );
    let stats = r.shared.stats();
    assert_eq!((stats.jobs_seen, stats.jobs_left), (4, 2));
    // Neither executed nor acknowledged: no SMILE write path exists in the fake, and the
    // hub saw no ack, result or complete call.
    assert!(
        lines.iter().all(|l| !["ack", "result", "complete", "fail"]
            .iter()
            .any(|w| l.contains(w))),
        "{lines:?}"
    );
    assert!(r.reports().is_empty());
}

#[test]
fn a_rejected_token_closes_the_gate_and_reports_once_per_probe() {
    let r = rig();
    r.hub.reply_with(
        "/api/agent/heartbeat",
        401,
        &[("X-MM-Agent-Auth-Error", "AGENT_TOKEN_REJECTED")],
        "",
    );
    let mut sync = r.sync(true);
    let wait = sync.cycle(Instant::now());
    assert_eq!(wait, TOKEN_RETRY);
    assert!(r.shared.health().auth_rejected);
    let reports = r.reports();
    assert_eq!(
        reports.iter().map(|p| p.code).collect::<Vec<_>>(),
        [codes::AUTH_FAILED]
    );
    assert_clean(&reports);
    let sent = r.hub.requests().len();
    assert_eq!(sent, 2, "{:?}", r.hub.lines());

    // The gate is closed: the next cycles reach no hub at all.
    sync.cycle(Instant::now() + Duration::from_secs(60));
    sync.cycle(Instant::now() + Duration::from_secs(120));
    assert_eq!(r.hub.requests().len(), sent);
    assert!(r.shared.health().problem().is_some());
}

#[test]
fn a_plain_401_is_a_hub_failure_not_the_token_gate() {
    let r = rig();
    r.hub.reply("/api/agent/heartbeat", 401, "");
    let mut sync = r.sync(false);
    sync.cycle(Instant::now());
    let health = r.shared.health();
    assert!(!health.auth_rejected);
    assert_eq!(health.hub_failures, 1);
    sync.cycle(Instant::now() + Duration::from_secs(60));
    assert_eq!(
        r.hub.to("/api/agent/heartbeat").len(),
        2,
        "the gate stayed open"
    );
}

#[test]
fn the_hub_down_past_one_backoff_is_reported_without_the_token() {
    let r = rig();
    r.hub.reply("/api/agent/heartbeat", 500, "boom");
    let mut sync = r.sync(false);
    let t0 = Instant::now();
    assert_eq!(sync.cycle(t0), Duration::from_secs(5));
    assert!(
        r.reports().is_empty(),
        "one failure is inside the first back-off"
    );
    assert_eq!(
        sync.cycle(t0 + Duration::from_secs(6)),
        Duration::from_secs(10)
    );
    let reports = r.reports();
    assert_eq!(
        reports.iter().map(|p| p.code).collect::<Vec<_>>(),
        [codes::HUB_FAILED]
    );
    assert_clean(&reports);
    let problem = r.shared.health().problem().expect("degraded");
    assert!(!problem.contains(TOKEN));
    assert_eq!(r.shared.stats().heartbeat_failures, 2);

    // Back up: one good cycle clears it.
    r.hub.reply("/api/agent/heartbeat", 200, "");
    sync.cycle(t0 + Duration::from_secs(20));
    assert!(r.shared.health().problem().is_none());
}

#[test]
fn smile_down_still_heartbeats_with_smile_ok_false() {
    let r = rig();
    r.smile.set_down(true);
    let mut sync = r.sync(false);
    sync.cycle(Instant::now());
    let body = dl_json::parse(&r.hub.to("/api/agent/heartbeat")[0].body).unwrap();
    assert_eq!(body.get("SmileOk").and_then(|v| v.as_bool()), Some(false));
    assert!(r.shared.health().smile_error.is_some());
    let reports = r.reports();
    assert_eq!(
        reports.iter().map(|p| p.code).collect::<Vec<_>>(),
        [codes::SMILE_FAILED]
    );
}

#[test]
fn every_scanner_posts_to_the_hub() {
    let r = rig();
    r.smile.add_folio(1001);
    r.sync(false).cycle(Instant::now());
    let mut scans = r.scans();
    scans.turn(Instant::now());

    let availability = r.hub.to("/api/agent/availability");
    assert_eq!(availability.len(), 1, "{:?}", r.hub.lines());
    let body = dl_json::parse(&availability[0].body).unwrap();
    let lines = body.get("Lines").and_then(|v| v.as_array()).unwrap();
    let first = &lines[0];
    assert_eq!(
        first.get("RoomTypeCode").and_then(|v| v.as_str()),
        Some("DLX")
    );
    assert_eq!(first.get("RoomsSold").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(first.get("RoomsTotal").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(
        body.get("ScanComplete").and_then(|v| v.as_bool()),
        Some(true)
    );

    let rooms = dl_json::parse(&r.hub.to("/api/agent/room-inventory")[0].body).unwrap();
    assert_eq!(
        rooms.get("Rooms").and_then(|v| v.as_array()).map(Vec::len),
        Some(2)
    );
    let types = dl_json::parse(&r.hub.to("/api/agent/room-types")[0].body).unwrap();
    assert_eq!(
        types
            .get("RoomTypes")
            .and_then(|v| v.as_array())
            .map(Vec::len),
        Some(1)
    );

    let folio = r.hub.to("/api/agent/folio-observations");
    assert_eq!(folio.len(), 1);
    let body = dl_json::parse(&folio[0].body).unwrap();
    let obs = body.get("Observations").and_then(|v| v.as_array()).unwrap();
    assert_eq!(obs.len(), 1);
    assert_eq!(obs[0].get("FolioNum").and_then(|v| v.as_i64()), Some(1001));

    let stats = r.shared.stats();
    for scanner in ["availability", "rooms", "folio"] {
        let s = &stats.scans[scanner];
        assert_eq!((s.runs, s.errors), (1, 0), "{scanner}: {s:?}");
    }
    assert!(r.reports().is_empty());
    // Every scan is a read; the fake SMILE has no write to call.
    assert!(r.smile.calls.lock().unwrap().contains(&"changed_folios"));
}

#[test]
fn the_folio_cursor_moves_only_after_the_hub_took_the_batch() {
    let r = rig();
    r.smile.add_folio(1001);
    r.hub.reply("/api/agent/folio-observations", 500, "");
    r.sync(false).cycle(Instant::now());
    let mut scans = r.scans();
    let t0 = Instant::now();
    scans.turn(t0);
    let cursor = r.dir.0.join(CURSOR_FILE);
    assert!(!cursor.exists(), "a failed post must not move the cursor");
    let reports = r.reports();
    assert_eq!(
        reports.iter().map(|p| p.code).collect::<Vec<_>>(),
        [codes::SCAN_FAILED]
    );
    assert_clean(&reports);
    assert_eq!(r.shared.stats().scans["folio"].errors, 1);

    r.hub.reply(
        "/api/agent/folio-observations",
        200,
        r#"{"Accepted":1,"Earned":0,"Reversed":0,"Unchanged":0,"Items":[{"FolioNum":1001,"FolioSubNum":0,"Outcome":"Accepted"}]}"#,
    );
    scans.turn(t0 + Duration::from_secs(3600));
    assert!(cursor.exists(), "a 2xx moves the cursor");
    let posts = r.hub.to("/api/agent/folio-observations");
    assert_eq!(posts.len(), 2, "the same folio is sent again");
    assert_eq!(posts[0].body, posts[1].body);
}

#[test]
fn another_database_stops_the_scans_and_is_reported() {
    let r = rig();
    let mut sync = r.sync(false);
    let t0 = Instant::now();
    sync.cycle(t0);
    // SMILE goes away and comes back with another database behind the same string.
    r.smile.set_down(true);
    sync.cycle(t0 + Duration::from_secs(31));
    r.smile.swap_database("SMILE_FO_RESTORED");
    r.smile.set_down(false);
    sync.cycle(t0 + Duration::from_secs(62));
    assert!(r.shared.scans_paused());
    let reports = r.reports();
    assert!(
        reports.iter().any(|p| p.code == codes::DB_IDENTITY_CHANGED),
        "{reports:?}"
    );
    assert!(r.shared.health().problem().is_some());

    let before = r.hub.requests().len();
    r.scans().turn(Instant::now());
    assert_eq!(r.hub.requests().len(), before, "no scan posts while paused");
}
