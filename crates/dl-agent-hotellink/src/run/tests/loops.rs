//! The mapping, availability, status, start-of-day and supervise cases.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dl_time::{DateTime, Offset, Time};

use super::*;
use crate::jobmodel::mapping_hash;
use crate::run::side::{drop_past_rows, start_of_local_day};
use crate::run::{Stop, supervise};

fn row(code: &str, date: &str, n: i64) -> AvailRow {
    AvailRow {
        room_type_code: code.into(),
        date: date.into(),
        available: n,
    }
}

fn at(y: i32, m: u8, d: u8, h: u8, min: u8, off: Offset) -> OffsetDateTime {
    let t = Time::from_hms(h, min, 0).expect("time");
    DateTime::new(Date::new(y, m, d).expect("date"), t).assume_offset(off)
}

fn fixed(t: OffsetDateTime) -> Clock {
    Arc::new(move || t)
}

#[test]
fn syncs_mapping_when_hub_wants() {
    let hub = Hub::new();
    hub.st().want_mapping = true;
    let repo = FakeRepo::new();
    repo.st().mapping = vec![MappingRow {
        room_id: "r-1".into(),
        room_type_code: "DLX".into(),
        is_booking: true,
        ..MappingRow::default()
    }];
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || hub.st().mappings.len() >= 2,
        "startup push + wantMapping push",
    );
    let (codes, hash) = hub.st().mappings.last().cloned().expect("push");
    assert_eq!(codes, ["DLX"]);
    assert_eq!(hash, mapping_hash(&repo.st().mapping));
    assert_eq!(run.r.stats.status().mapping_hash, hash);
}

#[test]
fn availability_respects_hl_inventory_sync_flag() {
    // Unlike the Go runner, the boot seeds HLInventorySync=1 when absent, so the "off"
    // case sets the flag explicitly.
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().global_params.insert("HLInventorySync".into(), 0);
    repo.st().avail = vec![row("DLX", "2026-06-12", 5)];
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || !hub.st().inv_sync_headers.is_empty(),
        "flag 0 signals sync off",
    );
    drop(run);
    assert_eq!(hub.st().inv_sync_headers[0], "0");
    assert!(hub.st().availability[0].is_empty());

    let hub2 = Hub::new();
    let repo2 = FakeRepo::new();
    repo2.st().global_params.insert("HLInventorySync".into(), 1);
    repo2.st().avail = vec![row("DLX", "2026-06-12", 5)];
    let now = fixed(at(2026, 6, 12, 0, 0, Offset::UTC));
    let run2 = start(runner_at(&hub2.fake.url, &repo2, &temp_dir("avail"), now));
    wait_until(|| !hub2.st().availability.is_empty(), "flag 1 pushes");
    assert_eq!(hub2.st().availability[0][0].available, 5);
    assert_eq!(hub2.st().inv_sync_headers[0], "");
    assert!(run2.r.stats.availability_pushes.load(Ordering::SeqCst) >= 1);
}

#[test]
fn boot_seeds_inventory_flag() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().avail = vec![row("DLX", "2999-01-01", 3)];
    let _run = start(runner(&hub.fake.url, &repo));
    wait_until(|| !hub.st().availability.is_empty(), "seeded flag pushes");
    assert_eq!(repo.st().global_params.get("HLInventorySync"), Some(&1));
}

#[test]
fn boot_waits_for_sql() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().sql_down_calls = 3;
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || run.r.stats.sql_ready.load(Ordering::SeqCst),
        "SQL comes back",
    );
    assert_eq!(repo.st().sql_down_calls, 0);
    assert!(
        run.r
            .stats
            .take_events()
            .iter()
            .any(|e| e.code == "hl_pms_failed")
    );
}

#[test]
fn availability_disabled_signals_hub() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().global_params.insert("HLInventorySync".into(), 0);
    let _run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || !hub.st().inv_sync_headers.is_empty(),
        "sync off is signalled",
    );
    assert_eq!(hub.st().inv_sync_headers[0], "0");
    assert!(hub.st().availability[0].is_empty());
}

#[test]
fn availability_drops_past_dates() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().global_params.insert("HLInventorySync".into(), 1);
    repo.st().avail = vec![
        row("DLX", "2026-06-14", 5),
        row("DLX", "2026-06-15", 6),
        row("DLX", "2026-06-16", 7),
    ];
    let now = fixed(at(2026, 6, 15, 9, 0, Offset::UTC));
    let _run = start(runner_at(&hub.fake.url, &repo, &temp_dir("past"), now));
    wait_until(|| !hub.st().availability.is_empty(), "pushes");
    let rows = hub.st().availability[0].clone();
    assert!(
        rows.iter().all(|r| r.date.as_str() >= "2026-06-15"),
        "{rows:?}"
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn drop_past_rows_keeps_undated() {
    let today = Date::new(2026, 6, 15).expect("date");
    let rows = vec![
        row("A", "", 1),
        row("A", "2026-06-14", 2),
        row("A", "2026-06-15", 3),
    ];
    let kept: Vec<i64> = drop_past_rows(rows, today)
        .iter()
        .map(|r| r.available)
        .collect();
    assert_eq!(kept, [1, 3]);
}

#[test]
fn hub_policy_overrides_the_db_flag_and_is_cached() {
    let hub = Hub::new();
    hub.st().config = r#"{"enabled":true,"availabilityIntervalMin":30,"availabilityDays":60,"policy":{"onCancel":{"allowWhenRoomAssigned":true}}}"#.into();
    let repo = FakeRepo::new();
    repo.st().global_params.insert("HLInventorySync".into(), 0);
    repo.st().avail = vec![row("DLX", "2999-01-01", 4)];
    let dir = temp_dir("policy-run");
    let _run = start(runner_at(
        &hub.fake.url,
        &repo,
        &dir,
        Arc::new(OffsetDateTime::now_utc),
    ));
    wait_until(
        || !hub.st().availability.is_empty(),
        "pushes despite the DB flag",
    );
    assert_eq!(hub.st().inv_sync_headers[0], "", "the hub owns the switch");
    let p = crate::policycache::read(&dir).expect("cached");
    assert!(p.on_cancel.allow_when_room_assigned);
}

#[test]
fn sync_mapping_returns_error() {
    let repo = FakeRepo::new();
    repo.st().mapping = vec![MappingRow {
        room_id: "r-1".into(),
        room_type_code: "DLX".into(),
        ..MappingRow::default()
    }];
    let r = runner(&crate::testhub::dead_url(), &repo);
    assert!(r.sync_mapping().is_err(), "dead hub: SyncMapping fails");
}

#[test]
fn status_snapshot() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().mapping = vec![MappingRow {
        room_id: "r-1".into(),
        room_type_code: "DLX".into(),
        ..MappingRow::default()
    }];
    let r = runner(&hub.fake.url, &repo);
    r.sync_mapping().expect("sync");
    let s = r.stats.status();
    assert_eq!(s.mapping_count, 1);
    assert_eq!(s.mapping_hash, mapping_hash(&repo.st().mapping));
    assert!(!s.last_sync_at.is_empty());
}

#[test]
fn heartbeat_reports_state() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    let _run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || !hub.fake.hits("/api/agent/heartbeat").is_empty(),
        "heartbeat sent",
    );
    let hb = &hub.fake.hits("/api/agent/heartbeat")[0];
    assert!(
        hb.body.contains(r#""pmsOk":true"#) && hb.body.contains(r#""breakerState":"closed""#),
        "{}",
        hb.body
    );
}

#[test]
fn start_of_local_day_pre_dawn() {
    // 06:30 in Vietnam is 23:30 UTC the day before.
    let now = at(2026, 6, 17, 6, 30, Offset::VIETNAM);
    assert_eq!(start_of_local_day(now).to_string(), "2026-06-17");
    assert_eq!(
        now.to_offset(Offset::UTC).date().to_string(),
        "2026-06-16",
        "the old truncation bug"
    );
}

#[test]
fn record_error_keeps_the_last_twenty() {
    let repo = FakeRepo::new();
    let r = runner(&crate::testhub::dead_url(), &repo);
    for i in 0..25 {
        r.record_error(&format!("e{i}"));
    }
    let errs = r.drain_errors();
    assert_eq!(errs.len(), 20);
    assert!(
        errs[0].ends_with(" e5") && errs[19].ends_with(" e24"),
        "{errs:?}"
    );
    assert_eq!(errs[0].len(), "2026-06-10 08:00:00 e5".len());
    assert!(r.drain_errors().is_empty());
}

#[test]
fn supervise_restarts_after_panic() {
    let stop = Stop::default();
    let calls = AtomicUsize::new(0);
    supervise("t", &stop, Duration::from_millis(1), || {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("boom");
        }
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn supervise_stops_when_stopped() {
    let stop = Stop::default();
    let calls = AtomicUsize::new(0);
    supervise("t", &stop, Duration::from_millis(1), || {
        calls.fetch_add(1, Ordering::SeqCst);
        stop.stop();
        panic!("boom");
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn supervise_no_restart_on_normal_return() {
    let stop = Stop::default();
    let calls = AtomicUsize::new(0);
    supervise("t", &stop, Duration::from_millis(1), || {
        calls.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn stop_wakes_sleepers() {
    let stop = Stop::default();
    let s2 = stop.clone();
    let h = std::thread::spawn(move || s2.sleep(Duration::from_secs(30)));
    std::thread::sleep(Duration::from_millis(20));
    stop.stop();
    assert!(h.join().expect("join"), "sleep returns true when stopped");
    assert!(!Stop::default().sleep(Duration::from_millis(1)));
}

/// Go cancels the runner's context on stop, which ends the long poll at once; here
/// `Runner::stop` shuts the poll's socket down.
#[test]
fn stop_ends_a_held_long_poll_within_two_seconds() {
    let (hub, polls, release) = super::holding_hub();
    let repo = FakeRepo::new();
    let parts = Parts {
        hub: HubClient::new(&hub.url, "h1", "tok", "test", ""),
        repo: repo.clone(),
        queue: FileQueue::open(&temp_dir("run-longpoll").join("queue.jsonl")).expect("queue"),
        state_dir: temp_dir("run-longpoll-state"),
        legacy_policy_dir: None,
    };
    let t = Timings {
        poll_wait_sec: 25,
        ..super::fast()
    };
    let mut running = start(Arc::new(Runner::with(
        parts,
        t,
        Arc::new(OffsetDateTime::now_utc),
    )));
    wait_until(|| polls.load(Ordering::SeqCst) == 1, "poll held open");
    std::thread::sleep(Duration::from_millis(300));
    let started = std::time::Instant::now();
    running.r.stop();
    let ended = running.finished_within(Duration::from_secs(2));
    release.store(true, Ordering::SeqCst);
    assert!(ended, "runner still running after {:?}", started.elapsed());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
}
