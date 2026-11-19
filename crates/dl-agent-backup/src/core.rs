//! The state the sync loop, the executor workers and the module share: the hub client, the
//! durable stores, the master key, the tenant backup keys, the problem reports and the
//! counters.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dl_json::Value;
use dl_tds::CancelToken;

use crate::Error;
use crate::crypto::decrypt_credential;
use crate::crypto::envelope::{BackupKeys, DataKey};
use crate::hub::HubClient;
use crate::hub::types::ConfigResponse;
use crate::log;
use crate::report::{Reports, Sink, Stats};
use crate::sql::Connector;
use crate::state::history::History;
use crate::state::outbox::Outbox;
use crate::state::queue::Queue;
use crate::state::runs::Runs;
use crate::state::state_dir;
use crate::upload::Uploaders;

/// How the module runs; see [`crate::module::settings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Server-side directory for backup files; empty uses the instance's backup directory.
    pub backup_dir: String,
    /// Backups running at once (Go `DefaultConcurrency`), at least 1.
    pub concurrency: usize,
    /// How long a running BACKUP may go on after stop before it is cancelled.
    pub stop_grace: Duration,
    /// How often the hub's config is polled (Go 60 s).
    pub poll: Duration,
    /// How often an idle worker looks at the queue (Go 5 s).
    pub worker_poll: Duration,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            backup_dir: String::new(),
            concurrency: 1,
            stop_grace: Duration::from_secs(10),
            poll: Duration::from_secs(60),
            worker_poll: Duration::from_secs(5),
        }
    }
}

/// How the hub link is doing, for the module's run state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    /// A config poll has succeeded since the module started.
    pub polled: bool,
    /// Why the module is degraded, if it is.
    pub problem: Option<String>,
}

/// Locks `m`, carrying on with the data when another thread panicked holding it (every
/// store writes its file before it returns, so the in-memory copy is never half-done).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The shared state.
pub struct Core {
    /// The hub.
    pub hub: HubClient,
    /// SQL Server connections.
    pub sql: Arc<dyn Connector>,
    /// Destinations.
    pub uploaders: Arc<dyn Uploaders>,
    /// Queued backups.
    pub queue: Mutex<Queue>,
    /// Reports the hub has not confirmed.
    pub outbox: Mutex<Outbox>,
    /// Backups kept for restore and retention, and recent outcomes.
    pub history: Mutex<History>,
    /// When each schedule last ran.
    pub runs: Mutex<Runs>,
    /// Problem reports.
    pub reports: Reports,
    /// Counters.
    pub stats: Mutex<Stats>,
    /// Settings.
    pub settings: Settings,
    /// The last config the hub sent.
    pub config: Mutex<Option<ConfigResponse>>,
    /// Schedules `backup.run-now` asked for, run on the next tick.
    pub run_now: Mutex<Vec<String>>,
    /// The hub link's health.
    pub health: Mutex<Health>,
    /// Cancels running SQL statements, uploads and downloads once the stop grace is over.
    pub cancel: CancelToken,
    /// Counts enqueued jobs, so an idle worker notices new work before its next poll.
    pub queued: AtomicU64,
    key: Mutex<Option<String>>,
    backup_keys: Mutex<KeyCache>,
}

/// How long the tenant's current backup key (or the hub's "none") is trusted before it is
/// asked for again, so a key the hub rotates is picked up without a restart.
const CURRENT_KEY_TTL: Duration = Duration::from_secs(15 * 60);

/// Backup data keys the hub gave this agent: the current one, and every key by id (a key's
/// bytes never change under its id). Memory only, like the master key.
#[derive(Default)]
struct KeyCache {
    current: Option<(Instant, Option<DataKey>)>,
    by_id: HashMap<String, DataKey>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("hub", &self.hub)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl BackupKeys for Core {
    fn current(&self) -> Result<Option<DataKey>, Error> {
        if let Some((at, k)) = &lock(&self.backup_keys).current
            && at.elapsed() < CURRENT_KEY_TTL
        {
            return Ok(k.clone());
        }
        let fresh = match self.hub.backup_key() {
            Ok(k) => k,
            // The hub is unreachable: the last answer it gave (even past its TTL) keeps
            // backups running, sealed as before, through an outage.
            Err(e @ Error::Hub(_)) => match &lock(&self.backup_keys).current {
                Some((_, k)) => return Ok(k.clone()),
                None => return Err(e),
            },
            Err(e) => return Err(e),
        };
        let mut cache = lock(&self.backup_keys);
        if let Some(k) = &fresh {
            cache.by_id.insert(k.id.clone(), k.clone());
        }
        cache.current = Some((Instant::now(), fresh.clone()));
        Ok(fresh)
    }

    fn by_id(&self, id: &str) -> Result<DataKey, Error> {
        if let Some(k) = lock(&self.backup_keys).by_id.get(id) {
            return Ok(k.clone());
        }
        let k = self.hub.backup_key_by_id(id)?;
        lock(&self.backup_keys)
            .by_id
            .insert(k.id.clone(), k.clone());
        Ok(k)
    }
}

impl Core {
    /// Asks the hub for the tenant's current backup key when the cached answer is stale, so
    /// a backup that runs while the hub is down still has it. A failure is logged; the
    /// backup itself fails later only if it still has no answer.
    pub fn prefetch_backup_key(&self) {
        if let Err(e) = BackupKeys::current(self) {
            log::warn(
                "could not fetch the tenant backup key",
                &[("error", &e.to_string())],
            );
        }
    }

    /// Opens the stores under `<module_data_dir>/backup/`.
    pub fn open(
        module_data_dir: &Path,
        hub: HubClient,
        sql: Arc<dyn Connector>,
        uploaders: Arc<dyn Uploaders>,
        sink: Arc<dyn Sink>,
        settings: Settings,
    ) -> Result<Core, Error> {
        let dir = state_dir(module_data_dir);
        Ok(Core {
            hub,
            sql,
            uploaders,
            queue: Mutex::new(Queue::open(dir.join("queue.enc"))?),
            outbox: Mutex::new(Outbox::open(dir.join("outbox.enc"))?),
            history: Mutex::new(History::open(dir.join("history.enc"))?),
            runs: Mutex::new(Runs::open(dir.join("runs.enc"))?),
            reports: Reports::new(sink),
            stats: Mutex::new(Stats::default()),
            settings,
            config: Mutex::new(None),
            run_now: Mutex::new(Vec::new()),
            health: Mutex::new(Health::default()),
            cancel: CancelToken::new(),
            queued: AtomicU64::new(0),
            key: Mutex::new(None),
            backup_keys: Mutex::new(KeyCache::default()),
        })
    }

    /// Sends a report to the hub through the outbox: it is written to disk first and
    /// removed once the hub confirms, so a hub outage loses nothing.
    pub fn deliver(&self, key: &str, body: Value) -> Result<(), Error> {
        lock(&self.outbox).add(key, body.clone())?;
        self.hub.report(&body)?;
        lock(&self.outbox).ack_sent(key, &body)
    }

    /// Re-sends what the outbox holds, oldest first, stopping at the first hub failure.
    /// Returns how many the hub confirmed.
    pub fn drain_outbox(&self) -> Result<usize, Error> {
        let pending = lock(&self.outbox).pending();
        let mut sent = 0;
        for entry in pending {
            self.hub.report(&entry.body)?;
            lock(&self.outbox).ack_sent(&entry.key, &entry.body)?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Whether a report under `key` is still waiting in the outbox.
    pub fn in_outbox(&self, key: &str) -> bool {
        lock(&self.outbox).pending().iter().any(|e| e.key == key)
    }

    /// Opens a credential from the config. `flagged` is the hub's `credentialError`: the hub
    /// could not open its own copy, so there is nothing to decrypt. On a decrypt failure
    /// the master key is fetched again once (it may have rotated), as Go `decryptCredential`.
    pub fn open_credential(
        &self,
        stored: &str,
        flagged: bool,
        what: &str,
    ) -> Result<String, Error> {
        if flagged {
            return Err(Error::Credential(format!(
                "the hub could not open the stored {what}; enter it again in the hub"
            )));
        }
        if stored.is_empty() {
            return Ok(String::new());
        }
        let key = self.master_key(false)?;
        let first = match decrypt_credential(stored, &key) {
            Ok(plain) => return Ok(plain),
            Err(e) => e,
        };
        let fresh = self.master_key(true).map_err(|e| match e {
            Error::Auth(m) => Error::Auth(m),
            other => Error::Credential(format!(
                "{what}: {first} (master key re-fetch also failed: {other})"
            )),
        })?;
        decrypt_credential(stored, &fresh).map_err(|e| Error::Credential(format!("{what}: {e}")))
    }

    fn master_key(&self, refresh: bool) -> Result<String, Error> {
        let mut key = lock(&self.key);
        match key.as_ref() {
            Some(k) if !refresh => Ok(k.clone()),
            _ => {
                let fresh = self.hub.master_key()?;
                *key = Some(fresh.clone());
                Ok(fresh)
            }
        }
    }

    /// Publishes the counters as metrics.
    pub fn publish(&self) {
        let depth = lock(&self.queue).depth();
        let outbox = lock(&self.outbox).len();
        lock(&self.stats).publish(self.reports.sink(), depth, outbox);
    }

    /// The `backup.status` answer.
    pub fn status(&self) -> Value {
        let depth = lock(&self.queue).depth();
        let outbox = lock(&self.outbox).len();
        let mut m = lock(&self.stats).to_map(depth, outbox);
        let recent: Vec<Value> = lock(&self.history)
            .recent(10)
            .into_iter()
            .map(|o| {
                dl_json::json!({
                    "historyId": o.history_id.as_str(),
                    "scheduleId": o.schedule_id.as_str(),
                    "database": o.database.as_str(),
                    "ok": o.ok,
                    "error": crate::report::scrub(&o.error),
                    "bytes": o.bytes,
                    "finishedAt": o.finished_at,
                })
            })
            .collect();
        m.insert("recent", Value::Array(recent));
        Value::Object(m)
    }
}
