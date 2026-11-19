//! `RequestProcessor`: turns a cloud request into a response. Re-validates it against the
//! catalog, replays journaled writes, and answers reads with `NotModified` or a row-level
//! `Delta` whenever the cloud's copy allows it. Key and vault operations never touch the PMS.
//!
//! PMS concurrency is bounded by the runtime's PMS lane (`MaxConcurrency` threads), the
//! counterpart of the .NET semaphore; [`Processor::route`] sends everything that does not need
//! the PMS to the fast lane, so a busy PMS never delays a rejection or a replay.

use std::sync::Arc;
use std::time::Instant;

use dl_json::Value;

use crate::agent::executor::{ExecError, Executor};
use crate::agent::health::Health;
use crate::agent::journal::Journal;
use crate::agent::secrets::TenantKeyStore;
use crate::agent::snapshots::{Snapshot, SnapshotStore};
use crate::agent::vault::LegalVault;
use crate::contracts::errors::{self, LOCK_TIMEOUT};
use crate::contracts::values::{parse_base64, to_base64};
use crate::contracts::{
    OperationKind, PmsRequest, PmsResponse, PmsResultSet, PmsRow, Status, catalog, delta, hasher,
    key_vault, projection, validator, vault_ops,
};

/// Where a request runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Answered without the PMS (key and vault operations, rejections, journal replays).
    Fast,
    /// Needs a PMS call.
    Pms,
}

/// What the processor works with.
pub struct Processor {
    executor: Arc<dyn Executor>,
    snapshots: SnapshotStore,
    journal: Arc<Journal>,
    keys: Arc<TenantKeyStore>,
    vault: LegalVault,
    health: Arc<Health>,
    lock_timeout_seconds: i32,
}

impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Processor").finish_non_exhaustive()
    }
}

impl Processor {
    /// A processor over its collaborators.
    pub fn new(
        executor: Arc<dyn Executor>,
        snapshot_capacity: usize,
        journal: Arc<Journal>,
        keys: Arc<TenantKeyStore>,
        vault: LegalVault,
        health: Arc<Health>,
        lock_timeout_seconds: i32,
    ) -> Processor {
        Processor {
            executor,
            snapshots: SnapshotStore::new(snapshot_capacity),
            journal,
            keys,
            vault,
            health,
            lock_timeout_seconds,
        }
    }

    /// The lane `request` belongs in.
    pub fn route(&self, request: &PmsRequest) -> Lane {
        if key_vault::is_key_op(&request.operation) || vault_ops::is_vault_op(&request.operation) {
            return Lane::Fast;
        }
        let Ok(cat) = catalog() else {
            return Lane::Fast;
        };
        match validator::validate(cat, request) {
            Err(_) => Lane::Fast,
            Ok(op) if op.kind == OperationKind::Write && self.replay(request).is_some() => {
                Lane::Fast
            }
            Ok(_) => Lane::Pms,
        }
    }

    fn replay(&self, request: &PmsRequest) -> Option<PmsResponse> {
        let key = request.idempotency_key.as_deref()?;
        let mut replay = self.journal.get(key)?;
        replay.request_id.clone_from(&request.request_id);
        Some(replay)
    }

    /// `HandleAsync`, counted in the metrics. Never fails: every request gets an answer.
    pub fn handle(&self, request: &PmsRequest) -> PmsResponse {
        let response = self.answer(request);
        self.health.count(response.status);
        response
    }

    fn answer(&self, request: &PmsRequest) -> PmsResponse {
        if key_vault::is_key_op(&request.operation) {
            return self.key_op(request);
        }
        if vault_ops::is_vault_op(&request.operation) {
            return self.vault_op(request);
        }
        let started = Instant::now();
        let cat = match catalog() {
            Ok(c) => c,
            Err(e) => {
                dl_log::error!("the embedded catalog does not load"; error = %e);
                return PmsResponse::bare(
                    &request.request_id,
                    Status::Failed,
                    Some("Agent error".into()),
                );
            }
        };
        let op = match validator::validate(cat, request) {
            Ok(op) => op,
            Err(error) => {
                dl_log::warn!("Rejected PMS request"; operation = %request.operation, error = %error);
                self.health.op_refused(&request.operation, &error);
                return PmsResponse::bare(&request.request_id, Status::Rejected, Some(error));
            }
        };
        if op.kind == OperationKind::Write
            && let Some(replay) = self.replay(request)
        {
            return replay;
        }

        let call = Instant::now();
        let sets = match self
            .executor
            .execute(op, &request.parameters, request.projection.as_ref())
        {
            Ok(sets) => {
                self.health.pms_ok(call.elapsed());
                sets
            }
            Err(e) => return self.failed(request, &op.name, &e, call),
        };
        let (etag, full) = delta::full(sets);
        let elapsed = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);

        if op.kind == OperationKind::Write {
            let written = PmsResponse {
                request_id: request.request_id.clone(),
                status: Status::Ok,
                etag: Some(etag),
                result_sets: full,
                error: None,
                elapsed_ms: elapsed,
            };
            let key = request.idempotency_key.as_deref().unwrap_or("");
            if let Err(e) = self.journal.append(key, &written) {
                // The write happened: answering Failed would invite a second one.
                dl_log::error!("could not journal a PMS write; a retry would run it again"; operation = %op.name, error = %e);
            }
            return written;
        }

        let snap_key = format!(
            "{}|{}|{}",
            op.name,
            hasher::hash_parameters(request.parameters.iter()),
            projection::hash(request.projection.as_ref())
        );
        let previous = self.snapshots.get(&snap_key);
        let full = Arc::new(full);
        self.snapshots.put(
            &snap_key,
            Snapshot {
                etag: etag.clone(),
                sets: Arc::clone(&full),
            },
        );
        self.health.metrics.snapshots.store(
            self.snapshots.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        let respond = |status: Status, sets: Vec<PmsResultSet>| PmsResponse {
            request_id: request.request_id.clone(),
            status,
            etag: Some(etag.clone()),
            result_sets: sets,
            error: None,
            elapsed_ms: elapsed,
        };
        if let Some(base) = request.base_etag.as_deref() {
            if base == etag {
                return respond(Status::NotModified, Vec::new());
            }
            if let Some(prev) = previous.filter(|p| p.etag == base) {
                return respond(Status::Delta, delta::build(op, &prev.sets, &full));
            }
        }
        respond(Status::Ok, full.as_ref().clone())
    }

    fn failed(
        &self,
        request: &PmsRequest,
        operation: &str,
        e: &ExecError,
        call: Instant,
    ) -> PmsResponse {
        let error = match e {
            ExecError::Sql { number } if *number == LOCK_TIMEOUT => {
                self.health.pms_ok(call.elapsed());
                let holders = self.executor.describe_lock_holders();
                dl_log::error!(
                    "PMS call gave up waiting for a record another PMS session holds locked.";
                    operation = %operation,
                    lock_timeout_s = self.lock_timeout_seconds,
                    holders = %holders.as_deref().unwrap_or("unknown")
                );
                errors::describe(*number)
            }
            ExecError::Sql { number } => {
                self.health.pms_ok(call.elapsed());
                dl_log::error!("PMS call failed."; operation = %operation, number = *number);
                errors::describe(*number)
            }
            ExecError::Unavailable(m) => {
                dl_log::error!("PMS call failed: the PMS is unavailable."; operation = %operation, error = %m);
                self.health.pms_failed(m);
                errors::describe(-1)
            }
            ExecError::Other(m) => {
                // The text stays in the local log: it can carry PMS data.
                dl_log::error!("PMS request failed unexpectedly."; operation = %operation, error = %m);
                "Agent error".to_string()
            }
        };
        PmsResponse::bare(&request.request_id, Status::Failed, Some(error))
    }

    /// Wrap/unwrap a data key with the tenant master key; only the escrow export returns the
    /// key itself.
    fn key_op(&self, request: &PmsRequest) -> PmsResponse {
        let output = (|| -> Result<Vec<u8>, String> {
            let vault = self.keys.vault().map_err(|e| e.to_string())?;
            if request.operation == key_vault::OP_EXPORT {
                return Ok(vault.export().to_vec());
            }
            let param = |name: &str| -> Result<String, String> {
                match request.parameters.get(name) {
                    None => Err("KeyNotFound".into()),
                    Some(v) => Ok(v.unwrap_or("").to_string()),
                }
            };
            let input = parse_base64(&param(key_vault::KEY_PARAM)?).ok_or("Format")?;
            let context = param(key_vault::CONTEXT_PARAM)?;
            let out = if request.operation == key_vault::OP_WRAP {
                vault.wrap(&input, &context)
            } else {
                vault.unwrap(&input, &context)
            };
            out.map_err(|_| "Cryptographic".to_string())
        })();
        match output {
            Ok(bytes) => single_value(request, key_vault::RESULT_COLUMN, Some(to_base64(&bytes))),
            Err(reason) => {
                dl_log::warn!("Key operation rejected"; operation = %request.operation, reason = %reason);
                self.health
                    .op_refused(&request.operation, "Key operation rejected.");
                PmsResponse::bare(
                    &request.request_id,
                    Status::Rejected,
                    Some("Key operation rejected.".into()),
                )
            }
        }
    }

    /// Store / return / remove a legal document in the hotel's vault.
    fn vault_op(&self, request: &PmsRequest) -> PmsResponse {
        let value = (|| -> Result<Option<String>, String> {
            let path =
                vault_ops::try_normalize(request.parameters.get(vault_ops::PATH_PARAM).flatten())
                    .ok_or("Bad path.")?;
            let param = |name: &str| -> Result<String, String> {
                match request.parameters.get(name) {
                    None => Err("KeyNotFound".into()),
                    Some(v) => Ok(v.unwrap_or("").to_string()),
                }
            };
            match request.operation.as_str() {
                vault_ops::OP_PUT => {
                    let data = parse_base64(&param(vault_ops::DATA_PARAM)?).ok_or("Format")?;
                    if data.len() > vault_ops::MAX_BYTES {
                        return Err("File too large.".into());
                    }
                    let sha = param(vault_ops::SHA256_PARAM)?;
                    self.vault
                        .put(&path, &data, &sha)
                        .map(Some)
                        .map_err(|e| e.to_string())
                }
                vault_ops::OP_GET => self
                    .vault
                    .get(&path)
                    .map(|b| b.map(|bytes| to_base64(&bytes)))
                    .map_err(|e| e.to_string()),
                _ => self
                    .vault
                    .delete(&path)
                    .map(|()| Some("deleted".into()))
                    .map_err(|e| e.to_string()),
            }
        })();
        match value {
            Ok(v) => single_value(request, vault_ops::RESULT_COLUMN, v),
            Err(reason) => {
                dl_log::warn!("Vault operation rejected"; operation = %request.operation, reason = %reason);
                self.health
                    .op_refused(&request.operation, "Vault operation rejected.");
                PmsResponse::bare(
                    &request.request_id,
                    Status::Rejected,
                    Some("Vault operation rejected.".into()),
                )
            }
        }
    }
}

/// `Ok` with one set (index 0, hash `""`) holding one row `{column: value}`, or no row.
fn single_value(request: &PmsRequest, column: &str, value: Option<String>) -> PmsResponse {
    let rows = value
        .map(|v| {
            let mut row = PmsRow::new();
            row.insert(column.to_string(), Value::String(v));
            vec![row]
        })
        .unwrap_or_default();
    PmsResponse {
        request_id: request.request_id.clone(),
        status: Status::Ok,
        etag: None,
        result_sets: vec![PmsResultSet {
            index: 0,
            hash: String::new(),
            rows,
            removed_keys: None,
            key_order: None,
        }],
        error: None,
        elapsed_ms: 0,
    }
}
