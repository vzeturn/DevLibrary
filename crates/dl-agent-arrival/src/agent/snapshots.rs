//! `SnapshotStore`: a bounded LRU of the last result sent to the cloud per (operation,
//! parameters, projection), the baseline for `NotModified` and `Delta` answers. In memory
//! only: after a restart the next request is answered in full once, then deltas resume.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use crate::contracts::PmsResultSet;

/// One remembered answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Its ETag.
    pub etag: String,
    /// Its full result sets.
    pub sets: Arc<Vec<PmsResultSet>>,
}

#[derive(Debug, Default)]
struct Inner {
    tick: u64,
    map: HashMap<String, (u64, Snapshot)>,
    order: BTreeMap<u64, String>,
}

/// The store.
#[derive(Debug)]
pub struct SnapshotStore {
    capacity: usize,
    inner: Mutex<Inner>,
}

impl SnapshotStore {
    /// A store keeping at most `max(1, capacity)` snapshots.
    pub fn new(capacity: usize) -> SnapshotStore {
        SnapshotStore {
            capacity: capacity.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The snapshot under `key`, marked as most recently used.
    pub fn get(&self, key: &str) -> Option<Snapshot> {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let inner = &mut *g;
        inner.tick += 1;
        let tick = inner.tick;
        let (stamp, snap) = inner.map.get_mut(key)?;
        inner.order.remove(stamp);
        *stamp = tick;
        inner.order.insert(tick, key.to_string());
        Some(snap.clone())
    }

    /// Stores `snapshot` under `key`, evicting the least recently used beyond capacity.
    pub fn put(&self, key: &str, snapshot: Snapshot) {
        let mut g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let inner = &mut *g;
        inner.tick += 1;
        let tick = inner.tick;
        if let Some((old, _)) = inner.map.insert(key.to_string(), (tick, snapshot)) {
            inner.order.remove(&old);
        }
        inner.order.insert(tick, key.to_string());
        while inner.map.len() > self.capacity {
            let Some((_, oldest)) = inner.order.pop_first() else {
                break;
            };
            inner.map.remove(&oldest);
        }
    }

    /// Number of snapshots held.
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .map
            .len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(etag: &str) -> Snapshot {
        Snapshot {
            etag: etag.into(),
            sets: Arc::new(Vec::new()),
        }
    }

    #[test]
    fn evicts_the_least_recently_used() {
        let s = SnapshotStore::new(2);
        s.put("a", snap("1"));
        s.put("b", snap("2"));
        assert_eq!(s.get("a").unwrap().etag, "1");
        s.put("c", snap("3"));
        assert!(s.get("b").is_none());
        assert_eq!(s.get("a").unwrap().etag, "1");
        s.put("a", snap("4"));
        assert_eq!(s.get("a").unwrap().etag, "4");
        assert_eq!(s.len(), 2);
        let one = SnapshotStore::new(0);
        one.put("x", snap("1"));
        one.put("y", snap("2"));
        assert_eq!(one.len(), 1);
    }
}
