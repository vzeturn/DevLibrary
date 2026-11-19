//! The three end-to-end scenarios. Go references (SMILE-HLAPI `go/`):
//! `internal/agent/run.go:159` bookingLoop, `:207` processJob, `:261` ackJob, `:269`
//! drainUnacked, `:89` breaker 5/30 s/5 min, and `e2e/e2e_test.go:133`.

use std::time::{Duration, Instant};

use dl_json::Value;

use super::{Hub, Proxy, booking_job, future_day, sql, start_module, wait};
use crate::jobqueue::FileQueue;
use crate::jobqueue::tests::temp_dir;
use crate::module::{NAME, QUEUE_FILE};

const MAPPING_CMD: &str = r#"{"cmdId":"m-1","hotelId":"h1","op":"insert","row":{"roomId":"r-200","roomTypeCode":"STD","splitPart":null,"totalPart":null,"isBooking":true},"orig":null,"enqueuedAt":"2026-06-10T08:30:00Z"}"#;

fn parse(body: &str) -> Value {
    dl_json::parse(body).unwrap_or_else(|e| panic!("ack body {body}: {e}"))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Booking + mapping command over the long poll → Folio, FolioRate, AdditionalName and
/// RoomTypeMapped written by the real module; the hub gets the job ack with the folio, the
/// mapping-command ack, and the mapping pushes (2 rows at boot, 3 after the command).
#[test]
fn booking_and_mapping_reach_sql_and_the_hub_gets_the_acks() {
    let Some(mut db) = sql() else { return };
    let proxy = Proxy::start(db.upstream);
    let hub = Hub::start();
    let job_id = "0001-e2e-1";
    {
        let mut st = hub.st();
        st.job = Some(booking_job(job_id, "EXT-E2E-1", future_day()));
        st.job_id = job_id.into();
        st.mapping_cmds.push(("m-1".into(), MAPPING_CMD.into()));
        st.availability = true;
    }
    let dir = temp_dir("e2e-booking");
    let started = Instant::now();
    let mut m = start_module(&dir, &hub.fake.url, &db.site_string(&proxy));
    wait(30, "job ack", || !hub.st().acks.is_empty());
    let applied_in = started.elapsed();
    wait(10, "mapping push after the command", || {
        hub.st().mappings.iter().any(|(n, _)| *n == 3)
    });
    wait(10, "availability push", || hub.st().availability_pushes > 0);

    let folios = db.folio_nums("EXT-E2E-1");
    assert_eq!(folios.len(), 1, "one folio");
    let p = "EXT-E2E-1";
    assert_eq!(
        db.count(
            "SELECT COUNT(*) FROM FolioRate fr JOIN Folio f ON f.FolioNum = fr.FolioNum WHERE f.Booker = @p1",
            p
        ),
        2,
        "two nights"
    );
    assert_eq!(
        db.count(
            "SELECT COUNT(*) FROM AdditionalName a JOIN Folio f ON f.FolioNum = a.FolioNum WHERE f.Booker = @p1",
            p
        ),
        1
    );
    assert_eq!(
        db.count(
            "SELECT COUNT(*) FROM Folio WHERE Booker = @p1 AND RoomTypeCode = 'DLX01' AND RateAmount = 1500000",
            p
        ),
        1
    );
    assert_eq!(
        db.count(
            "SELECT COUNT(*) FROM RoomTypeMapped WHERE RoomId = @p1 AND RoomTypeCode = 'STD' AND IsBooking = 1",
            "r-200"
        ),
        1
    );

    let st = hub.st();
    assert_eq!(st.acks.len(), 1, "{:?}", st.acks);
    let ack = parse(&st.acks[0]);
    assert_eq!(ack.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(s(&ack, "error"), "");
    assert!(ack.get("recentErrors").is_some_and(Value::is_null));
    let res = ack.get("pmsResult").expect("pmsResult");
    assert_eq!(s(res, "action"), "save");
    assert_eq!(res.get("completed").and_then(Value::as_bool), Some(true));
    let f0 = &res.get("folios").and_then(Value::as_array).expect("folios")[0];
    assert_eq!(f0.get("folioNum").and_then(Value::as_f64), Some(folios[0]));
    assert_eq!(s(f0, "roomTypeCode"), "DLX01");
    assert_eq!(st.map_cmd_acks.len(), 1);
    let cmd_ack = parse(&st.map_cmd_acks[0]);
    assert_eq!(cmd_ack.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(st.mappings.first().map(|(n, _)| *n), Some(2), "boot push");
    let last_hash = st
        .mappings
        .last()
        .map(|(_, h)| h.clone())
        .unwrap_or_default();
    drop(st);
    // The module publishes metrics every 500 ms.
    wait(5, "metrics", || {
        m.metric("jobs_acked") == 1
            && m.metric("mapping_count") == 3
            && m.metric_str("mapping_hash") == last_hash
    });
    let took = m.stop();
    eprintln!(
        "e2e booking: acked {applied_in:?} after start, stop {took:?}, {} proxy connections",
        proxy.attempts()
    );
    assert!(took < Duration::from_secs(2), "stop took {took:?}");
}

/// The job is applied, its ack is held by the hub and the module stops (the process dies
/// before the ack lands): the queue keeps it applied-not-acked. On restart the module acks
/// it again with the same body before polling, and the hub's redelivery of the same job
/// writes nothing (one folio, same FolioNum, two rates) and is acked again.
#[test]
fn ack_lost_at_a_crash_is_replayed_without_a_second_write() {
    let Some(mut db) = sql() else { return };
    let proxy = Proxy::start(db.upstream);
    let hub = Hub::start();
    let job_id = "0001-e2e-2";
    let booker = "EXT-E2E-2";
    {
        let mut st = hub.st();
        st.job = Some(booking_job(job_id, booker, future_day()));
        st.job_id = job_id.into();
        st.hold_acks = true;
    }
    let dir = temp_dir("e2e-replay");
    let mut m = start_module(&dir, &hub.fake.url, &db.site_string(&proxy));
    wait(30, "the held ack", || !hub.st().held_acks.is_empty());
    let first = db.folio_nums(booker);
    assert_eq!(first.len(), 1, "applied before the ack");
    let took = m.stop();
    assert!(
        took < Duration::from_secs(2),
        "stop cut the held ack: {took:?}"
    );
    assert!(hub.st().acks.is_empty(), "no ack delivered");
    let queue_path = dir.join("data").join(NAME).join(QUEUE_FILE);
    {
        let q = FileQueue::open(&queue_path).expect("queue");
        assert_eq!(q.unacked().len(), 1, "applied, not acked, on disk");
        assert!(q.pending().is_empty());
    }
    let lost = hub.st().held_acks[0].clone();
    {
        let mut st = hub.st();
        st.hold_acks = false;
        st.redeliver_once = true;
    }

    let mut m2 = start_module(&dir, &hub.fake.url, &db.site_string(&proxy));
    wait(30, "the replayed ack and the redelivery ack", || {
        hub.st().acks.len() >= 2
    });
    let st = hub.st();
    assert_eq!(
        st.acks[0], lost,
        "the replayed ack is byte for byte the lost one"
    );
    let again = parse(&st.acks[1]);
    assert_eq!(again.get("ok").and_then(Value::as_bool), Some(true));
    let res = again.get("pmsResult").expect("pmsResult");
    assert_eq!(s(res, "action"), "save");
    drop(st);
    assert_eq!(
        db.folio_nums(booker),
        first,
        "no second folio, same FolioNum"
    );
    assert_eq!(
        db.count(
            "SELECT COUNT(*) FROM FolioRate fr JOIN Folio f ON f.FolioNum = fr.FolioNum WHERE f.Booker = @p1",
            booker
        ),
        2
    );
    assert_eq!(
        db.count(
            "SELECT COUNT(*) FROM Folio WHERE Booker = @p1 AND ISNULL(CancelNum, 0) <> 0",
            booker
        ),
        0,
        "nothing cancelled"
    );
    m2.stop();
    let q = FileQueue::open(&queue_path).expect("queue");
    assert!(q.unacked().is_empty() && q.pending().is_empty());
}

/// SQL down with a job waiting: five PMS failures open the breaker; while it is open no
/// apply runs and the proxy sees no connection attempt; SQL back → after the cooldown the
/// half-open probe applies the job, the breaker closes and the hub gets the ack.
#[test]
fn breaker_opens_on_sql_down_stops_hammering_and_recovers() {
    let Some(mut db) = sql() else { return };
    let proxy = Proxy::start(db.upstream);
    let hub = Hub::start();
    let dir = temp_dir("e2e-breaker");
    let mut m = start_module(&dir, &hub.fake.url, &db.site_string(&proxy));
    wait(30, "running", || m.metric("polls_ok") > 0);

    proxy.down();
    let down_at = Instant::now();
    let job_id = "0001-e2e-3";
    let booker = "EXT-E2E-3";
    {
        let mut st = hub.st();
        st.job = Some(booking_job(job_id, booker, future_day()));
        st.job_id = job_id.into();
    }
    wait(30, "breaker open", || {
        m.metric_str("breaker_state") == "open"
    });
    let opened_in = down_at.elapsed();
    let failed = m.metric("jobs_failed");
    let attempts_at_open = proxy.attempts();
    let polls_at_open = hub.st().polls;
    std::thread::sleep(Duration::from_millis(2000));
    let polls_while_open = hub.st().polls - polls_at_open;
    let attempts_while_open = proxy.attempts() - attempts_at_open;
    assert_eq!(failed, 5, "threshold");
    assert_eq!(m.metric("jobs_failed"), failed, "no apply while open");
    // Only GetHotelDate (every poll, fail-open, as Go run.go:190) still asks the pool, whose
    // own backoff (1 s doubling) allows at most one real connect per step.
    assert!(
        attempts_while_open <= 2,
        "{attempts_while_open} SQL connection attempts while the breaker is open"
    );
    // The poll goes on, but 200 ms apart while the hub only hands back held jobs.
    assert!(
        (1..=15).contains(&polls_while_open),
        "{polls_while_open} hub polls in 2 s while held"
    );
    assert!(hub.st().acks.is_empty(), "a PMS-down job is never acked");
    assert_eq!(db.folio_nums(booker).len(), 0);

    proxy.up();
    let up_at = Instant::now();
    wait(60, "ack after recovery", || !hub.st().acks.is_empty());
    let recovered_in = up_at.elapsed();
    wait(10, "breaker closed", || {
        m.metric_str("breaker_state") == "closed"
    });
    assert_eq!(db.folio_nums(booker).len(), 1);
    let ack = parse(&hub.st().acks[0]);
    assert_eq!(ack.get("ok").and_then(Value::as_bool), Some(true));
    // The PMS-down error lines go out with the heartbeat or this ack, whichever drains the
    // ring first (Go `drainErrors`); either way the ack itself is clean.
    assert_eq!(s(&ack, "error"), "");
    m.stop();
    eprintln!(
        "e2e breaker: open after {opened_in:?} ({failed} failures, {attempts_at_open} proxy \
         connections in total), {attempts_while_open} attempts and {polls_while_open} hub polls in 2 s open, \
         recovered {recovered_in:?} after SQL came back"
    );
}
