//! Targeted stream resnapshots (ADR 0102, port of `Outbox/SqliteOutbox.Resnapshots.cs`),
//! kept in `stream_resnapshot` so a restart neither loses nor repeats one.
//!
//! A row walks `pending` (requested) → `running` (capture forgot the stream's rows) →
//! `captured` (the initial load re-sent them, up to `through_seq`) → `done` once every
//! batch up to `through_seq` left the queue with no new quarantine → `reported` after a
//! heartbeat carried it to the cloud, which then supersedes the stream's rejected batches.
//! A batch of the stream quarantined meanwhile makes it `dirty` instead: never reported.

use std::time::Duration;

use dl_sqlite::{Connection, TransactionBehavior};

use super::batches::{SqliteOutbox, parse_round_trip};
use super::error::Result;
use super::sql;
use crate::config::codes;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::StreamResnapshot;
use crate::reporting::Value;

/// The `stream_resnapshot` schema script.
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS stream_resnapshot(
  stream TEXT PRIMARY KEY, state TEXT NOT NULL, ord INTEGER NOT NULL, reason TEXT NOT NULL, requested_utc TEXT NOT NULL,
  quarantined_at_start INTEGER NULL, through_seq INTEGER NULL, completed_utc TEXT NULL);
";

const STATE_PENDING: &str = "pending";
const STATE_RUNNING: &str = "running";
const STATE_CAPTURED: &str = "captured";
const STATE_DONE: &str = "done";
const STATE_REPORTED: &str = "reported";
const STATE_DIRTY: &str = "dirty";

/// Most finished resnapshots one heartbeat carries.
pub const MAX_REPORTED_RESNAPSHOTS: usize = 64;

/// How long reported and dirty rows are kept for diagnostics.
pub const RESNAPSHOT_HISTORY: Duration = Duration::from_secs(7 * 24 * 3600);

/// What [`SqliteOutbox::request_stream_resnapshot`] scheduled and what was already
/// waiting for capture (`StreamResnapshotRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamResnapshotRequest {
    /// Streams scheduled now.
    pub added: Vec<String>,
    /// Streams already pending or running (left as they are).
    pub already_pending: Vec<String>,
}

struct ResnapshotRow {
    stream: String,
    state: String,
    quarantined_at_start: Option<i64>,
    through_seq: Option<i64>,
    completed_utc: Option<DateTimeOffset>,
}

/// `at.ToUniversalTime().ToString("O")`.
fn utc(at: DateTimeOffset) -> String {
    at.to_utc().to_round_trip_string()
}

/// Distinct by ordinal comparison, first occurrence order (`Distinct(StringComparer.Ordinal)`).
fn distinct<S: AsRef<str>>(streams: &[S]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for s in streams {
        if !out.contains(&s.as_ref()) {
            out.push(s.as_ref());
        }
    }
    out
}

/// Quarantined batches of the stream, including those a gap renumber set aside.
fn quarantined_of(db: &Connection, stream: &str) -> Result<i64> {
    let aside = format!("{}{stream}", SqliteOutbox::SET_ASIDE_PREFIX);
    sql::scalar_i64(
        db,
        "SELECT COUNT(*) FROM outbox WHERE quarantined=1 AND (stream=$s OR stream=$a)",
        &[("$s", &stream), ("$a", &aside)],
    )
}

fn resnapshot_rows(db: &Connection) -> Result<Vec<ResnapshotRow>> {
    let raw = sql::query(
        db,
        "SELECT stream,state,quarantined_at_start,through_seq,completed_utc FROM stream_resnapshot ORDER BY ord",
        &[],
        |r| {
            Ok((
                r.get::<String>(0)?,
                r.get::<String>(1)?,
                r.get::<Option<i64>>(2)?,
                r.get::<Option<i64>>(3)?,
                r.get::<Option<String>>(4)?,
            ))
        },
    )?;
    raw.into_iter()
        .map(|(stream, state, q, t, c)| {
            Ok(ResnapshotRow {
                stream,
                state,
                quarantined_at_start: q,
                through_seq: t,
                completed_utc: c.as_deref().map(parse_round_trip).transpose()?,
            })
        })
        .collect()
}

impl SqliteOutbox {
    /// Schedules a resnapshot of `streams`, in that order. A stream already pending or
    /// running is left as it is (its pass re-sends every row anyway); a finished one
    /// starts over.
    pub fn request_stream_resnapshot<S: AsRef<str>>(
        &self,
        streams: &[S],
        reason: &str,
        now: DateTimeOffset,
    ) -> Result<StreamResnapshotRequest> {
        let mut result = StreamResnapshotRequest::default();
        {
            let mut db = self.lock();
            let tx = db.transaction_with(TransactionBehavior::Immediate)?;
            let at = utc(now);
            for stream in distinct(streams) {
                let state = sql::scalar_text(
                    &tx,
                    "SELECT state FROM stream_resnapshot WHERE stream=$s",
                    &[("$s", &stream)],
                )?;
                if matches!(state.as_deref(), Some(STATE_PENDING | STATE_RUNNING)) {
                    result.already_pending.push(stream.to_owned());
                    continue;
                }
                sql::exec(
                    &tx,
                    "INSERT INTO stream_resnapshot(stream,state,ord,reason,requested_utc)
VALUES($s,'pending',(SELECT COALESCE(MAX(ord),0)+1 FROM stream_resnapshot),$r,$t)
ON CONFLICT(stream) DO UPDATE SET state='pending', ord=excluded.ord, reason=excluded.reason,
  requested_utc=excluded.requested_utc, quarantined_at_start=NULL, through_seq=NULL, completed_utc=NULL",
                    &[("$s", &stream), ("$r", &reason), ("$t", &at)],
                )?;
                result.added.push(stream.to_owned());
            }
            tx.commit()?;
        }
        if !result.added.is_empty() {
            self.log.info(
                "Resnapshot of streams {Streams} scheduled: {Reason}",
                &[Value::from(result.added.join(", ")), Value::from(reason)],
            );
        }
        Ok(result)
    }

    /// Streams waiting for capture to resend them, in request order.
    pub fn pending_stream_resnapshot(&self) -> Result<Vec<String>> {
        sql::query(
            &self.lock(),
            "SELECT stream FROM stream_resnapshot WHERE state='pending' ORDER BY ord",
            &[],
            |r| r.get::<String>(0),
        )
    }

    /// Capture forgot what it sent for `streams` (targeted or full reset): they are running
    /// from now on, and the quarantined batches they already have are counted so a new one
    /// can be told apart.
    pub fn begin_stream_resnapshot<S: AsRef<str>>(
        &self,
        streams: &[S],
        now: DateTimeOffset,
    ) -> Result<()> {
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let at = utc(now);
        for stream in distinct(streams) {
            let q = quarantined_of(&tx, stream)?;
            sql::exec(
                &tx,
                "INSERT INTO stream_resnapshot(stream,state,ord,reason,requested_utc,quarantined_at_start)
VALUES($s,'running',(SELECT COALESCE(MAX(ord),0)+1 FROM stream_resnapshot),'full resnapshot',$t,$q)
ON CONFLICT(stream) DO UPDATE SET state='running', quarantined_at_start=excluded.quarantined_at_start,
  through_seq=NULL, completed_utc=NULL",
                &[("$s", &stream), ("$t", &at), ("$q", &q)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The initial load finished: every running stream was re-sent up to its current last
    /// seq.
    pub fn stream_resnapshot_captured(&self) -> Result<()> {
        sql::exec(
            &self.lock(),
            "UPDATE stream_resnapshot SET state='captured',
  through_seq=COALESCE((SELECT last_seq FROM stream_state WHERE stream_state.stream=stream_resnapshot.stream),0)
WHERE state='running'",
            &[],
        )?;
        Ok(())
    }

    /// Moves captured streams on (`done` once their batches up to `through_seq` are sent,
    /// `dirty` when one was quarantined), prunes old history and returns the finished
    /// resnapshots the cloud has not been told about yet (oldest first, at most
    /// [`MAX_REPORTED_RESNAPSHOTS`]).
    pub fn finished_stream_resnapshots(
        &self,
        now: DateTimeOffset,
    ) -> Result<Vec<StreamResnapshot>> {
        let mut dirty = Vec::new();
        let finished;
        {
            let mut db = self.lock();
            let tx = db.transaction_with(TransactionBehavior::Immediate)?;
            let at = utc(now);
            let history = i64::try_from(RESNAPSHOT_HISTORY.as_secs()).unwrap_or(i64::MAX)
                * crate::contracts::time::TICKS_PER_SECOND;
            for row in resnapshot_rows(&tx)? {
                if row.state == STATE_CAPTURED
                    && let Some(through) = row.through_seq
                {
                    if quarantined_of(&tx, &row.stream)? > row.quarantined_at_start.unwrap_or(0) {
                        sql::exec(
                            &tx,
                            "UPDATE stream_resnapshot SET state='dirty', completed_utc=$t WHERE stream=$s",
                            &[("$s", &row.stream), ("$t", &at)],
                        )?;
                        dirty.push(row.stream);
                        continue;
                    }
                    if sql::scalar_i64(
                        &tx,
                        "SELECT COUNT(*) FROM outbox WHERE stream=$s AND quarantined=0 AND seq<=$q",
                        &[("$s", &row.stream), ("$q", &through)],
                    )? > 0
                    {
                        continue;
                    }
                    // A gap renumber can lower the stream's numbering; never claim a seq the stream does not reach.
                    let last = sql::scalar_i64(
                        &tx,
                        "SELECT COALESCE(MAX(last_seq),0) FROM stream_state WHERE stream=$s",
                        &[("$s", &row.stream)],
                    )?;
                    sql::exec(
                        &tx,
                        "UPDATE stream_resnapshot SET state='done', through_seq=$q, completed_utc=$t WHERE stream=$s",
                        &[("$s", &row.stream), ("$q", &through.min(last)), ("$t", &at)],
                    )?;
                } else if (row.state == STATE_REPORTED || row.state == STATE_DIRTY)
                    && let Some(done_at) = row.completed_utc
                    && now.ticks_since(done_at) > history
                {
                    sql::exec(
                        &tx,
                        "DELETE FROM stream_resnapshot WHERE stream=$s",
                        &[("$s", &row.stream)],
                    )?;
                }
            }
            let mut done: Vec<StreamResnapshot> = resnapshot_rows(&tx)?
                .into_iter()
                .filter_map(
                    |r| match (r.state.as_str(), r.completed_utc, r.through_seq) {
                        (STATE_DONE, Some(completed_at_utc), Some(through_seq)) => {
                            Some(StreamResnapshot {
                                stream: r.stream,
                                through_seq,
                                completed_at_utc,
                            })
                        }
                        _ => None,
                    },
                )
                .collect();
            done.sort_by_key(|r| r.completed_at_utc);
            done.truncate(MAX_REPORTED_RESNAPSHOTS);
            finished = done;
            tx.commit()?;
        }
        for stream in dirty {
            self.log.warn(
                "Resnapshot of stream {Stream} is not clean ({Code}): a batch it re-sent was quarantined; its rejected batches stay open in the cloud",
                &[Value::from(stream), Value::from(codes::SYNC_STREAM_RESNAPSHOT)],
            );
        }
        Ok(finished)
    }

    /// A heartbeat carried these finished resnapshots to the cloud.
    pub fn mark_stream_resnapshots_reported(&self, sent: &[StreamResnapshot]) -> Result<()> {
        for r in sent {
            let at = utc(r.completed_at_utc);
            sql::exec(
                &self.lock(),
                "UPDATE stream_resnapshot SET state='reported' WHERE stream=$s AND state='done' AND completed_utc=$t",
                &[("$s", &r.stream), ("$t", &at)],
            )?;
        }
        Ok(())
    }

    /// When the last clean resnapshot of `stream` finished, or `None`.
    pub fn stream_resnapshot_finished_at(&self, stream: &str) -> Result<Option<DateTimeOffset>> {
        let text = sql::scalar_text(
            &self.lock(),
            "SELECT completed_utc FROM stream_resnapshot WHERE stream=$s AND state IN ('done','reported')",
            &[("$s", &stream)],
        )?;
        text.as_deref().map(parse_round_trip).transpose()
    }
}
