//! [`Journaled`]: the executor every op goes through, in front of the SQL executor.
//!
//! * `replay: true`: answered from the journal (`replayed: true`) or `rejected`
//!   `REQUEST_KEY_UNKNOWN`; the inner executor never runs.
//! * A keyed write already in the journal is answered from it as well (exactly once even
//!   if the cloud sends a key again without `replay`).
//! * A keyed write the inner executor `applied` is recorded after its commit. A journal at
//!   its cap refuses the write before it runs (`failed` `JOURNAL_FULL`) and raises a
//!   problem report; a journal that cannot be opened refuses keyed writes the same way.
//! * Two ops with the same key at once: the second is answered `failed` `AGENT_BUSY`.
//! * Reads and unkeyed ops pass straight through.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use dl_json::Map;

use super::{Journal, JournalError};
use crate::attrs;
use crate::ctx::Ctx;
use crate::exec::{Executor, failed, rejected};
use crate::log::Logger;
use crate::telemetry::{self, Level, Telemetry};
use crate::wire::{self, Op, OpResult};

/// Failed: the journal is at its cap; the keyed write was not run.
pub const CODE_JOURNAL_FULL: &str = "JOURNAL_FULL";
/// Failed: the journal cannot be opened or read; the keyed write was not run.
pub const CODE_JOURNAL_UNAVAILABLE: &str = "JOURNAL_UNAVAILABLE";

/// The journal in front of an executor.
pub struct Journaled {
    inner: Arc<dyn Executor>,
    journal: Option<Arc<Journal>>,
    inflight: Mutex<HashSet<String>>,
    log: Logger,
    telemetry: Option<Arc<Telemetry>>,
}

impl Journaled {
    /// `inner` behind `journal` (`None`: keyed writes and replays are refused).
    pub fn new(
        inner: Arc<dyn Executor>,
        journal: Option<Arc<Journal>>,
        log: Logger,
        telemetry: Option<Arc<Telemetry>>,
    ) -> Journaled {
        Journaled {
            inner,
            journal,
            inflight: Mutex::new(HashSet::new()),
            log,
            telemetry,
        }
    }

    fn problem(&self, level: Level, op: &Op, err: &JournalError) {
        self.log.error(
            "agent.journal.failed",
            attrs!["op_id" => &op.id, "op" => &op.op, "reason" => err.to_string()],
        );
        if let Some(t) = &self.telemetry {
            let mut m = Map::new();
            m.insert("op", op.op.as_str());
            m.insert("op_id", op.id.as_str());
            if let Some(j) = &self.journal {
                let h = j.health();
                m.insert("dir", h.dir.display().to_string());
                m.insert("entries", h.entries);
                m.insert("bytes", h.bytes);
            }
            t.report(level, telemetry::CODE_JOURNAL_FAILED, &err.to_string(), m);
        }
    }

    /// The answer the journal gives for `key` (the stored result, replayed, or a refusal
    /// when the entry belongs to another op or cannot be read), or `None` on a miss.
    fn lookup(&self, j: &Journal, op: &Op, key: &str) -> Option<OpResult> {
        match j.get(key) {
            Ok(None) => None,
            Ok(Some(e)) if e.op != op.op => Some(rejected(
                &op.id,
                wire::ERR_OP_REJECTED,
                "request_key_conflict",
            )),
            Ok(Some(e)) => {
                let mut r = e.result;
                r.id = op.id.clone();
                r.replayed = Some(true);
                Some(r)
            }
            Err(e) => {
                self.problem(Level::Error, op, &e);
                Some(match e {
                    JournalError::Corrupt(_) => {
                        rejected(&op.id, wire::ERR_OP_REJECTED, "journal_corrupt")
                    }
                    _ => failed(&op.id, CODE_JOURNAL_UNAVAILABLE, "journal_unreadable"),
                })
            }
        }
    }

    fn keyed(&self, ctx: &Ctx, op: &Op, key: &str) -> OpResult {
        let Some(j) = &self.journal else {
            if op.is_replay() {
                return rejected(
                    &op.id,
                    wire::ERR_REQUEST_KEY_UNKNOWN,
                    "no op journal on this agent",
                );
            }
            return failed(&op.id, CODE_JOURNAL_UNAVAILABLE, "journal_unavailable");
        };
        match self.lookup(j, op, key) {
            Some(r) => return r,
            None if op.is_replay() => {
                return rejected(
                    &op.id,
                    wire::ERR_REQUEST_KEY_UNKNOWN,
                    "request key not in the op journal",
                );
            }
            None => {}
        }
        let h = j.health();
        if h.full() {
            let err = JournalError::Full {
                entries: h.entries,
                bytes: h.bytes,
            };
            // Expired entries may have made room.
            if j.prune().is_err() || j.health().full() {
                self.problem(Level::Critical, op, &err);
                return failed(&op.id, CODE_JOURNAL_FULL, "op journal full");
            }
        }
        let res = self.inner.execute(ctx, op);
        if res.status == wire::STATUS_APPLIED
            && let Err(e) = j.put(key, &op.op, &res)
        {
            // Committed already: answer applied; a later replay of this key is answered
            // REQUEST_KEY_UNKNOWN and the cloud keeps the outcome unknown.
            let level = if matches!(e, JournalError::Full { .. }) {
                Level::Critical
            } else {
                Level::Error
            };
            self.problem(level, op, &e);
        }
        res
    }
}

impl Executor for Journaled {
    fn contract_max(&self) -> i64 {
        self.inner.contract_max()
    }

    fn execute(&self, ctx: &Ctx, op: &Op) -> OpResult {
        let Some(key) = op.request_key.clone() else {
            if op.is_replay() {
                return rejected(
                    &op.id,
                    wire::ERR_REQUEST_KEY_UNKNOWN,
                    "replay without a request key",
                );
            }
            return self.inner.execute(ctx, op);
        };
        let Some(_running) = Running::take(&self.inflight, &key) else {
            return failed(
                &op.id,
                wire::ERR_AGENT_BUSY,
                "an op with the same request key is still running",
            );
        };
        self.keyed(ctx, op, &key)
    }
}

/// A request key marked running until dropped (also on a panic of the inner executor).
struct Running<'a> {
    set: &'a Mutex<HashSet<String>>,
    key: String,
}

impl<'a> Running<'a> {
    fn take(set: &'a Mutex<HashSet<String>>, key: &str) -> Option<Running<'a>> {
        let fresh = set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned());
        fresh.then(|| Running {
            set,
            key: key.to_owned(),
        })
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}
