//! The job cases of Go `run_test.go` plus the restart re-ack.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::jobmodel::{ACTION_CANCEL, MAPPING_DELETE, MAPPING_INSERT, MAPPING_UPDATE};
use crate::pms::Folio;
use std::collections::HashMap;

use crate::reconcile::tests::{day, existing_folio, rn, save_job};

fn one_ack(hub: &Hub, msg: &str) -> Ack {
    wait_until(|| hub.st().acks.len() == 1, msg);
    hub.st().acks[0].clone()
}

#[test]
fn process_job_holds_when_breaker_open() {
    let repo = FakeRepo::new();
    let r = runner(&testhub_dead(), &repo);
    for _ in 0..5 {
        r.breaker.on_failure();
    }
    let job = Job {
        job_id: "j1".into(),
        booking_id: "b1".into(),
        ..Job::default()
    };
    r.queue.enqueue(&job).expect("enqueue");
    r.process_job(&job, None);
    assert!(
        r.queue.unacked().is_empty(),
        "breaker open: job must not be applied"
    );
    assert_eq!(r.queue.pending().len(), 1, "job must remain pending");
    assert!(
        r.stats.pms_bad.load(Ordering::SeqCst),
        "pmsOk must be false when the breaker holds"
    );
}

fn testhub_dead() -> String {
    crate::testhub::dead_url()
}

#[test]
fn process_job_manual_acks_ok_no_error() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    repo.st().folios = vec![Folio {
        folio_status: 2,
        ..Folio::default()
    }];
    let r = runner(&hub.fake.url, &repo);
    let mut job = save_job();
    job.action = ACTION_CANCEL.into();
    r.process_job(&job, None);
    let st = hub.st();
    assert_eq!(st.acks.len(), 1);
    let a = &st.acks[0];
    assert!(a.ok, "manual is acked ok (not an error)");
    assert_eq!(a.error, "", "manual sends no error string");
    assert!(a.needs_manual, "{a:?}");
    assert!(r.drain_errors().is_empty(), "manual is not a recent error");
    assert_eq!(r.stats.manual_conflicts.load(Ordering::SeqCst), 1);
    assert!(
        r.stats
            .take_events()
            .iter()
            .any(|e| e.code == "hl_manual_conflict")
    );
}

#[test]
fn applies_job_and_acks_ok() {
    let hub = Hub::new();
    hub.st().jobs = vec![save_job()];
    let repo = FakeRepo::new();
    let run = start(runner(&hub.fake.url, &repo));
    let a = one_ack(&hub, "one ack");
    assert!(a.ok && a.job_id == save_job().job_id, "{a:?}");
    assert_eq!(repo.st().inserted.len(), 1, "the folio is inserted");
    wait_until(|| run.r.queue_depth() == 0, "acked job leaves the queue");
    let s = &run.r.stats;
    assert_eq!(s.jobs_polled.load(Ordering::SeqCst), 1);
    assert_eq!(s.jobs_applied.load(Ordering::SeqCst), 1);
    assert_eq!(s.jobs_acked.load(Ordering::SeqCst), 1);
    assert!(!s.status().last_job_at.is_empty());
}

#[test]
fn self_stops_when_superseded() {
    let hub = Hub::new();
    hub.st().supersede = true;
    let repo = FakeRepo::new();
    let mut run = start(runner(&hub.fake.url, &repo));
    assert!(
        run.finished_within(Duration::from_secs(5)),
        "Run must exit by itself on 403"
    );
    assert!(run.r.stats.superseded.load(Ordering::SeqCst));
}

#[test]
fn unauthorized_is_flagged_and_reported_once() {
    let hub = Hub::new();
    hub.st().unauthorized = true;
    let repo = FakeRepo::new();
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(
        || hub.st().unauthorized && run.r.stats.unauthorized.load(Ordering::SeqCst),
        "401 flagged",
    );
    wait_until(
        || run.r.stats.hub_fail_streak.load(Ordering::SeqCst) > 2,
        "keeps retrying",
    );
    let auth = run
        .r
        .stats
        .take_events()
        .iter()
        .filter(|e| e.code == "hl_auth_failed")
        .count();
    assert_eq!(auth, 1, "one report per 401 streak");
    hub.st().unauthorized = false;
    wait_until(
        || !run.r.stats.unauthorized.load(Ordering::SeqCst),
        "a good poll clears the flag",
    );
}

#[test]
fn acks_failure_with_error() {
    let hub = Hub::new();
    let mut bad = save_job();
    bad.check_in = "không-phải-ngày".into();
    hub.st().jobs = vec![bad];
    let repo = FakeRepo::new();
    let run = start(runner(&hub.fake.url, &repo));
    let a = one_ack(&hub, "one ack");
    assert!(!a.ok && !a.error.is_empty(), "{a:?}");
    assert_eq!(run.r.stats.jobs_failed.load(Ordering::SeqCst), 1);
    let ev = run.r.stats.take_events();
    let rec = ev
        .iter()
        .find(|e| e.code == "hl_reconcile_failed")
        .expect("event");
    assert!(
        !rec.message.contains("không-phải-ngày"),
        "reports carry no booking data: {rec:?}"
    );
}

#[test]
fn retries_after_hub_errors() {
    let hub = Hub::new();
    {
        let mut st = hub.st();
        st.fail_polls = 2;
        st.jobs = vec![save_job()];
    }
    let repo = FakeRepo::new();
    let _run = start(runner(&hub.fake.url, &repo));
    one_ack(&hub, "after two 500s the job still arrives");
    assert!(hub.st().poll_count >= 3);
}

#[test]
fn re_acks_after_restart() {
    let hub = Hub::new();
    let repo = FakeRepo::new();
    let dir = temp_dir("reack");
    {
        let q = FileQueue::open(&dir.join("queue.jsonl")).expect("queue");
        q.enqueue(&save_job()).expect("enqueue");
        q.mark_applied(&save_job().job_id, false, "boom", None)
            .expect("applied");
    }
    let run = start(runner_at(
        &hub.fake.url,
        &repo,
        &dir,
        Arc::new(OffsetDateTime::now_utc),
    ));
    let a = one_ack(&hub, "the lost ack is sent again");
    assert!(!a.ok && a.error == "boom", "{a:?}");
    assert!(repo.st().inserted.is_empty(), "no second apply");
    wait_until(|| run.r.queue_depth() == 0, "acked after the restart");
}

fn with_hotel_date(d: Option<dl_time::DateTime>) -> Arc<FakeRepo> {
    let repo = FakeRepo::new();
    repo.st().hotel_date = d;
    repo
}

#[test]
fn skips_past_booking() {
    let hub = Hub::new();
    hub.st().jobs = vec![save_job()]; // check-in 2026-06-12
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    let _run = start(runner(&hub.fake.url, &repo));
    let a = one_ack(&hub, "one ack (skip)");
    assert!(a.ok && a.pms_action == "skip", "{a:?}");
    assert!(repo.st().inserted.is_empty(), "skip must not apply");
}

/// RT-0651 (`TestRunnerAppliesModifyOfPastArrivalWithLiveFolio`): a Hotel Link change of
/// a booking whose arrival is already before the hotel date (stay extended, late arrival)
/// still reaches its live folio. Before the fix every such job was skipped.
#[test]
fn applies_modify_of_past_arrival_with_live_folio() {
    let hub = Hub::new();
    let mut job = save_job(); // check-in 2026-06-12 < hotel date
    job.check_out = "2026-06-16".into();
    if let Some(rates) = job.rooms.as_mut().and_then(|r| r[0].rates.as_mut()) {
        rates.push(rn("2026-06-14", 1600000.0));
        rates.push(rn("2026-06-15", 1600000.0));
    }
    let booker = job.booker.clone();
    hub.st().jobs = vec![job];
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    let mut f = existing_folio();
    f.room_code = String::new(); // not assigned, not checked in → modify allowed
    repo.st().folios_by_ext = Some(HashMap::from([(booker, vec![f])]));
    let _run = start(runner(&hub.fake.url, &repo));
    let a = one_ack(&hub, "one ack");
    assert!(
        a.ok && a.pms_action != "skip",
        "a past-arrival modify with a live folio is reconciled, not skipped: {a:?}"
    );
    let s = repo.st();
    assert!(
        s.updates.len() == 1 && s.updates[0].1.contains_key("DepartureDate"),
        "the live folio's DepartureDate is updated: {:?}",
        s.updates
    );
    assert!(s.inserted.is_empty(), "no new folio");
}

/// `TestRunnerKeepsPastArrivalPendingOnReadError`: a PMS read error on a past-arrival job
/// neither skips it (would hide the change) nor applies it (a later empty read would book
/// a back-dated folio): no ack, the job stays pending.
#[test]
fn keeps_past_arrival_pending_on_read_error() {
    let hub = Hub::new();
    hub.st().jobs = vec![save_job()];
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    repo.st().get_folios_err = Some("sql: lock timeout".into());
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(|| hub.st().poll_count >= 2, "polls past the job");
    std::thread::sleep(Duration::from_millis(100));
    assert!(hub.st().acks.is_empty(), "no ack: {:?}", hub.st().acks);
    assert!(repo.st().inserted.is_empty(), "no back-dated folio");
    assert_eq!(run.r.queue.pending().len(), 1, "job stays pending");
}

/// A PMS-down read on a past-arrival job counts against the breaker, like a failed apply.
#[test]
fn past_arrival_read_pms_down_trips_breaker() {
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    repo.st().get_folios_err = Some("dial tcp 10.0.0.5:1433: connection refused".into());
    let r = runner(&testhub_dead(), &repo);
    let job = save_job();
    r.queue.enqueue(&job).expect("enqueue");
    for _ in 0..5 {
        r.process_job(&job, Some(day(2026, 6, 15)));
    }
    assert_eq!(r.breaker.state(), "open", "PMS down opens the breaker");
    assert!(r.stats.pms_bad.load(Ordering::SeqCst), "pmsOk false");
    assert_eq!(r.queue.pending().len(), 1, "job stays pending");
}

/// A past-arrival job while the breaker is open is held, not skipped: the breaker is
/// checked before the PMS is read (Go 3.5.12 order).
#[test]
fn past_arrival_held_when_breaker_open() {
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    let r = runner(&testhub_dead(), &repo);
    for _ in 0..5 {
        r.breaker.on_failure();
    }
    let job = save_job();
    r.queue.enqueue(&job).expect("enqueue");
    r.process_job(&job, Some(day(2026, 6, 15)));
    assert!(r.queue.unacked().is_empty(), "not skipped");
    assert_eq!(r.queue.pending().len(), 1, "held pending");
}

#[test]
fn does_not_skip_current_booking() {
    let hub = Hub::new();
    hub.st().jobs = vec![save_job()];
    let repo = with_hotel_date(Some(day(2026, 6, 12)));
    let _run = start(runner(&hub.fake.url, &repo));
    assert!(one_ack(&hub, "one ack").ok);
    assert_eq!(
        repo.st().inserted.len(),
        1,
        "check-in == hotel date is processed"
    );
}

#[test]
fn fail_open_when_hotel_date_error() {
    let hub = Hub::new();
    hub.st().jobs = vec![save_job()];
    let repo = with_hotel_date(None);
    let _run = start(runner(&hub.fake.url, &repo));
    assert!(one_ack(&hub, "one ack (fail-open)").ok);
    assert_eq!(repo.st().inserted.len(), 1, "fail-open applies");
}

#[test]
fn skips_yesterday_arrival() {
    let hub = Hub::new();
    let mut job = save_job();
    job.check_in = "2026-06-14".into();
    hub.st().jobs = vec![job];
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    let _run = start(runner(&hub.fake.url, &repo));
    let a = one_ack(&hub, "one ack (yesterday)");
    assert!(a.ok && a.pms_action == "skip", "{a:?}");
    assert!(repo.st().inserted.is_empty());
}

#[test]
fn processes_future_arrival() {
    let hub = Hub::new();
    let mut job = save_job();
    job.check_in = "2026-06-22".into();
    hub.st().jobs = vec![job];
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    let _run = start(runner(&hub.fake.url, &repo));
    let a = one_ack(&hub, "one ack");
    assert!(a.ok && a.pms_action != "skip", "{a:?}");
    assert_eq!(repo.st().inserted.len(), 1);
}

#[test]
fn cancel_with_empty_check_in_processed_normally() {
    let hub = Hub::new();
    let mut job = save_job();
    job.action = ACTION_CANCEL.into();
    job.check_in = String::new();
    hub.st().jobs = vec![job];
    let repo = with_hotel_date(Some(day(2026, 6, 15)));
    let _run = start(runner(&hub.fake.url, &repo));
    assert!(one_ack(&hub, "one ack").ok);
}

#[test]
fn applies_mapping_cmds() {
    let hub = Hub::new();
    hub.st().mapping_cmds = vec![
        MappingCmd {
            cmd_id: "c1".into(),
            op: MAPPING_INSERT.into(),
            row: MappingRow {
                room_type_code: "DLX".into(),
                is_booking: true,
                ..MappingRow::default()
            },
            ..MappingCmd::default()
        },
        MappingCmd {
            cmd_id: "c2".into(),
            op: MAPPING_UPDATE.into(),
            orig: Some(MappingRow {
                room_type_code: "STD".into(),
                ..MappingRow::default()
            }),
            row: MappingRow {
                room_type_code: "STD".into(),
                split_part: Some(1),
                total_part: Some(3),
                ..MappingRow::default()
            },
            ..MappingCmd::default()
        },
        MappingCmd {
            cmd_id: "c3".into(),
            op: MAPPING_DELETE.into(),
            orig: Some(MappingRow {
                room_type_code: "OLD".into(),
                ..MappingRow::default()
            }),
            ..MappingCmd::default()
        },
    ];
    let repo = FakeRepo::new();
    let _run = start(runner(&hub.fake.url, &repo));
    wait_until(|| hub.st().map_cmd_acks.len() == 3, "three mapping acks");
    wait_until(
        || hub.st().mappings.len() >= 2,
        "startup push + push after the commands",
    );
    for (id, ok, err) in &hub.st().map_cmd_acks {
        assert!(*ok && err.is_empty(), "{id}: {err}");
    }
    let st = repo.st();
    assert_eq!(st.mapping_inserted.len(), 1);
    assert_eq!(st.mapping_inserted[0].room_type_code, "DLX");
    assert_eq!(st.mapping_updated.len(), 1);
    assert_eq!(st.mapping_updated[0].1.split_part, Some(1));
    assert_eq!(st.mapping_deleted.len(), 1);
    assert_eq!(st.mapping_deleted[0].room_type_code, "OLD");
}

#[test]
fn mapping_cmd_errors_are_acked_as_failed() {
    let hub = Hub::new();
    hub.st().mapping_cmds = vec![
        MappingCmd {
            cmd_id: "u1".into(),
            op: MAPPING_UPDATE.into(),
            ..MappingCmd::default()
        },
        MappingCmd {
            cmd_id: "x1".into(),
            op: "upsert".into(),
            ..MappingCmd::default()
        },
    ];
    let repo = FakeRepo::new();
    let _run = start(runner(&hub.fake.url, &repo));
    wait_until(|| hub.st().map_cmd_acks.len() == 2, "two mapping acks");
    let acks = hub.st().map_cmd_acks.clone();
    assert_eq!(
        acks[0],
        ("u1".into(), false, "lệnh update/delete thiếu orig".into())
    );
    assert_eq!(
        acks[1],
        (
            "x1".into(),
            false,
            "op mapping không hợp lệ: \"upsert\"".into()
        )
    );
}

#[test]
fn holds_mapping_cmds_when_pms_down() {
    let hub = Hub::new();
    hub.st().mapping_cmds = vec![MappingCmd {
        cmd_id: "c1".into(),
        op: MAPPING_INSERT.into(),
        row: MappingRow {
            room_type_code: "DLX".into(),
            ..MappingRow::default()
        },
        ..MappingCmd::default()
    }];
    let repo = FakeRepo::new();
    repo.st().mapping_write_err = Some(Error::new(Kind::PmsDown, "PMS down"));
    let run = start(runner(&hub.fake.url, &repo));
    wait_until(|| hub.st().poll_count >= 2, "polls past the command");
    assert!(hub.st().map_cmd_acks.is_empty(), "PMS down: no ack");
    assert!(repo.st().mapping_inserted.is_empty());
    assert!(
        run.r.stats.pms_bad.load(Ordering::SeqCst),
        "PMS down sets pmsOk=false"
    );
}

/// The hub hands a held job back on every poll; with the breaker open the loop waits
/// `breaker_poll_wait` between polls instead of polling again at once.
#[test]
fn held_jobs_slow_the_poll_while_the_breaker_is_open() {
    use std::sync::atomic::AtomicUsize;
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    let job = save_job().to_json();
    let fake = crate::testhub::FakeHub::start(move |req| {
        if req.method == "GET" && req.path.starts_with("/api/agent/jobs?") {
            p.fetch_add(1, Ordering::SeqCst);
            return (200, format!(r#"{{"jobs":[{job}]}}"#));
        }
        (200, r#"{"ok":true}"#.into())
    });
    let repo = FakeRepo::new();
    repo.st().hotel_date = Some(day(2026, 6, 1));
    let parts = Parts {
        hub: HubClient::new(&fake.url, "h1", "tok", "test", ""),
        repo: repo.clone(),
        queue: FileQueue::open(&temp_dir("run-held").join("queue.jsonl")).expect("queue"),
        state_dir: temp_dir("run-held-state"),
        legacy_policy_dir: None,
    };
    let t = Timings {
        breaker_cooldown: Duration::from_secs(60),
        breaker_poll_wait: Duration::from_millis(200),
        ..fast()
    };
    let r = Arc::new(Runner::with(parts, t, Arc::new(OffsetDateTime::now_utc)));
    for _ in 0..5 {
        r.breaker.on_failure();
    }
    let running = start(r);
    wait_until(|| polls.load(Ordering::SeqCst) >= 1, "first poll");
    std::thread::sleep(Duration::from_secs(1));
    let n = polls.load(Ordering::SeqCst);
    assert!((2..=8).contains(&n), "{n} polls in about 1 s");
    assert!(repo.st().inserted.is_empty(), "held, not applied");
    assert_eq!(running.r.queue.pending().len(), 1);
}
