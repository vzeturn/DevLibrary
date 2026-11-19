//! Shared kit of the `sync.verify` tests (port of the fakes in
//! `Commands/VerifyCommandHandlerTests.cs`): a fake cloud that keeps `sync.row_state` per
//! table and answers the manifest endpoints from it, the catalog subset, and command
//! builders.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use dl_agent_pda_sync::capture::codecs::key_codec;
use dl_agent_pda_sync::capture::shadow_manifest::{self, ROWS_PER_PASS};
use dl_agent_pda_sync::capture::{
    CaptureState, InMemoryCaptureState, RuntimeConfigSource, ShadowEntry, ShadowWrite, StateCommit,
    StateFactory,
};
use dl_agent_pda_sync::commands::{
    CloudManifest, CloudManifestKeys, CloudManifestSource, VerifyCommandHandler, VerifyParts,
};
use dl_agent_pda_sync::config::SharedClock;
use dl_agent_pda_sync::contracts::WireJson;
use dl_agent_pda_sync::contracts::catalog::SyncCatalog;
use dl_agent_pda_sync::contracts::hashing::row_key;
use dl_agent_pda_sync::contracts::map::StrMap;
use dl_agent_pda_sync::contracts::wire::{
    CommandStatus, SyncBatch, SyncCommand, SyncCommandResult, SyncVerifyCommandResult,
    SyncVerifyPayload, site_command_types,
};
use dl_agent_pda_sync::outbox::SqliteOutbox;
use dl_agent_pda_sync::reporting::Logger;
use dl_agent_pda_sync::transport::Error;
use dl_agent_pda_sync::util::ctx::Ctx;

pub const LEDGER: &str = "Invoice"; // watermark+rescan, mirrored
pub const MASTER: &str = "CashierDef"; // scan-diff, mirrored
pub const OPEN_TABLE: &str = "TmpInvoice"; // snapshot-diff (open checks), mirrored
pub const UNMIRRORED: &str = "ARINV"; // classified, no cloud table
pub const SKIPPED: &str = "APLateCharge"; // class skip

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The catalog limited to `names`, in that order.
pub fn subset(names: &[&str]) -> SyncCatalog {
    let all = SyncCatalog::load().unwrap();
    let mut c = all.clone();
    c.tables = names
        .iter()
        .map(|n| {
            all.find(n)
                .unwrap_or_else(|| panic!("{n} is not in the catalog"))
                .clone()
        })
        .collect();
    c
}

/// The catalog of the handler tests.
pub fn test_catalog() -> Arc<SyncCatalog> {
    Arc::new(subset(&[LEDGER, MASTER, OPEN_TABLE, UNMIRRORED, SKIPPED]))
}

fn map(pairs: &[(&str, Option<&str>)]) -> StrMap {
    let mut m = StrMap::new();
    for (k, v) in pairs {
        m.insert(*k, v.map(str::to_owned));
    }
    m
}

/// `KeyCodec.Encode({ No = value })`.
pub fn key(value: &str) -> String {
    key_codec::encode(&map(&[("No", Some(value))]))
}

/// `RowKey.Of({ No = value })`.
pub fn row_key_of(value: &str) -> String {
    row_key::of(&map(&[("No", Some(value))]))
}

/// The 256-bucket number of `{ No = value }`.
pub fn bucket_of(value: &str) -> i32 {
    row_key::bucket(&row_key_of(value), 256) as i32
}

/// Shadow rows `(key(v), "h-" + v)`.
pub fn rows(values: &[&str]) -> Vec<(String, String)> {
    values.iter().map(|v| (key(v), format!("h-{v}"))).collect()
}

/// Commits `rows` into the shadow of `table`.
pub fn seed(state: &dyn CaptureState, table: &str, rows: &[(String, String)]) {
    let mut commit = StateCommit::default();
    commit.shadow.push(ShadowWrite {
        table: table.to_owned(),
        upserts: rows
            .iter()
            .map(|(k, h)| ShadowEntry::new(k, h, "-", None))
            .collect(),
        delete_keys: Vec::new(),
    });
    state.commit(&commit, &Ctx::background()).unwrap();
}

/// Called with the table of every manifest request.
pub type OnCall = Box<dyn Fn(&str) + Send + Sync>;

/// The cloud side: `sync.row_state` per vendor table (shadow-form key to row hash), with a
/// manifest built from it exactly as the agent builds its own (the digest's equality with
/// PostgreSQL is the golden vector of `verify_handler`).
#[derive(Default)]
pub struct FakeCloud {
    tables: Mutex<BTreeMap<String, BTreeMap<String, String>>>,
    pub errors: Mutex<BTreeMap<String, String>>,
    pub calls: Mutex<Vec<String>>,
    pub on_call: Mutex<Option<OnCall>>,
    pub keys_error: Mutex<Option<String>>,
    pub keys_truncated: AtomicBool,
    pub key_calls: Mutex<Vec<Vec<i32>>>,
}

impl FakeCloud {
    pub fn new() -> Arc<FakeCloud> {
        Arc::new(FakeCloud::default())
    }

    /// Replaces the rows of `table` (shadow-form keys).
    pub fn set_rows(&self, table: &str, rows: &[(String, String)]) {
        lock(&self.tables).insert(table.to_owned(), rows.iter().cloned().collect());
    }

    /// Applies one batch the way the cloud applier does: upserts by key, tombstones delete.
    pub fn apply(&self, b: &SyncBatch) {
        let mut t = lock(&self.tables);
        let rows = t.entry(b.table.clone()).or_default();
        for r in &b.rows {
            rows.insert(key_codec::encode(&r.key), r.hash.clone());
        }
        for d in &b.tombstones {
            rows.remove(&key_codec::encode(&d.key));
        }
    }

    /// Uploads everything queued in `outbox`, in queue order, and acknowledges it.
    pub fn pump(&self, outbox: &SqliteOutbox) -> usize {
        let items = outbox.peek::<&str>(100_000, &[]).unwrap();
        for i in &items {
            let b = SyncBatch::from_json(std::str::from_utf8(&i.payload).unwrap()).unwrap();
            self.apply(&b);
            outbox.ack(i.id).unwrap();
        }
        items.len()
    }

    /// The cloud row keys of `table`, sorted.
    pub fn row_keys(&self, table: &str) -> Vec<String> {
        let t = lock(&self.tables);
        let mut keys: Vec<String> = t
            .get(table)
            .map(|r| {
                r.keys()
                    .map(|k| row_key::of(&key_codec::decode(k).unwrap()))
                    .collect()
            })
            .unwrap_or_default();
        keys.sort();
        keys
    }

    fn state_of(&self, table: &str) -> InMemoryCaptureState {
        let state = InMemoryCaptureState::new();
        let rows: Vec<(String, String)> = lock(&self.tables)
            .get(table)
            .map(|r| r.iter().map(|(k, h)| (k.clone(), h.clone())).collect())
            .unwrap_or_default();
        seed(&state, table, &rows);
        state
    }
}

impl CloudManifestSource for FakeCloud {
    fn get(&self, table: &str, buckets: i32, ctx: &Ctx) -> Result<CloudManifest, Error> {
        lock(&self.calls).push(table.to_owned());
        if let Some(f) = &*lock(&self.on_call) {
            f(table);
        }
        if let Some(e) = lock(&self.errors).get(table) {
            return Ok(CloudManifest::failed(e));
        }
        let state = self.state_of(table);
        let m = shadow_manifest::build(&state, table, buckets, ROWS_PER_PASS, ctx).unwrap();
        Ok(CloudManifest::of(m.buckets))
    }

    fn get_keys(
        &self,
        table: &str,
        buckets: i32,
        bucket_list: &[i32],
        ctx: &Ctx,
    ) -> Result<CloudManifestKeys, Error> {
        lock(&self.key_calls).push(bucket_list.to_vec());
        if let Some(e) = &*lock(&self.keys_error) {
            return Ok(CloudManifestKeys::failed(e));
        }
        let state = self.state_of(table);
        let wanted: HashSet<i32> = bucket_list.iter().copied().collect();
        let rows = shadow_manifest::bucket_rows(&state, table, buckets, &wanted, ctx)
            .unwrap()
            .unwrap();
        Ok(CloudManifestKeys {
            rows,
            truncated: self.keys_truncated.load(Ordering::SeqCst),
            error: None,
        })
    }
}

/// A handler over the given parts.
pub fn handler(
    outbox: &Arc<SqliteOutbox>,
    state: StateFactory,
    cloud: Arc<dyn CloudManifestSource>,
    clock: SharedClock,
    catalog: Arc<SyncCatalog>,
    config: Option<Arc<dyn RuntimeConfigSource>>,
    log: &Logger,
) -> VerifyCommandHandler {
    VerifyCommandHandler::new(
        VerifyParts {
            outbox: outbox.clone(),
            open_state: state,
            cloud,
            clock,
            catalog,
            config,
        },
        log,
    )
}

/// A `sync.verify` command.
pub fn verify_command(payload: &SyncVerifyPayload) -> SyncCommand {
    SyncCommand {
        command_id: dl_uuid::Uuid::new_v4(),
        type_: site_command_types::SYNC_VERIFY.into(),
        aggregate_key: SyncVerifyPayload::AGGREGATE_KEY.into(),
        expected_hash: None,
        payload: payload.to_element(),
        created_by: "system:sync-verify".into(),
        expires_at_utc: None,
        contract_version: 1,
        aggregate_seq: None,
        idempotency_key: None,
        tenant_id: None,
    }
}

/// `sync.verify` of `tables` with 256 buckets.
pub fn verify(tables: &[&str]) -> SyncCommand {
    verify_command(&SyncVerifyPayload {
        tables: tables.iter().map(|t| (*t).to_owned()).collect(),
        buckets: 256,
        reason: "nightly check".into(),
    })
}

/// The result of an applied verify.
pub fn result_of(r: &SyncCommandResult) -> SyncVerifyCommandResult {
    assert_eq!(r.status, CommandStatus::Applied, "{:?}", r.reason);
    SyncVerifyCommandResult::from_element(r.result.as_ref().unwrap()).unwrap()
}

/// Runs a verify of `tables` and reads the result.
pub fn run(h: &VerifyCommandHandler, tables: &[&str]) -> SyncVerifyCommandResult {
    result_of(&h.execute(&verify(tables), &Ctx::background()).unwrap())
}
