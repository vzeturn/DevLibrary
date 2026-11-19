//! A table's shadow as the cloud manifest (`State/ShadowManifest.cs`, ADR 0126/0128).
//!
//! Byte for byte as `PgSyncIngestService.ManifestAsync` computes it over `sync.row_state`:
//! row key = [`row_key::of`] of the wire key (the shadow keeps it in [`key_codec`] form),
//! bucket = [`row_key::bucket`], digest = lowercase hex SHA-256 over the UTF-8 lines
//! `row_key|row_hash` sorted by the UTF-8 bytes of the row key (PostgreSQL `COLLATE "C"`)
//! and joined by `\n`. Buckets without rows are absent on both sides. Deleted rows are gone
//! from the shadow and are tombstones (left out) in the cloud.

use std::collections::{BTreeMap, HashMap, HashSet};

use dl_hash::{Digest, Sha256};

use super::codecs::key_codec;
use super::error::{CaptureError, Result};
use super::state::CaptureState;
use crate::contracts::hashing::row_key;
use crate::util::ctx::Ctx;

/// Most rows held in memory at once; a larger table is read again for each range of buckets.
pub const ROWS_PER_PASS: usize = 200_000;

/// Row count and digest of one manifest bucket, as the cloud's `GET /sync/v1/manifest`
/// answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestBucket {
    /// Bucket number (0..buckets).
    pub bucket: i32,
    /// Rows in the bucket.
    pub count: i32,
    /// Lowercase hex SHA-256 of the bucket's lines.
    pub digest: String,
}

impl ManifestBucket {
    /// A bucket.
    pub fn new(bucket: i32, count: i32, digest: &str) -> ManifestBucket {
        ManifestBucket {
            bucket,
            count,
            digest: digest.to_owned(),
        }
    }
}

/// A table's shadow as a manifest. `duplicate_keys` is true when several shadow entries
/// share one wire key (the `#n` occurrences of a site that breaks the logical key): the
/// cloud keeps only one of them, so the table cannot be compared and `buckets` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ShadowManifestResult {
    /// Rows counted.
    pub rows: i32,
    /// Non-empty buckets, ascending.
    pub buckets: Vec<ManifestBucket>,
    /// The shadow holds duplicate key occurrences.
    pub duplicate_keys: bool,
}

fn check_buckets(buckets: i32) -> Result<u32> {
    if !(1..=256).contains(&buckets) {
        return Err(CaptureError::argument(format!(
            "buckets ('{buckets}') must be between 1 and 256. (Parameter 'buckets')"
        )));
    }
    Ok(buckets.unsigned_abs())
}

/// The cloud row key of a shadow key, `None` for a `#n` occurrence (or any non-canonical
/// form: it does not survive the round trip).
fn cloud_key(shadow_key: &str) -> Result<Option<String>> {
    let wire = key_codec::decode(shadow_key)?;
    if key_codec::encode(&wire) != shadow_key {
        return Ok(None);
    }
    Ok(Some(row_key::of(&wire)))
}

/// Visits every shadow entry of `table` as (cloud row key, hash). Stops at the first
/// duplicate occurrence and returns `false` then.
fn scan(
    state: &dyn CaptureState,
    table: &str,
    ctx: &Ctx,
    mut visit: impl FnMut(String, &str),
) -> Result<bool> {
    let mut duplicate = false;
    let mut failure: Option<CaptureError> = None;
    state.scan_shadow(
        table,
        &mut |key, hash| {
            if duplicate || failure.is_some() {
                return;
            }
            match cloud_key(key) {
                Ok(Some(k)) => visit(k, hash),
                Ok(None) => duplicate = true,
                Err(e) => failure = Some(e),
            }
        },
        ctx,
    )?;
    match failure {
        Some(e) => Err(e),
        None => Ok(!duplicate),
    }
}

/// `ShadowManifest.BuildAsync`: the manifest of `table` with `buckets` buckets (1..256),
/// read in passes of at most `rows_per_pass` rows ([`ROWS_PER_PASS`]).
pub fn build(
    state: &dyn CaptureState,
    table: &str,
    buckets: i32,
    rows_per_pass: usize,
    ctx: &Ctx,
) -> Result<ShadowManifestResult> {
    let n = check_buckets(buckets)?;
    if rows_per_pass < 1 {
        return Err(CaptureError::argument(
            "rowsPerPass ('0') must be greater than or equal to '1'. (Parameter 'rowsPerPass')",
        ));
    }
    let total = state.count_shadow(table, ctx)?;
    let passes = total.div_ceil(rows_per_pass).clamp(1, n as usize) as u32;
    let mut result = Vec::new();
    let mut rows = 0i32;
    for pass in 0..passes {
        let lo = pass * n / passes;
        let hi = (pass + 1) * n / passes;
        let mut lines: BTreeMap<u32, Vec<(String, String)>> = BTreeMap::new();
        let complete = scan(state, table, ctx, |key, hash| {
            let bucket = row_key::bucket(&key, n);
            if bucket >= lo && bucket < hi {
                lines
                    .entry(bucket)
                    .or_default()
                    .push((key, hash.to_owned()));
            }
        })?;
        if !complete {
            return Ok(ShadowManifestResult {
                rows: 0,
                buckets: Vec::new(),
                duplicate_keys: true,
            });
        }
        for (bucket, list) in lines {
            let count = i32::try_from(list.len()).unwrap_or(i32::MAX);
            rows = rows.saturating_add(count);
            result.push(ManifestBucket {
                bucket: bucket as i32,
                count,
                digest: digest(list),
            });
        }
    }
    Ok(ShadowManifestResult {
        rows,
        buckets: result,
        duplicate_keys: false,
    })
}

/// `ShadowManifest.BucketRowsAsync`: row key to hash of the shadow entries in the `wanted`
/// buckets (ADR 0128), keyed as the cloud keys them, for a row-by-row comparison with
/// `GET /sync/v1/manifest/keys`. `None` when the shadow holds duplicate key occurrences.
pub fn bucket_rows(
    state: &dyn CaptureState,
    table: &str,
    buckets: i32,
    wanted: &HashSet<i32>,
    ctx: &Ctx,
) -> Result<Option<HashMap<String, String>>> {
    let n = check_buckets(buckets)?;
    let mut rows = HashMap::new();
    let complete = scan(state, table, ctx, |key, hash| {
        if wanted.contains(&(row_key::bucket(&key, n) as i32)) {
            rows.insert(key, hash.to_owned());
        }
    })?;
    Ok(complete.then_some(rows))
}

/// Bucket numbers whose count or digest differ (digests compared ignoring case),
/// including buckets present on one side only, ascending.
pub fn mismatched(store: &[ManifestBucket], cloud: &[ManifestBucket]) -> Vec<i32> {
    let mine: HashMap<i32, &ManifestBucket> = store.iter().map(|b| (b.bucket, b)).collect();
    let theirs: HashMap<i32, &ManifestBucket> = cloud.iter().map(|b| (b.bucket, b)).collect();
    let mut all: Vec<i32> = mine.keys().chain(theirs.keys()).copied().collect();
    all.sort_unstable();
    all.dedup();
    all.into_iter()
        .filter(|b| match (mine.get(b), theirs.get(b)) {
            (Some(m), Some(t)) => m.count != t.count || !m.digest.eq_ignore_ascii_case(&t.digest),
            _ => true,
        })
        .collect()
}

fn digest(mut lines: Vec<(String, String)>) -> String {
    lines.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut sha = Sha256::default();
    for (i, (key, hash)) in lines.iter().enumerate() {
        if i > 0 {
            sha.update(b"\n");
        }
        sha.update(key.as_bytes());
        sha.update(b"|");
        sha.update(hash.as_bytes());
    }
    dl_encoding::hex::encode(sha.finalize())
}
