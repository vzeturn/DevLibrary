//! `ResultSetDelta`: full responses with their hashes, and the per-key delta the agent
//! sends when the cloud already holds the previous snapshot.

use std::collections::{HashMap, HashSet};

use super::catalog::Operation;
use super::hasher;
use super::messages::PmsResultSet;
use super::row::PmsRow;

/// `Full`: every set with its hash, and the combined ETag.
pub fn full(sets: Vec<Vec<PmsRow>>) -> (String, Vec<PmsResultSet>) {
    let result: Vec<PmsResultSet> = sets
        .into_iter()
        .enumerate()
        .map(|(i, rows)| PmsResultSet {
            index: i as i32,
            hash: hasher::hash_rows(&rows),
            rows,
            removed_keys: None,
            key_order: None,
        })
        .collect();
    let etag = hasher::combine_etag(result.iter().map(|s| s.hash.as_str()));
    (etag, result)
}

/// `Build`: the sets that changed since `previous`, keyed sets as upserts, removed keys
/// and the new key order.
pub fn build(
    op: &Operation,
    previous: &[PmsResultSet],
    current: &[PmsResultSet],
) -> Vec<PmsResultSet> {
    let mut changed = Vec::new();
    for (i, cur) in current.iter().enumerate() {
        let prev = previous.get(i);
        if prev.is_some_and(|p| p.hash == cur.hash) {
            continue;
        }
        let keys = op.key_columns(i);
        let Some(prev) = prev.filter(|_| !keys.is_empty()) else {
            changed.push(cur.clone());
            continue;
        };
        // Insertion-ordered map: a repeated key keeps its first position, last hash.
        let mut prev_keys: Vec<String> = Vec::new();
        let mut prev_by_key: HashMap<String, String> = HashMap::new();
        for row in &prev.rows {
            let key = hasher::row_key(row, keys);
            if prev_by_key
                .insert(key.clone(), hasher::hash_row(row))
                .is_none()
            {
                prev_keys.push(key);
            }
        }
        let mut upserts = Vec::new();
        let mut order = Vec::with_capacity(cur.rows.len());
        let mut seen = HashSet::new();
        for row in &cur.rows {
            let key = hasher::row_key(row, keys);
            let changed_row = prev_by_key
                .get(&key)
                .is_none_or(|h| *h != hasher::hash_row(row));
            if changed_row {
                upserts.push(row.clone());
            }
            seen.insert(key.clone());
            order.push(key);
        }
        let removed = prev_keys
            .into_iter()
            .filter(|k| !seen.contains(k))
            .collect();
        changed.push(PmsResultSet {
            index: i as i32,
            hash: cur.hash.clone(),
            rows: upserts,
            removed_keys: Some(removed),
            key_order: Some(order),
        });
    }
    changed
}

/// `TryApply`: merges `delta` onto `base` and checks every hash; `None` when anything
/// does not add up.
pub fn try_apply(
    op: &Operation,
    base: &[PmsResultSet],
    delta: &[PmsResultSet],
    expected_etag: &str,
) -> Option<Vec<PmsResultSet>> {
    let mut result: Vec<PmsResultSet> = base.to_vec();
    for d in delta {
        let idx = usize::try_from(d.index)
            .ok()
            .filter(|&i| i <= result.len())?;
        let keys = op.key_columns(idx);
        let rows = match &d.key_order {
            Some(order) if !keys.is_empty() && idx < result.len() => {
                let mut by_key: HashMap<String, PmsRow> = HashMap::new();
                for row in &result[idx].rows {
                    by_key.insert(hasher::row_key(row, keys), row.clone());
                }
                for k in d.removed_keys.iter().flatten() {
                    by_key.remove(k);
                }
                for row in &d.rows {
                    by_key.insert(hasher::row_key(row, keys), row.clone());
                }
                let mut ordered = Vec::with_capacity(order.len());
                for k in order {
                    ordered.push(by_key.get(k)?.clone());
                }
                ordered
            }
            _ => d.rows.clone(),
        };
        if hasher::hash_rows(&rows) != d.hash {
            return None;
        }
        let set = PmsResultSet {
            index: d.index,
            hash: d.hash.clone(),
            rows,
            removed_keys: None,
            key_order: None,
        };
        if idx == result.len() {
            result.push(set);
        } else {
            result[idx] = set;
        }
    }
    (hasher::combine_etag(result.iter().map(|s| s.hash.as_str())) == expected_etag)
        .then_some(result)
}
