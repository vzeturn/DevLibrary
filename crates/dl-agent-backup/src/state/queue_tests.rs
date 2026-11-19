//! Ports of the Go `queue` tests.

use super::*;
use crate::state::temp_dir;
use dl_json::json;

const MIN: u64 = 60_000;

fn open() -> (Queue, PathBuf) {
    let dir = temp_dir("queue");
    let path = dir.join("queue.enc");
    (Queue::open(path.clone()).unwrap(), dir)
}

fn job(id: &str, at: u64) -> Job {
    Job::new(id, &format!("sched-{id}"), json!({"databaseName": id}), at)
}

#[test]
fn enqueue_dequeue() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    let j = q.dequeue(10).unwrap().unwrap();
    assert_eq!(j.id, "a");
    assert_eq!(j.params, json!({"databaseName": "a"}));
    assert_eq!(j.lease_until, Some(10 + LEASE.as_millis() as u64));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dequeue_empty_returns_none() {
    let (mut q, dir) = open();
    assert_eq!(q.dequeue(0).unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dequeue_skips_leased_and_returns_next() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.enqueue(job("b", 2)).unwrap();
    assert_eq!(q.dequeue(10).unwrap().unwrap().id, "a");
    assert_eq!(q.dequeue(11).unwrap().unwrap().id, "b");
    assert_eq!(q.dequeue(12).unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pending_order_follows_enqueued_at() {
    let (mut q, dir) = open();
    q.enqueue(job("late", 30)).unwrap();
    q.enqueue(job("early", 10)).unwrap();
    q.enqueue(job("mid", 20)).unwrap();
    let ids: Vec<String> = q.pending().into_iter().map(|j| j.id).collect();
    assert_eq!(ids, ["early", "mid", "late"]);
    assert_eq!(q.dequeue(40).unwrap().unwrap().id, "early");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn enqueue_is_idempotent_while_leased() {
    let (mut q, dir) = open();
    assert!(q.enqueue(job("a", 1)).unwrap());
    q.dequeue(2).unwrap();
    assert!(!q.enqueue(job("a", 3)).unwrap());
    assert_eq!(q.depth(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn complete_removes_job_and_unknown_is_fine() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.dequeue(2).unwrap();
    q.complete("a").unwrap();
    q.complete("nope").unwrap();
    assert_eq!(q.depth(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fail_releases_lease_immediately_and_retries() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.dequeue(2).unwrap();
    q.fail("a", "boom").unwrap();
    let again = q.dequeue(3).unwrap().unwrap();
    assert_eq!((again.attempts, again.last_error.as_str()), (1, "boom"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fail_exhausted_moves_to_dead() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    for i in 0..DEFAULT_MAX_ATTEMPTS {
        q.dequeue(10 + i as u64).unwrap().unwrap();
        q.fail("a", "boom").unwrap();
    }
    assert_eq!(q.dequeue(100).unwrap(), None);
    let dead = q.dead();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].attempts, DEFAULT_MAX_ATTEMPTS);
    assert_eq!(q.depth(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fail_with_empty_error_and_unknown_job() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.fail("a", "").unwrap();
    assert_eq!(q.pending()[0].last_error, "unknown error");
    assert!(q.fail("nope", "x").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn renew_extends_lease_and_fails_for_unknown_or_expired() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.dequeue(0).unwrap();
    q.renew("a", 4 * MIN).unwrap();
    assert_eq!(
        q.pending()[0].lease_until,
        Some(4 * MIN + LEASE.as_millis() as u64)
    );
    assert!(q.renew("nope", 0).is_err());
    assert!(q.renew("a", 100 * MIN).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn expired_lease_is_reclaimed_at_the_cost_of_an_attempt() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.dequeue(0).unwrap();
    let again = q.dequeue(6 * MIN).unwrap().unwrap();
    assert_eq!(again.id, "a");
    assert_eq!(again.attempts, 1);
    assert_eq!(again.last_error, LEASE_EXPIRED);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn repeated_crashes_reach_the_dead_letter() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    let mut now = 0;
    q.dequeue(now).unwrap();
    for _ in 1..DEFAULT_MAX_ATTEMPTS {
        now += 6 * MIN;
        assert!(q.dequeue(now).unwrap().is_some());
    }
    now += 6 * MIN;
    assert_eq!(q.dequeue(now).unwrap(), None);
    assert_eq!(q.dead()[0].last_error, LEASE_EXPIRED);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn discard_removes_dead_job_and_unknown_is_fine() {
    let (mut q, dir) = open();
    let mut j = job("a", 1);
    j.max_attempts = 1;
    q.enqueue(j).unwrap();
    q.dequeue(2).unwrap();
    q.fail("a", "x").unwrap();
    q.discard("a").unwrap();
    q.discard("a").unwrap();
    assert!(q.dead().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn release_keeps_the_job_without_spending_an_attempt() {
    let (mut q, dir) = open();
    q.enqueue(job("a", 1)).unwrap();
    q.dequeue(2).unwrap();
    q.release("a").unwrap();
    let again = q.dequeue(3).unwrap().unwrap();
    assert_eq!(again.attempts, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn queue_survives_reopen_and_preserves_explicit_fields() {
    let (mut q, dir) = open();
    let mut j = job("a", 12345);
    j.max_attempts = 5;
    q.enqueue(j).unwrap();
    q.dequeue(20000).unwrap();
    let mut q2 = Queue::open(dir.join("queue.enc")).unwrap();
    let pending = q2.pending();
    assert_eq!(pending[0].enqueued_at, 12345);
    assert_eq!(pending[0].max_attempts, 5);
    assert!(pending[0].lease_until.is_some());
    assert!(q2.has_schedule("sched-a"));
    q2.complete("a").unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn secrets_in_params_are_not_plaintext_on_disk_on_windows_only() {
    // Off Windows the file is framed, owner-only JSON (development builds), the same as
    // einvoice config.enc; on Windows DPAPI seals it. Either way it must round-trip.
    let (mut q, dir) = open();
    q.enqueue(Job::new("a", "s", json!({"password": "Sa$ecret'1;"}), 1))
        .unwrap();
    let raw = std::fs::read(dir.join("queue.enc")).unwrap();
    let plain_on_disk = String::from_utf8_lossy(&raw).contains("Sa$ecret");
    assert_eq!(plain_on_disk, cfg!(not(windows)));
    let q2 = Queue::open(dir.join("queue.enc")).unwrap();
    assert_eq!(q2.pending()[0].params, json!({"password": "Sa$ecret'1;"}));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn open_creates_parent_directory_on_first_write() {
    let dir = temp_dir("queue-parent");
    let nested = dir.join("a").join("b").join("queue.enc");
    let mut q = Queue::open(nested.clone()).unwrap();
    q.enqueue(job("a", 1)).unwrap();
    assert!(nested.exists());
    std::fs::remove_dir_all(dir).unwrap();
}
