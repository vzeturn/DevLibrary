//! Durable agent-side memory of what capture sent (`State/ICaptureState.cs`,
//! `State/InMemoryCaptureState.cs`).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use super::error::Result;
use super::util::DotNetDict;
use crate::contracts::hashing::ordinal_cmp;
use crate::contracts::map::OrderedMap;
use crate::util::ctx::Ctx;

/// One remembered row: stable key, canonical hash and the bucket (day / check number /
/// ...) it was last seen in. `ref_` optionally holds the typed vendor key so the row can
/// be looked up directly later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowEntry {
    /// Shadow key (encoded wire key, optional `#n` suffix).
    pub key: String,
    /// Canonical row hash (or blob SHA-256).
    pub hash: String,
    /// Bucket.
    pub bucket: String,
    /// Typed vendor key (keyset encoding).
    pub ref_: Option<String>,
}

impl ShadowEntry {
    /// An entry.
    pub fn new(key: &str, hash: &str, bucket: &str, ref_: Option<&str>) -> ShadowEntry {
        ShadowEntry {
            key: key.to_owned(),
            hash: hash.to_owned(),
            bucket: bucket.to_owned(),
            ref_: ref_.map(str::to_owned),
        }
    }
}

/// Shadow changes of one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowWrite {
    /// Shadow table (vendor table, `X$blob`, `$open`).
    pub table: String,
    /// Upserts.
    pub upserts: Vec<ShadowEntry>,
    /// Keys to delete (applied before the upserts).
    pub delete_keys: Vec<String>,
}

/// Everything that must change atomically after a batch was handed to the sink: the
/// stream's next sequence number, the shadow hashes of the rows in it and the cursors.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StateCommit {
    /// Stream whose sequence number was consumed.
    pub stream: Option<String>,
    /// Sequence number just consumed; the stream's next number becomes `seq + 1`.
    pub seq: Option<i64>,
    /// Shadow writes, in order.
    pub shadow: Vec<ShadowWrite>,
    /// Cursor name to value; `None` deletes the cursor (insertion order kept).
    pub cursors: OrderedMap<Option<String>>,
}

/// Key to hash of a shadow, in the order the store returned them, with O(1) lookups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shadow {
    entries: Vec<(String, String)>,
    index: HashMap<String, usize>,
}

impl Shadow {
    /// An empty shadow.
    pub fn new() -> Shadow {
        Shadow::default()
    }

    /// Sets `key` (a later value replaces an earlier one in place).
    pub fn insert(&mut self, key: String, hash: String) {
        if let Some(&i) = self.index.get(&key) {
            self.entries[i].1 = hash;
        } else {
            self.index.insert(key.clone(), self.entries.len());
            self.entries.push((key, hash));
        }
    }

    /// The hash of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.index.get(key).map(|&i| self.entries[i].1.as_str())
    }

    /// Whether `key` is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// Keys in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// Pairs in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, h)| (k.as_str(), h.as_str()))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl FromIterator<(String, String)> for Shadow {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Shadow {
        let mut s = Shadow::new();
        for (k, h) in iter {
            s.insert(k, h);
        }
        s
    }
}

/// Durable agent-side memory. Commits happen only after the sink accepted a batch
/// (at-least-once): after a crash the same changes are detected again and re-sent,
/// which the idempotent cloud applier tolerates.
pub trait CaptureState: Send + Sync {
    /// Next sequence number of a stream (starts at 1).
    fn peek_next_seq(&self, stream: &str, ctx: &Ctx) -> Result<i64>;

    /// A cursor value.
    fn get_cursor(&self, name: &str, ctx: &Ctx) -> Result<Option<String>>;

    /// Key to hash of a table's shadow; restricted to one bucket when given.
    fn get_shadow(&self, table: &str, bucket: Option<&str>, ctx: &Ctx) -> Result<Shadow>;

    /// Full shadow entries (key, hash, bucket, ref) of one bucket.
    fn get_shadow_entries(&self, table: &str, bucket: &str, ctx: &Ctx) -> Result<Vec<ShadowEntry>>;

    /// Shadow hashes for just these keys (missing keys are absent from the result).
    fn get_shadow_by_keys(&self, table: &str, keys: &[String], ctx: &Ctx) -> Result<Shadow>;

    /// Distinct buckets that currently have shadow entries (ordinal order).
    fn get_buckets(&self, table: &str, ctx: &Ctx) -> Result<Vec<String>>;

    /// Applies a commit atomically.
    fn commit(&self, commit: &StateCommit, ctx: &Ctx) -> Result<()>;

    /// Forgets a table's shadow (forces a full re-diff, e.g. after a resnapshot).
    fn reset_table(&self, table: &str, ctx: &Ctx) -> Result<()>;
}

#[derive(Default)]
struct Memory {
    seq: DotNetDict<i64>,
    cursors: DotNetDict<String>,
    shadow: DotNetDict<DotNetDict<ShadowEntry>>,
}

/// Volatile [`CaptureState`] for tests and dry runs.
#[derive(Default)]
pub struct InMemoryCaptureState {
    gate: Mutex<Memory>,
}

impl std::fmt::Debug for InMemoryCaptureState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryCaptureState")
            .finish_non_exhaustive()
    }
}

impl InMemoryCaptureState {
    /// An empty state.
    pub fn new() -> InMemoryCaptureState {
        InMemoryCaptureState::default()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Memory) -> T) -> T {
        let mut m = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut m)
    }
}

impl CaptureState for InMemoryCaptureState {
    fn peek_next_seq(&self, stream: &str, _ctx: &Ctx) -> Result<i64> {
        Ok(self.with(|m| m.seq.get(stream).copied().unwrap_or(1)))
    }

    fn get_cursor(&self, name: &str, _ctx: &Ctx) -> Result<Option<String>> {
        Ok(self.with(|m| m.cursors.get(name).cloned()))
    }

    fn get_shadow(&self, table: &str, bucket: Option<&str>, _ctx: &Ctx) -> Result<Shadow> {
        Ok(self.with(|m| {
            m.shadow
                .get(table)
                .map(|t| {
                    t.iter()
                        .map(|(_, e)| e)
                        .filter(|e| bucket.is_none_or(|b| e.bucket == b))
                        .map(|e| (e.key.clone(), e.hash.clone()))
                        .collect()
                })
                .unwrap_or_default()
        }))
    }

    fn get_shadow_entries(
        &self,
        table: &str,
        bucket: &str,
        _ctx: &Ctx,
    ) -> Result<Vec<ShadowEntry>> {
        Ok(self.with(|m| {
            m.shadow
                .get(table)
                .map(|t| {
                    t.iter()
                        .map(|(_, e)| e)
                        .filter(|e| e.bucket == bucket)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        }))
    }

    fn get_shadow_by_keys(&self, table: &str, keys: &[String], _ctx: &Ctx) -> Result<Shadow> {
        Ok(self.with(|m| {
            let Some(t) = m.shadow.get(table) else {
                return Shadow::new();
            };
            keys.iter()
                .filter_map(|k| t.get(k).map(|e| (k.clone(), e.hash.clone())))
                .collect()
        }))
    }

    fn get_buckets(&self, table: &str, _ctx: &Ctx) -> Result<Vec<String>> {
        Ok(self.with(|m| {
            let mut out: Vec<String> = Vec::new();
            if let Some(t) = m.shadow.get(table) {
                for (_, e) in t.iter() {
                    if !out.contains(&e.bucket) {
                        out.push(e.bucket.clone());
                    }
                }
            }
            out.sort_by(|a, b| ordinal_cmp(a, b));
            out
        }))
    }

    fn commit(&self, commit: &StateCommit, _ctx: &Ctx) -> Result<()> {
        self.with(|m| {
            if let (Some(stream), Some(seq)) = (&commit.stream, commit.seq) {
                m.seq.insert(stream, seq + 1);
            }
            for w in &commit.shadow {
                if !m.shadow.contains_key(&w.table) {
                    m.shadow.insert(&w.table, DotNetDict::new());
                }
                if let Some(map) = m.shadow.get_mut(&w.table) {
                    for key in &w.delete_keys {
                        map.remove(key);
                    }
                    for e in &w.upserts {
                        map.insert(&e.key, e.clone());
                    }
                }
            }
            for (name, value) in commit.cursors.iter() {
                match value {
                    None => {
                        m.cursors.remove(name);
                    }
                    Some(v) => m.cursors.insert(name, v.clone()),
                }
            }
        });
        Ok(())
    }

    fn reset_table(&self, table: &str, _ctx: &Ctx) -> Result<()> {
        self.with(|m| m.shadow.remove(table));
        Ok(())
    }
}
