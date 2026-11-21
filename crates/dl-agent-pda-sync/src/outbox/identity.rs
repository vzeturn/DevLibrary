//! Re-stamping queued batches after the agent identity changed (`pda-enroll` re-attach).
//!
//! A batch carries `agent_id` inside its JSON, stamped at enqueue. After a re-enrol the
//! cloud refuses every batch stamped with the old id (HTTP 400 `PDA_VALID`), so the queue
//! is rewritten once at start-up, in one transaction, and the recovery resnapshot
//! cooldown is cleared so the follow-up resnapshot is not held back.

use dl_sqlite::TransactionBehavior;

use super::batches::SqliteOutbox;
use super::error::{OutboxError, Result};
use super::sql;
use crate::contracts::time::DateTimeOffset;
use crate::reporting::Value as LogValue;
use crate::util::stj::{self, Style, Writer};

/// What [`SqliteOutbox::adopt_agent_id`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdoptResult {
    /// Queued batches whose payload `agent_id` was rewritten.
    pub restamped: usize,
    /// The identity differs from the one recorded (or a batch carried another id).
    pub changed: bool,
}

/// Sets `agent_id` in `payload` when it differs; `None` when it is already `agent_id`.
fn restamp(payload: &[u8], agent_id: &str) -> Result<Option<Vec<u8>>> {
    let text = std::str::from_utf8(payload)
        .map_err(|_| OutboxError::Corrupt("A queued batch is not valid UTF-8.".to_owned()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut node = stj::parse(text).map_err(|e| OutboxError::Corrupt(e.to_string()))?;
    let Some(obj) = node.as_object_mut() else {
        return Err(OutboxError::Corrupt(
            "A queued batch is not a JSON object.".to_owned(),
        ));
    };
    if obj.get("agent_id").and_then(|v| v.as_str()) == Some(agent_id) {
        return Ok(None);
    }
    obj.insert("agent_id", agent_id);
    let mut w = Writer::new(Style::Compact);
    w.value(&node);
    Ok(Some(w.finish().into_bytes()))
}

impl SqliteOutbox {
    /// Makes the queue match the current identity: every not quarantined batch is
    /// re-stamped with `agent_id`, the identity is recorded in `meta`, and when it changed
    /// the recovery resnapshot cooldown is cleared (a deferred resnapshot is requested
    /// now). One transaction.
    pub fn adopt_agent_id(&self, agent_id: &str, at: DateTimeOffset) -> Result<AdoptResult> {
        let mut db = self.lock();
        let tx = db.transaction_with(TransactionBehavior::Immediate)?;
        let queued = sql::query(
            &tx,
            "SELECT id,payload FROM outbox WHERE quarantined=0 ORDER BY id",
            &[],
            |r| Ok((r.get::<i64>(0)?, r.get::<Vec<u8>>(1)?)),
        )?;
        let mut restamped = 0;
        for (id, payload) in &queued {
            if let Some(rewritten) = restamp(payload, agent_id)? {
                let len = i64::try_from(rewritten.len()).unwrap_or(i64::MAX);
                sql::exec(
                    &tx,
                    "UPDATE outbox SET payload=$p, bytes=$b WHERE id=$i",
                    &[("$p", &rewritten), ("$b", &len), ("$i", id)],
                )?;
                restamped += 1;
            }
        }
        let previous = sql::scalar_text(
            &tx,
            "SELECT value FROM meta WHERE key=$k",
            &[("$k", &"agent_id")],
        )?;
        let changed = restamped > 0 || previous.as_deref().is_some_and(|p| p != agent_id);
        if previous.as_deref() != Some(agent_id) {
            Self::set_meta(&tx, "agent_id", agent_id)?;
        }
        if changed {
            let deferred = sql::scalar_text(
                &tx,
                "SELECT value FROM meta WHERE key=$k",
                &[("$k", &"recovery_resnapshot_deferred")],
            )?;
            sql::exec(
                &tx,
                "DELETE FROM meta WHERE key=$k",
                &[("$k", &"recovery_resnapshot_utc")],
            )?;
            if deferred.as_deref() == Some("1") {
                Self::set_meta(&tx, "needs_resnapshot", "1")?;
                Self::set_meta(&tx, "recovery_resnapshot_utc", &at.to_round_trip_string())?;
                Self::set_meta(&tx, "recovery_resnapshot_deferred", "0")?;
            }
        }
        tx.commit()?;
        if changed {
            self.log.warn(
                "Agent identity changed from {Old} to {New}: {Restamped} queued batches re-stamped, recovery resnapshot cooldown cleared",
                &[
                    LogValue::from(previous.as_deref().unwrap_or("unknown")),
                    LogValue::from(agent_id),
                    LogValue::from(i64::try_from(restamped).unwrap_or(i64::MAX)),
                ],
            );
        }
        Ok(AdoptResult { restamped, changed })
    }
}
