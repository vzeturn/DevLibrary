//! The local job queue (Go `agent/jobqueue/file.go`): append-only JSON lines, the last
//! line of a job id wins, fsync after every append, compaction drops acked jobs. A job
//! moves pending -> applied -> acked; a crash between apply and ack replays the ack, a
//! crash before apply replays the apply.
//!
//! Unlike Go's map iteration, entries come back in the order they were first seen, so the
//! pending jobs are retried oldest first.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dl_json::Value;
use dl_time::OffsetDateTime;

use crate::error::{Error, Result};
use crate::gojson::Obj;
use crate::jobmodel::{Job, PmsResult};
use crate::reconcile::rfc3339_utc;

/// `StatePending`: received from the hub, not applied to the PMS yet.
pub const PENDING: &str = "pending";
/// `StateApplied`: applied to the PMS, not acked to the hub yet.
pub const APPLIED: &str = "applied";
/// `StateAcked`: acked; dropped by the next compaction.
pub const ACKED: &str = "acked";

/// `Entry`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    /// `jobId`.
    pub job_id: String,
    /// `bookingId`.
    pub booking_id: String,
    /// `job`.
    pub job: Job,
    /// `state`.
    pub state: String,
    /// `ok`: the apply result (meaningful from `applied`).
    pub ok: bool,
    /// `lastError` (omitempty).
    pub last_error: String,
    /// `pmsResult` (omitempty).
    pub pms_result: Option<PmsResult>,
    /// `appliedAt` (omitempty).
    pub applied_at: String,
    /// `updatedAt`.
    pub updated_at: String,
}

impl Entry {
    fn to_line(&self) -> String {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.str("jobId", &self.job_id)
            .str("bookingId", &self.booking_id)
            .raw("job", |out| self.job.write_json(out))
            .str("state", &self.state)
            .bool("ok", self.ok)
            .str_omit("lastError", &self.last_error);
        if let Some(r) = &self.pms_result {
            o.raw("pmsResult", |out| r.write_json(out));
        }
        o.str_omit("appliedAt", &self.applied_at)
            .str("updatedAt", &self.updated_at)
            .end();
        out.push('\n');
        out
    }

    fn from_line(line: &str) -> Option<Entry> {
        let v = dl_json::parse(line).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let e = Entry {
            job_id: s("jobId"),
            booking_id: s("bookingId"),
            job: v.get("job").map(Job::from_value).unwrap_or_default(),
            state: s("state"),
            ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false),
            last_error: s("lastError"),
            pms_result: v
                .get("pmsResult")
                .filter(|r| !r.is_null())
                .map(PmsResult::from_value),
            applied_at: s("appliedAt"),
            updated_at: s("updatedAt"),
        };
        (!e.job_id.is_empty()).then_some(e)
    }
}

/// `FileQueue`.
#[derive(Debug)]
pub struct FileQueue {
    path: PathBuf,
    entries: Mutex<Vec<Entry>>,
}

fn io(path: &Path, what: &str, e: std::io::Error) -> Error {
    Error::other(format!("job queue {what} {}: {e}", path.display()))
}

fn now() -> String {
    rfc3339_utc(OffsetDateTime::now_utc())
}

/// Reads the entries of a queue file (latest line per job id, first-seen order); broken or
/// truncated lines are skipped. A missing file is empty.
fn read_entries(path: &Path) -> Result<Vec<Entry>> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(path, "open", e)),
    };
    let mut out: Vec<Entry> = Vec::new();
    for line in std::io::BufReader::new(f).split(b'\n') {
        let line = line.map_err(|e| io(path, "read", e))?;
        let Some(e) = Entry::from_line(&String::from_utf8_lossy(&line)) else {
            continue;
        };
        match out.iter_mut().find(|x| x.job_id == e.job_id) {
            Some(slot) => *slot = e,
            None => out.push(e),
        }
    }
    Ok(out)
}

impl FileQueue {
    /// `OpenFile`: creates the directory and loads the file.
    pub fn open(path: &Path) -> Result<FileQueue> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, "mkdir", e))?;
        }
        Ok(FileQueue {
            path: path.to_owned(),
            entries: Mutex::new(read_entries(path)?),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn append_locked(&self, entries: &mut Vec<Entry>, e: Entry) -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|x| io(&self.path, "open", x))?;
        f.write_all(e.to_line().as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(|x| io(&self.path, "append", x))?;
        match entries.iter_mut().find(|x| x.job_id == e.job_id) {
            Some(slot) => *slot = e,
            None => entries.push(e),
        }
        Ok(())
    }

    /// `Enqueue`: idempotent, an already known job keeps its state.
    pub fn enqueue(&self, job: &Job) -> Result<()> {
        let mut entries = self.lock();
        if entries.iter().any(|e| e.job_id == job.job_id) {
            return Ok(());
        }
        let e = Entry {
            job_id: job.job_id.clone(),
            booking_id: job.booking_id.clone(),
            job: job.clone(),
            state: PENDING.into(),
            updated_at: now(),
            ..Entry::default()
        };
        self.append_locked(&mut entries, e)
    }

    /// `MarkApplied`: records the apply result (creates the entry when unknown).
    pub fn mark_applied(
        &self,
        job_id: &str,
        ok: bool,
        err: &str,
        res: Option<&PmsResult>,
    ) -> Result<()> {
        let mut entries = self.lock();
        let mut e = entries
            .iter()
            .find(|x| x.job_id == job_id)
            .cloned()
            .unwrap_or_else(|| Entry {
                job_id: job_id.to_owned(),
                ..Entry::default()
            });
        e.state = APPLIED.into();
        e.ok = ok;
        e.last_error = err.to_owned();
        e.pms_result = res.cloned();
        e.applied_at = now();
        e.updated_at = e.applied_at.clone();
        self.append_locked(&mut entries, e)
    }

    /// `MarkAcked`: unknown ids are ignored.
    pub fn mark_acked(&self, job_id: &str) -> Result<()> {
        let mut entries = self.lock();
        let Some(mut e) = entries.iter().find(|x| x.job_id == job_id).cloned() else {
            return Ok(());
        };
        e.state = ACKED.into();
        e.updated_at = now();
        self.append_locked(&mut entries, e)
    }

    fn snapshot(&self, want: &str) -> Vec<Entry> {
        self.lock()
            .iter()
            .filter(|e| e.state == want)
            .cloned()
            .collect()
    }

    /// `Pending`: received but not applied.
    pub fn pending(&self) -> Vec<Entry> {
        self.snapshot(PENDING)
    }

    /// `Unacked`: applied but not acked.
    pub fn unacked(&self) -> Vec<Entry> {
        self.snapshot(APPLIED)
    }

    /// `Depth`: entries not acked yet.
    pub fn depth(&self) -> usize {
        self.lock().iter().filter(|e| e.state != ACKED).count()
    }

    /// `Compact`: rewrites the file without the acked entries (temp file, fsync, rename).
    pub fn compact(&self) -> Result<()> {
        let mut entries = self.lock();
        let live: Vec<Entry> = entries
            .iter()
            .filter(|e| e.state != ACKED)
            .cloned()
            .collect();
        let body: String = live.iter().map(Entry::to_line).collect();
        crate::config::write_atomic(&self.path, body.as_bytes())
            .map_err(|e| io(&self.path, "compact", e))?;
        *entries = live;
        Ok(())
    }

    /// Imports the unfinished jobs of the Go agent's `queue.jsonl` once: every pending or
    /// applied entry this queue does not know yet is appended with its state, so no job and
    /// no unsent ack is lost. `marker` records that the import ran; the Go file is never
    /// changed. Returns how many entries were imported.
    pub fn import_legacy(&self, legacy: &Path, marker: &Path) -> Result<usize> {
        if marker.exists() || !legacy.exists() {
            return Ok(0);
        }
        let old = read_entries(legacy)?;
        let mut entries = self.lock();
        let mut n = 0;
        for e in old {
            if e.state == ACKED || entries.iter().any(|x| x.job_id == e.job_id) {
                continue;
            }
            self.append_locked(&mut entries, e)?;
            n += 1;
        }
        drop(entries);
        let note = format!("imported {n} entries from {}\n", legacy.display());
        crate::config::write_atomic(marker, note.as_bytes()).map_err(|e| io(marker, "write", e))?;
        Ok(n)
    }
}

#[cfg(test)]
pub(crate) mod tests;
