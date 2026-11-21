//! The batch outbox `outbox.db` (port of `Outbox/SqliteOutbox.cs`; the targeted stream
//! resnapshots of `SqliteOutbox.Resnapshots.cs` are in [`super::resnapshots`]). Same schema,
//! pragmas, statements and transactions as the C# agent, so an `outbox.db` it wrote is
//! continued unchanged (and the other way round).
//!
//! ```text
//! outbox(id INTEGER PRIMARY KEY AUTOINCREMENT, stream TEXT, seq INTEGER, payload BLOB,
//!        bytes INTEGER, quarantined INTEGER DEFAULT 0, attempts INTEGER DEFAULT 0,
//!        created_utc TEXT)                         -- ux_outbox_stream_seq(stream, seq)
//! stream_state(stream TEXT PRIMARY KEY, last_seq INTEGER, watermark TEXT NULL)
//! meta(key TEXT PRIMARY KEY, value TEXT)           -- needs_resnapshot, recovery_resnapshot_*
//! stream_resnapshot(stream TEXT PRIMARY KEY, state, ord, reason, requested_utc,
//!        quarantined_at_start, through_seq, completed_utc)
//! ```

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use dl_sqlite::{Connection, TransactionBehavior};

use super::error::{OutboxError, Result};
use super::sql::{self, Param};
use crate::contracts::WireJson;
use crate::contracts::time::DateTimeOffset;
use crate::contracts::wire::{SyncBatch, wire_version};
use crate::reporting::{Logger, Value};
use crate::util::stj::{self, Style, Writer};

/// `MM.PdaOrder.SyncAgent.Outbox.SqliteOutbox`: the log category.
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Outbox.SqliteOutbox";

/// The C# constructor's schema script (the resnapshot table follows in a second script).
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS outbox(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  stream TEXT NOT NULL, seq INTEGER NOT NULL, payload BLOB NOT NULL,
  bytes INTEGER NOT NULL, quarantined INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0,
  created_utc TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS ux_outbox_stream_seq ON outbox(stream, seq);
CREATE TABLE IF NOT EXISTS stream_state(
  stream TEXT PRIMARY KEY, last_seq INTEGER NOT NULL, watermark TEXT NULL);
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// One queued batch (`OutboxItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    /// Row id (queue order).
    pub id: i64,
    /// Stream.
    pub stream: String,
    /// Per-stream sequence.
    pub seq: i64,
    /// The batch JSON (UTF-8).
    pub payload: Vec<u8>,
}

/// What [`SqliteOutbox::enqueue`] did (`EnqueueResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueResult {
    /// Stored.
    Queued,
    /// The size cap is reached: nothing stored, nothing dropped.
    CapReached,
}

/// What [`SqliteOutbox::resequence`] did: batches renumbered and quarantined batches set
/// aside (`ResequenceResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResequenceResult {
    /// Batches that got a new seq.
    pub renumbered: usize,
    /// Quarantined batches moved out of the stream's seq space.
    pub set_aside: usize,
}

/// Durable batch queue (SQLite WAL). Per-stream sequence numbers and the capture watermark
/// of each stream live in `stream_state`, written in the same transaction as the enqueue
/// and only ever advanced, so purging or capping the queue can never lose the
/// closed-ledger cursor. When the size cap is hit the queue stops accepting and raises
/// `needs_resnapshot`; already queued batches are never dropped.
pub struct SqliteOutbox {
    db: Mutex<Connection>,
    cap_bytes: i64,
    /// Stored bytes at the last cap refusal (-1 = none since the last queued batch).
    refused_at: AtomicI64,
    pub(super) log: Logger,
}

impl std::fmt::Debug for SqliteOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteOutbox")
            .field("cap_bytes", &self.cap_bytes)
            .finish_non_exhaustive()
    }
}

impl SqliteOutbox {
    /// Prefix of the stream a quarantined batch is moved to by [`SqliteOutbox::resequence`]:
    /// out of the seq space.
    pub const SET_ASIDE_PREFIX: &'static str = "!quarantined:";

    /// Opens (creating when absent, with its directory) the outbox at `path`; `cap_bytes`
    /// bounds the queued payload bytes.
    pub fn open(path: impl AsRef<Path>, cap_bytes: i64, log: &Logger) -> Result<SqliteOutbox> {
        let db = sql::open_store(
            path.as_ref(),
            &[
                SCHEMA,
                super::resnapshots::SCHEMA,
                super::key_repairs::SCHEMA,
            ],
        )?;
        Ok(SqliteOutbox {
            db: Mutex::new(db),
            cap_bytes,
            refused_at: AtomicI64::new(-1),
            log: log.for_category(CATEGORY),
        })
    }

    /// The connection (the C# `_gate` lock).
    pub(super) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn exec(&self, sql_text: &str, params: &[Param<'_>]) -> Result<usize> {
        sql::exec(&self.lock(), sql_text, params)
    }

    fn scalar(&self, sql_text: &str, params: &[Param<'_>]) -> Result<i64> {
        sql::scalar_i64(&self.lock(), sql_text, params)
    }

    pub(super) fn meta(&self, key: &str) -> Result<Option<String>> {
        sql::scalar_text(
            &self.lock(),
            "SELECT value FROM meta WHERE key=$k",
            &[("$k", &key)],
        )
    }

    fn set_meta(db: &Connection, key: &str, value: &str) -> Result<()> {
        sql::exec(
            db,
            "INSERT INTO meta(key,value) VALUES($k,$v) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            &[("$k", &key), ("$v", &value)],
        )?;
        Ok(())
    }

    /// A resnapshot of everything was requested and not yet taken by capture.
    pub fn needs_resnapshot(&self) -> Result<bool> {
        Ok(self.meta("needs_resnapshot")?.as_deref() == Some("1"))
    }

    /// Capture took the requested resnapshot.
    pub fn clear_needs_resnapshot(&self) -> Result<()> {
        Self::set_meta(&self.lock(), "needs_resnapshot", "0")
    }

    /// Requests a resnapshot of everything (logged as a warning).
    pub fn request_resnapshot(&self, reason: &str) -> Result<()> {
        Self::set_meta(&self.lock(), "needs_resnapshot", "1")?;
        self.log
            .warn("Resnapshot requested: {Reason}", &[Value::from(reason)]);
        Ok(())
    }

    /// Batches waiting (quarantined ones excluded); the heartbeat's `queue_depth`.
    pub fn depth(&self) -> Result<i64> {
        self.scalar("SELECT COUNT(*) FROM outbox WHERE quarantined=0", &[])
    }

    /// Payload bytes stored, quarantined batches included (what the cap counts).
    pub fn bytes(&self) -> Result<i64> {
        self.scalar("SELECT COALESCE(SUM(bytes),0) FROM outbox", &[])
    }

    /// The outbox cannot take a further batch: either the stored payload bytes reached
    /// `cap_bytes`, or `enqueue` refused a batch (`stored + len > cap_bytes`) and nothing
    /// has drained since (the stored bytes are still at or above the count at the refusal).
    /// The second case covers an outbox a few bytes under the cap, where every real batch
    /// is refused although `bytes() < cap_bytes`. An ack or purge that frees room clears it.
    pub fn is_full(&self) -> Result<bool> {
        let bytes = self.bytes()?;
        let refused = self.refused_at.load(Ordering::Relaxed);
        Ok(bytes >= self.cap_bytes || (refused >= 0 && bytes >= refused))
    }

    /// Batches parked by [`SqliteOutbox::quarantine`].
    pub fn quarantined_count(&self) -> Result<i64> {
        self.scalar("SELECT COUNT(*) FROM outbox WHERE quarantined=1", &[])
    }

    /// Highest sequence handed out for the stream (0 = none yet).
    pub fn last_seq(&self, stream: &str) -> Result<i64> {
        sql::scalar_long_or_zero(
            &self.lock(),
            "SELECT last_seq FROM stream_state WHERE stream=$s",
            &[("$s", &stream)],
        )
    }

    /// The capture watermark stored with the stream's last batch that carried one.
    pub fn watermark(&self, stream: &str) -> Result<Option<String>> {
        sql::scalar_text(
            &self.lock(),
            "SELECT watermark FROM stream_state WHERE stream=$s",
            &[("$s", &stream)],
        )
    }

    /// Assigns the next seq for `stream`, stores the batch `build(seq)` and its watermark
    /// atomically. `request_resnapshot_on_cap` is false for a producer that keeps unsent
    /// changes itself and retries (the capture runner): flagging a resnapshot there would
    /// re-send everything into the same full queue. (C# default: true.)
    pub fn enqueue(
        &self,
        stream: &str,
        build: impl FnOnce(i64) -> SyncBatch,
        request_resnapshot_on_cap: bool,
    ) -> Result<EnqueueResult> {
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let seq = sql::scalar_long_or_zero(
            &tx,
            "SELECT last_seq FROM stream_state WHERE stream=$s",
            &[("$s", &stream)],
        )? + 1;
        let batch = build(seq);
        let json = batch.to_json().into_bytes();
        if json.len() > wire_version::MAX_BODY_BYTES as usize {
            return Err(OutboxError::BatchTooLarge { bytes: json.len() });
        }
        let stored = sql::scalar_i64(&tx, "SELECT COALESCE(SUM(bytes),0) FROM outbox", &[])?;
        let len = i64::try_from(json.len()).unwrap_or(i64::MAX);
        if stored.saturating_add(len) > self.cap_bytes {
            tx.rollback()?;
            self.refused_at.store(stored, Ordering::Relaxed);
            if request_resnapshot_on_cap {
                Self::set_meta(&db, "needs_resnapshot", "1")?;
                let reason = format!("outbox cap of {} bytes reached", self.cap_bytes);
                self.log
                    .warn("Resnapshot requested: {Reason}", &[Value::from(reason)]);
            }
            return Ok(EnqueueResult::CapReached);
        }
        let created = DateTimeOffset::now_utc().to_round_trip_string();
        sql::exec(
            &tx,
            "INSERT INTO outbox(stream,seq,payload,bytes,created_utc) VALUES($s,$q,$p,$b,$t)",
            &[
                ("$s", &stream),
                ("$q", &seq),
                ("$p", &json),
                ("$b", &len),
                ("$t", &created),
            ],
        )?;
        sql::exec(
            &tx,
            "INSERT INTO stream_state(stream,last_seq,watermark) VALUES($s,$q,$w)
ON CONFLICT(stream) DO UPDATE SET last_seq=excluded.last_seq,
  watermark=COALESCE(excluded.watermark, stream_state.watermark)",
            &[("$s", &stream), ("$q", &seq), ("$w", &batch.watermark)],
        )?;
        tx.commit()?;
        self.refused_at.store(-1, Ordering::Relaxed);
        Ok(EnqueueResult::Queued)
    }

    /// Oldest batches first, per stream in seq order; quarantined items and the streams in
    /// `skip` (parked while they wait for a parent stream, ADR 0102) are left out.
    pub fn peek<S: AsRef<str>>(&self, max: i32, skip: &[S]) -> Result<Vec<OutboxItem>> {
        let names: Vec<String> = (0..skip.len()).map(|i| format!("$k{i}")).collect();
        let except = if names.is_empty() {
            String::new()
        } else {
            format!(" AND stream NOT IN ({})", names.join(","))
        };
        let text = format!(
            "SELECT id,stream,seq,payload FROM outbox WHERE quarantined=0{except} ORDER BY id LIMIT $n"
        );
        let streams: Vec<&str> = skip.iter().map(AsRef::as_ref).collect();
        let mut params: Vec<Param<'_>> = names
            .iter()
            .zip(streams.iter())
            .map(|(n, s)| (n.as_str(), s as &dyn dl_sqlite::ToSql))
            .collect();
        params.push(("$n", &max));
        sql::query(&self.lock(), &text, &params, |r| {
            Ok(OutboxItem {
                id: r.get(0)?,
                stream: r.get(1)?,
                seq: r.get(2)?,
                payload: r.get(3)?,
            })
        })
    }

    /// The cloud applied the batch: removes it.
    pub fn ack(&self, id: i64) -> Result<()> {
        self.exec("DELETE FROM outbox WHERE id=$i", &[("$i", &id)])?;
        Ok(())
    }

    /// The cloud already applied everything below `expected_seq` for the stream; returns
    /// the batches removed.
    pub fn ack_below(&self, stream: &str, expected_seq: i64) -> Result<usize> {
        self.exec(
            "DELETE FROM outbox WHERE stream=$s AND seq<$q",
            &[("$s", &stream), ("$q", &expected_seq)],
        )
    }

    /// The batch `seq` of the stream is queued (sendable or quarantined).
    pub fn has(&self, stream: &str, seq: i64) -> Result<bool> {
        Ok(self.scalar(
            "SELECT COUNT(*) FROM outbox WHERE stream=$s AND seq=$q",
            &[("$s", &stream), ("$q", &seq)],
        )? > 0)
    }

    /// The batch `seq` of the stream is queued but parked by [`SqliteOutbox::quarantine`].
    pub fn is_quarantined(&self, stream: &str, seq: i64) -> Result<bool> {
        Ok(self.scalar(
            "SELECT COUNT(*) FROM outbox WHERE stream=$s AND seq=$q AND quarantined=1",
            &[("$s", &stream), ("$q", &seq)],
        )? > 0)
    }

    /// Counts one more upload attempt of the batch.
    pub fn mark_attempt(&self, id: i64) -> Result<()> {
        self.exec(
            "UPDATE outbox SET attempts=attempts+1 WHERE id=$i",
            &[("$i", &id)],
        )?;
        Ok(())
    }

    /// Permanently rejected by the cloud: park it (kept for diagnostics) and, unless the
    /// caller paces resnapshots itself (`request_resnapshot` false), force a resnapshot.
    pub fn quarantine(&self, id: i64, reason: &str, request_resnapshot: bool) -> Result<()> {
        self.exec(
            "UPDATE outbox SET quarantined=1 WHERE id=$i",
            &[("$i", &id)],
        )?;
        if request_resnapshot {
            self.request_resnapshot(&format!("batch {id} quarantined: {reason}"))?;
        }
        Ok(())
    }

    /// Self-heal of a seq gap (AD1): the cloud expects `expected` on the stream, but no
    /// sendable batch carries it (it was quarantined or is gone). In one transaction:
    /// quarantined batches of the stream at or above `expected` move to
    /// [`SqliteOutbox::SET_ASIDE_PREFIX`] + stream (kept for diagnostics, keyed by their
    /// id), the queued batches are renumbered from `expected` in their original order (the
    /// seq inside the payload too), and the stream's last seq follows, so the next enqueue
    /// continues the new numbering.
    pub fn resequence(&self, stream: &str, expected: i64) -> Result<ResequenceResult> {
        if expected < 1 {
            return Err(OutboxError::InvalidArgument(format!(
                "expected ('{expected}') must be greater than or equal to '1'. (Parameter 'expected')\nActual value was {expected}."
            )));
        }
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let aside = format!("{}{stream}", Self::SET_ASIDE_PREFIX);
        let set_aside = sql::exec(
            &tx,
            "UPDATE outbox SET stream=$a, seq=id WHERE stream=$s AND seq>=$q AND quarantined=1",
            &[("$a", &aside), ("$s", &stream), ("$q", &expected)],
        )?;
        let queued = sql::query(
            &tx,
            "SELECT id,seq,payload FROM outbox WHERE stream=$s AND seq>=$q ORDER BY seq",
            &[("$s", &stream), ("$q", &expected)],
            |r| Ok((r.get::<i64>(0)?, r.get::<i64>(1)?, r.get::<Vec<u8>>(2)?)),
        )?;
        // Ascending order: each target seq is free, every batch below it already moved down.
        let mut renumbered = 0;
        for (next, (id, seq, payload)) in (expected..).zip(&queued) {
            if *seq != next {
                let rewritten = with_seq(payload, next)?;
                let len = i64::try_from(rewritten.len()).unwrap_or(i64::MAX);
                sql::exec(
                    &tx,
                    "UPDATE outbox SET seq=$n, payload=$p, bytes=$b WHERE id=$i",
                    &[("$n", &next), ("$p", &rewritten), ("$b", &len), ("$i", id)],
                )?;
                renumbered += 1;
            }
        }
        let last = expected - 1 + i64::try_from(queued.len()).unwrap_or(i64::MAX);
        sql::exec(
            &tx,
            "UPDATE stream_state SET last_seq=$l WHERE stream=$s",
            &[("$l", &last), ("$s", &stream)],
        )?;
        tx.commit()?;
        Ok(ResequenceResult {
            renumbered,
            set_aside,
        })
    }

    /// When the last automatic recovery resnapshot was requested (survives restarts).
    pub fn last_recovery_resnapshot(&self) -> Result<Option<DateTimeOffset>> {
        match self.meta("recovery_resnapshot_utc")? {
            None => Ok(None),
            Some(s) => parse_round_trip(&s).map(Some),
        }
    }

    /// A recovery resnapshot is owed but was held back by the cooldown.
    pub fn recovery_resnapshot_deferred(&self) -> Result<bool> {
        Ok(self.meta("recovery_resnapshot_deferred")?.as_deref() == Some("1"))
    }

    /// Requests a resnapshot as an automatic recovery and records when.
    pub fn request_recovery_resnapshot(&self, reason: &str, at: DateTimeOffset) -> Result<()> {
        let db = self.lock();
        Self::set_meta(&db, "needs_resnapshot", "1")?;
        self.log
            .warn("Resnapshot requested: {Reason}", &[Value::from(reason)]);
        Self::set_meta(&db, "recovery_resnapshot_utc", &at.to_round_trip_string())?;
        Self::set_meta(&db, "recovery_resnapshot_deferred", "0")
    }

    /// Records that a recovery resnapshot is owed but held back.
    pub fn defer_recovery_resnapshot(&self) -> Result<()> {
        Self::set_meta(&self.lock(), "recovery_resnapshot_deferred", "1")
    }
}

/// `DateTimeOffset.Parse(s, InvariantCulture, RoundtripKind)` of a stored "O" text.
pub(super) fn parse_round_trip(s: &str) -> Result<DateTimeOffset> {
    DateTimeOffset::parse(s).ok_or_else(|| {
        OutboxError::Corrupt(format!(
            "The string '{s}' was not recognized as a valid DateTime."
        ))
    })
}

/// `JsonNode.Parse(payload)["seq"] = seq` then `ToJsonString()`: member order kept (a
/// missing `seq` is appended), numbers as written, strings re-escaped with the default
/// encoder, compact.
fn with_seq(payload: &[u8], seq: i64) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(payload)
        .map_err(|_| OutboxError::Corrupt("A queued batch is not valid UTF-8.".to_owned()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut node = stj::parse(text).map_err(|e| OutboxError::Corrupt(e.to_string()))?;
    let Some(obj) = node.as_object_mut() else {
        return Err(OutboxError::Corrupt(
            "A queued batch is not a JSON object.".to_owned(),
        ));
    };
    obj.insert("seq", seq);
    let mut w = Writer::new(Style::Compact);
    w.value(&node);
    Ok(w.finish().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::with_seq;

    #[test]
    fn with_seq_rewrites_in_place_and_reescapes_like_json_node() {
        let out = with_seq(br#"{"agent_id":"a","seq":1,"n":1.50,"s":"\u00e9+"}"#, 7).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#"{"agent_id":"a","seq":7,"n":1.50,"s":"\u00E9\u002B"}"#
        );
        assert!(with_seq(b"[1]", 2).is_err());
        assert!(with_seq(b"{", 2).is_err());
    }
}
