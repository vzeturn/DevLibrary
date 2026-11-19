//! The durable job queue, the port of the Go `queue` package: idempotent enqueue, leased
//! dequeue, lease renewal, retry with an attempt budget and a dead letter. A lease that
//! runs out means the worker died mid-job, so reclaiming it costs an attempt.
//!
//! Jobs carry the SQL password and destination credentials, so the file is sealed like
//! every state file. All methods take the current time in milliseconds for testability.

use std::path::PathBuf;
use std::time::Duration;

use dl_json::{Map, Value};

use super::{JsonFile, s, u};
use crate::Error;

/// How long a dequeued job is held before another worker may reclaim it.
pub const LEASE: Duration = Duration::from_secs(5 * 60);
/// Attempts before a job moves to the dead letter.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// The error recorded when a lease runs out.
pub const LEASE_EXPIRED: &str = "lease expired — worker died mid-job";

/// One queued backup.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The job id; the hub history id it reports under.
    pub id: String,
    /// The schedule that made it.
    pub schedule_id: String,
    /// What to run (the executor's params, secrets included).
    pub params: Value,
    /// Attempts used.
    pub attempts: u32,
    /// Attempt budget.
    pub max_attempts: u32,
    /// When it was queued (ms).
    pub enqueued_at: u64,
    /// When the current lease runs out (ms), while a worker holds it.
    pub lease_until: Option<u64>,
    /// The last failure.
    pub last_error: String,
    /// In the dead letter: out of attempts, waiting to be reported and discarded.
    pub dead: bool,
}

impl Job {
    /// A fresh job with the default attempt budget.
    pub fn new(id: &str, schedule_id: &str, params: Value, now: u64) -> Job {
        Job {
            id: id.to_owned(),
            schedule_id: schedule_id.to_owned(),
            params,
            attempts: 0,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            enqueued_at: now,
            lease_until: None,
            last_error: String::new(),
            dead: false,
        }
    }

    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id", self.id.as_str());
        m.insert("scheduleId", self.schedule_id.as_str());
        m.insert("params", self.params.clone());
        m.insert("attempts", self.attempts as u64);
        m.insert("maxAttempts", self.max_attempts as u64);
        m.insert("enqueuedAt", self.enqueued_at);
        m.insert("leaseUntil", self.lease_until);
        m.insert("lastError", self.last_error.as_str());
        m.insert("dead", self.dead);
        Value::Object(m)
    }

    fn from_json(v: &Value) -> Job {
        Job {
            id: s(v, "id"),
            schedule_id: s(v, "scheduleId"),
            params: v.get("params").cloned().unwrap_or(Value::Null),
            attempts: u(v, "attempts") as u32,
            max_attempts: (u(v, "maxAttempts") as u32).max(1),
            enqueued_at: u(v, "enqueuedAt"),
            lease_until: v.get("leaseUntil").and_then(Value::as_u64),
            last_error: s(v, "lastError"),
            dead: v.get("dead").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

/// The queue file and its jobs in memory.
#[derive(Debug)]
pub struct Queue {
    file: JsonFile,
    jobs: Vec<Job>,
    lease: Duration,
}

impl Queue {
    /// Opens (or starts) the queue at `path`.
    pub fn open(path: PathBuf) -> Result<Queue, Error> {
        let file = JsonFile::new(path);
        let jobs = match file.load()? {
            Some(doc) => doc
                .get("jobs")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(Job::from_json)
                        .filter(|j| !j.id.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Ok(Queue {
            file,
            jobs,
            lease: LEASE,
        })
    }

    /// Overrides the lease length (tests).
    pub fn with_lease(mut self, lease: Duration) -> Queue {
        self.lease = lease;
        self
    }

    fn persist(&self) -> Result<(), Error> {
        let jobs: Vec<Value> = self.jobs.iter().map(Job::to_json).collect();
        let mut m = Map::new();
        m.insert("jobs", jobs);
        self.file.store(&Value::Object(m))
    }

    fn find(&mut self, id: &str) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// Adds `job` unless a job with its id is already queued (leased or not); returns
    /// whether it was added.
    pub fn enqueue(&mut self, job: Job) -> Result<bool, Error> {
        if self.jobs.iter().any(|j| j.id == job.id) {
            return Ok(false);
        }
        self.jobs.push(job);
        self.persist()?;
        Ok(true)
    }

    /// Reclaims expired leases (each costs an attempt), then leases the oldest pending job.
    pub fn dequeue(&mut self, now: u64) -> Result<Option<Job>, Error> {
        let mut changed = false;
        for j in self.jobs.iter_mut().filter(|j| !j.dead) {
            if j.lease_until.is_some_and(|until| until <= now) {
                j.lease_until = None;
                j.attempts += 1;
                j.last_error = LEASE_EXPIRED.to_owned();
                j.dead = j.attempts >= j.max_attempts;
                changed = true;
            }
        }
        let lease_ms = self.lease.as_millis() as u64;
        let next = self
            .jobs
            .iter_mut()
            .filter(|j| !j.dead && j.lease_until.is_none())
            .min_by_key(|j| j.enqueued_at);
        let out = next.map(|j| {
            j.lease_until = Some(now + lease_ms);
            j.clone()
        });
        if changed || out.is_some() {
            self.persist()?;
        }
        Ok(out)
    }

    /// Extends the lease of a job this worker still holds.
    pub fn renew(&mut self, id: &str, now: u64) -> Result<(), Error> {
        let lease_ms = self.lease.as_millis() as u64;
        let j = self
            .find(id)
            .ok_or_else(|| Error::Io(format!("queue: renew unknown job {id}")))?;
        match j.lease_until {
            Some(until) if until > now && !j.dead => {
                j.lease_until = Some(now + lease_ms);
                self.persist()
            }
            _ => Err(Error::Io(format!(
                "queue: lease on job {id} is no longer held"
            ))),
        }
    }

    /// Removes a finished job; an unknown id is not an error.
    pub fn complete(&mut self, id: &str) -> Result<(), Error> {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        if self.jobs.len() != before {
            self.persist()?;
        }
        Ok(())
    }

    /// Records a failed attempt and releases the lease at once; out of attempts → dead.
    pub fn fail(&mut self, id: &str, error: &str) -> Result<(), Error> {
        let j = self
            .find(id)
            .ok_or_else(|| Error::Io(format!("queue: fail unknown job {id}")))?;
        j.attempts += 1;
        j.last_error = if error.is_empty() {
            "unknown error".into()
        } else {
            error.to_owned()
        };
        j.lease_until = None;
        j.dead = j.attempts >= j.max_attempts;
        self.persist()
    }

    /// Gives the lease back without spending an attempt: the agent is stopping and the job
    /// should run again on the next start.
    pub fn release(&mut self, id: &str) -> Result<(), Error> {
        if let Some(j) = self.find(id) {
            j.lease_until = None;
            return self.persist();
        }
        Ok(())
    }

    /// Jobs that ran out of attempts.
    pub fn dead(&self) -> Vec<Job> {
        self.jobs.iter().filter(|j| j.dead).cloned().collect()
    }

    /// Drops a job (after its dead-letter report); an unknown id is not an error.
    pub fn discard(&mut self, id: &str) -> Result<(), Error> {
        self.complete(id)
    }

    /// Jobs waiting or running, oldest first.
    pub fn pending(&self) -> Vec<Job> {
        let mut out: Vec<Job> = self.jobs.iter().filter(|j| !j.dead).cloned().collect();
        out.sort_by_key(|j| j.enqueued_at);
        out
    }

    /// How many jobs are waiting or running.
    pub fn depth(&self) -> usize {
        self.jobs.iter().filter(|j| !j.dead).count()
    }

    /// Whether a job for `schedule_id` is already waiting or running.
    pub fn has_schedule(&self, schedule_id: &str) -> bool {
        self.jobs
            .iter()
            .any(|j| !j.dead && j.schedule_id == schedule_id)
    }
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod tests;
