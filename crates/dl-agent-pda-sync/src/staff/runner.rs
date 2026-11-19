//! Port of `Staff/StaffSyncRunner.cs`: one staff sync cycle (ADR 0101).

use super::StaffError;
use super::cloud::{StaffCloud, StaffCloudStatus};
use super::pin_key::PinKeyProvider;
use super::source::StaffSource;
use crate::config::time::{SharedClock, Timestamp};
use crate::contracts::WireJson;
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::utf16_len;
use crate::contracts::wire::{StaffClerk, StaffSnapshot, staff_sync_limits};
use crate::reporting::{Exception, Logger, Value};
use crate::util::ctx::Ctx;
use crate::util::stj::{Style, Writer, eq_ignore_case};
use dl_hash::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The logger category (`ILogger<StaffSyncRunner>`).
pub const CATEGORY: &str = "MM.PdaOrder.SyncAgent.Staff.StaffSyncRunner";

/// The same list is sent again after this long (`ResendAfter`), so a restored cloud
/// catches up.
pub const RESEND_AFTER: Duration = Duration::from_secs(24 * 3600);

/// How a cycle ended (`StaffSyncOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaffSyncOutcome {
    /// The cloud applied the clerk list.
    Sent,
    /// Same list as the last one the cloud applied, within [`RESEND_AFTER`]: nothing sent.
    Unchanged,
    /// The store has no clerk (or none survived the checks): nothing sent, since an empty
    /// list would disable everyone.
    Empty,
    /// The PIN key or the send failed; retried on the next cycle.
    Failed,
}

/// What `staff-sync.json` holds (`StaffSyncState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaffSyncState {
    /// SHA-256 (lowercase hex) of the last applied list; `None` when the file had null.
    pub list_digest: Option<String>,
    /// When it was applied.
    pub sent_at: Timestamp,
}

impl StaffSyncState {
    /// `JsonSerializer.Serialize(state)` with the default options: PascalCase, compact.
    pub fn to_json(&self) -> String {
        let mut w = Writer::new(Style::Compact);
        w.begin_object();
        w.key("ListDigest");
        match &self.list_digest {
            Some(d) => w.string(d),
            None => w.null(),
        };
        w.key("SentAt")
            .date_time_offset(self.sent_at.unix_nanos(), self.sent_at.offset_minutes());
        w.end_object();
        w.finish()
    }

    /// `JsonSerializer.Deserialize<StaffSyncState>` with the default options
    /// (case-sensitive names, missing members default, JSON null is `Ok(None)`).
    pub fn from_json(json: &str) -> Result<Option<StaffSyncState>, String> {
        let v = crate::util::stj::parse(json).map_err(|e| e.to_string())?;
        let m = match &v {
            dl_json::Value::Null => return Ok(None),
            dl_json::Value::Object(m) => m,
            _ => {
                return Err(
                    "The JSON value could not be converted to MM.PdaOrder.SyncAgent.Staff.StaffSyncRunner+StaffSyncState. Path: $".to_owned(),
                );
            }
        };
        let get = |n: &str| {
            m.iter()
                .rev()
                .find(|(k, _)| k.as_str() == n)
                .map(|(_, x)| x)
        };
        let list_digest = match get("ListDigest") {
            None | Some(dl_json::Value::Null) => None,
            Some(dl_json::Value::String(s)) => Some(s.clone()),
            Some(_) => {
                return Err(
                    "The JSON value could not be converted to System.String. Path: $.ListDigest"
                        .to_owned(),
                );
            }
        };
        let sent_at = match get("SentAt") {
            None => Timestamp::default(),
            Some(dl_json::Value::String(s)) => Timestamp::parse_iso(s).ok_or_else(|| {
                "The JSON value could not be converted to System.DateTimeOffset. Path: $.SentAt"
                    .to_owned()
            })?,
            Some(_) => {
                return Err("The JSON value could not be converted to System.DateTimeOffset. Path: $.SentAt".to_owned());
            }
        };
        Ok(Some(StaffSyncState {
            list_digest,
            sent_at,
        }))
    }
}

/// One staff sync cycle (`StaffSyncRunner`): PIN key, POS clerk list (already hashed),
/// then a send only when the list changed since the last applied one (a SHA-256 digest in
/// `staff-sync.json`), and at least once a day. A key the cloud no longer uses is dropped
/// and fetched again once. Logs carry counts and statuses only.
pub struct StaffSyncRunner {
    source: Arc<dyn StaffSource>,
    cloud: Arc<dyn StaffCloud>,
    keys: Arc<PinKeyProvider>,
    state_file: PathBuf,
    agent_id: String,
    agent_version: String,
    clock: SharedClock,
    log: Logger,
}

impl std::fmt::Debug for StaffSyncRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaffSyncRunner")
            .field("state_file", &self.state_file)
            .finish_non_exhaustive()
    }
}

/// The parts of a [`StaffSyncRunner`].
pub struct StaffSyncParts {
    /// The POS clerk list.
    pub source: Arc<dyn StaffSource>,
    /// The cloud calls.
    pub cloud: Arc<dyn StaffCloud>,
    /// The PIN key.
    pub keys: Arc<PinKeyProvider>,
    /// `{DataDir}\staff-sync.json`.
    pub state_file: PathBuf,
    /// The agent id.
    pub agent_id: String,
    /// The agent version.
    pub agent_version: String,
    /// The clock.
    pub clock: SharedClock,
}

impl StaffSyncRunner {
    /// A runner over `parts`, logging under [`CATEGORY`].
    pub fn new(parts: StaffSyncParts, log: &Logger) -> StaffSyncRunner {
        StaffSyncRunner {
            source: parts.source,
            cloud: parts.cloud,
            keys: parts.keys,
            state_file: parts.state_file,
            agent_id: parts.agent_id,
            agent_version: parts.agent_version,
            clock: parts.clock,
            log: log.for_category(CATEGORY),
        }
    }

    /// Runs one cycle (`RunOnceAsync`). An error is what the C# code lets escape (a SQL
    /// failure, the secret store, the state file write, cancellation).
    pub fn run_once(&self, ctx: &Ctx) -> Result<StaffSyncOutcome, StaffError> {
        for attempt in 1..=2 {
            let key = self.keys.get(ctx)?;
            let pin_key = match (&key.status, &key.pin_key) {
                (StaffCloudStatus::Ok, Some(k)) => k.clone(),
                _ => {
                    self.log.warn(
                        "Staff sync: the PIN key could not be fetched ({Status}, HTTP {HttpStatus})",
                        &[key.status.name().into(), Value::Int(i64::from(key.http_status))],
                    );
                    return Ok(StaffSyncOutcome::Failed);
                }
            };

            let clerks = self.clean(self.source.read(&pin_key.key, ctx)?);
            if clerks.is_empty() {
                self.log.warn(
                    "Staff sync: the POS has no clerk to send; nothing sent",
                    &[],
                );
                return Ok(StaffSyncOutcome::Empty);
            }
            let count = clerks.len();
            let snapshot = StaffSnapshot {
                agent_id: self.agent_id.clone(),
                agent_version: self.agent_version.clone(),
                key_id: pin_key.key_id.clone(),
                clerks,
            };
            let digest = digest(&snapshot);
            if let Some(last) = self.load_state()
                && last.list_digest.as_deref() == Some(digest.as_str())
                && self.clock.now().unix_nanos() - last.sent_at.unix_nanos()
                    < RESEND_AFTER.as_nanos() as i128
            {
                return Ok(StaffSyncOutcome::Unchanged);
            }

            let sent = self.cloud.send(&snapshot, ctx)?;
            match sent.status {
                StaffCloudStatus::Ok => {
                    self.save_state(&StaffSyncState {
                        list_digest: Some(digest),
                        sent_at: self.clock.now(),
                    })?;
                    self.log
                        .info("Staff sync: {Count} POS clerks sent", &[int(count)]);
                    return Ok(StaffSyncOutcome::Sent);
                }
                StaffCloudStatus::KeyStale if attempt == 1 => {
                    self.log.info(
                        "Staff sync: the cloud changed the PIN key; fetching it again",
                        &[],
                    );
                    self.keys.invalidate()?;
                }
                _ => {
                    self.log.warn(
                        "Staff sync: the cloud did not apply the clerk list ({Status}, HTTP {HttpStatus})",
                        &[sent.status.name().into(), Value::Int(i64::from(sent.http_status))],
                    );
                    return Ok(StaffSyncOutcome::Failed);
                }
            }
        }
        Ok(StaffSyncOutcome::Failed)
    }

    /// Keys the cloud would refuse (over-long) are dropped, and so is every clerk of a key
    /// that repeats case-insensitively (fail closed). Sorted by key (ordinal) so the digest
    /// only changes when the list does; at most [`staff_sync_limits::MAX_CLERKS`].
    pub fn clean(&self, clerks: Vec<StaffClerk>) -> Vec<StaffClerk> {
        let max = staff_sync_limits::MAX_KEY_LENGTH;
        let too_long = clerks.iter().filter(|c| utf16_len(&c.key) > max).count();
        // GroupBy(OrdinalIgnoreCase), groups in first-seen order.
        let mut groups: Vec<Vec<StaffClerk>> = Vec::new();
        for c in clerks.into_iter().filter(|c| utf16_len(&c.key) <= max) {
            match groups
                .iter_mut()
                .find(|g| eq_ignore_case(&g[0].key, &c.key))
            {
                Some(g) => g.push(c),
                None => groups.push(vec![c]),
            }
        }
        let repeated: usize = groups.iter().filter(|g| g.len() > 1).map(Vec::len).sum();
        if too_long + repeated > 0 {
            self.log.warn(
                "Staff sync: {TooLong} clerk(s) with a code over {Max} characters and {Repeated} with a repeated code were left out",
                &[int(too_long), int(max), int(repeated)],
            );
        }
        let mut kept: Vec<StaffClerk> = groups
            .into_iter()
            .filter(|g| g.len() == 1)
            .filter_map(|g| g.into_iter().next())
            .collect();
        kept.sort_by(|a, b| ordinal_cmp(&a.key, &b.key));
        kept.truncate(staff_sync_limits::MAX_CLERKS);
        kept
    }

    fn load_state(&self) -> Option<StaffSyncState> {
        if !self.state_file.is_file() {
            return None;
        }
        let read = std::fs::read(&self.state_file)
            .map_err(|e| ("System.IO.IOException", e.to_string()))
            .and_then(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                StaffSyncState::from_json(text).map_err(|m| ("System.Text.Json.JsonException", m))
            });
        match read {
            Ok(s) => s,
            Err((type_name, message)) => {
                // A lost digest costs one resend; the cloud applies the same list again.
                self.log.warn_ex(
                    Exception::new(type_name, message),
                    "Staff sync state {File} could not be read; the clerk list will be sent again",
                    &[self.state_file.display().to_string().into()],
                );
                None
            }
        }
    }

    fn save_state(&self, state: &StaffSyncState) -> Result<(), StaffError> {
        let mut tmp = self.state_file.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, state.to_json())?;
        std::fs::rename(&tmp, &self.state_file)?;
        Ok(())
    }
}

/// A C# `int` log property.
fn int(n: usize) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Over the clerk list and the key id, not the agent version: an update alone does not
/// resend (`Digest`).
pub fn digest(snapshot: &StaffSnapshot) -> String {
    let without_version = StaffSnapshot {
        agent_version: String::new(),
        ..snapshot.clone()
    };
    dl_encoding::hex::encode(Sha256::digest(without_version.to_json().as_bytes()))
}
