//! Insertion-ordered JSON object.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::{fmt, iter::FusedIterator, mem, slice};

use crate::Value;

/// Maps up to this many entries are searched linearly and carry no index.
const SMALL: usize = 8;

/// A JSON object: string keys mapped to [`Value`]s, iterated in insertion order.
///
/// Entries live in a `Vec`. Maps with more than 8 entries also keep a sorted index of
/// entry positions, so lookups are `O(log n)`; small maps are scanned linearly, which is
/// faster at that size. Inserting a new key into a large map shifts the index
/// (`O(n)` moves of `usize`, done with `memmove`); building a map with
/// [`collect`](Iterator::collect) or [`extend`](Extend::extend) sorts once instead.
/// [`remove`](Map::remove) keeps the order of the remaining entries and is `O(n)`.
///
/// Equality ignores order, because JSON objects are unordered (RFC 8259 section 4).
///
/// ```
/// use dl_json::{Map, Value};
///
/// let mut room = Map::new();
/// room.insert("number", 1205);
/// room.insert("type", "deluxe");
/// room.insert("number", 1206); // existing key: value replaced, position kept
/// assert_eq!(room.keys().collect::<Vec<_>>(), ["number", "type"]);
/// assert_eq!(room.get("number"), Some(&Value::from(1206)));
/// ```
#[derive(Clone, Default)]
pub struct Map {
    entries: Vec<(String, Value)>,
    /// Entry positions sorted by key; present iff `entries.len() > SMALL`.
    /// Boxed so that `Map` (and therefore `Value`) stays small; most objects are small
    /// and never allocate it.
    #[allow(clippy::box_collection)]
    index: Option<Box<Vec<usize>>>,
}

impl Map {
    /// Creates an empty map.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: None,
        }
    }

    /// Creates an empty map with room for `capacity` entries.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            index: None,
        }
    }

    /// Builds a map from entries that may contain duplicate keys: the last value wins
    /// and keeps the position of the first occurrence (same as repeated `insert`).
    pub(crate) fn from_entries(mut entries: Vec<(String, Value)>) -> Self {
        if entries.len() <= SMALL {
            let mut i = 1;
            while i < entries.len() {
                let (head, tail) = entries.split_at_mut(i);
                if let Some(first) = head.iter_mut().find(|(k, _)| *k == tail[0].0) {
                    first.1 = mem::take(&mut tail[0].1);
                    entries.remove(i);
                } else {
                    i += 1;
                }
            }
            return Self {
                entries,
                index: None,
            };
        }
        let mut index = sorted_index(&entries);
        if index.windows(2).any(|w| entries[w[0]].0 == entries[w[1]].0) {
            // Runs of equal keys are ordered by position (the sort breaks ties by it).
            let mut keep = alloc::vec![true; entries.len()];
            let mut i = 0;
            while i < index.len() {
                let mut j = i + 1;
                while j < index.len() && entries[index[j]].0 == entries[index[i]].0 {
                    j += 1;
                }
                if j - i > 1 {
                    let last = mem::take(&mut entries[index[j - 1]].1);
                    entries[index[i]].1 = last;
                    for &p in &index[i + 1..j] {
                        keep[p] = false;
                    }
                }
                i = j;
            }
            let mut flags = keep.into_iter();
            entries.retain(|_| flags.next().unwrap_or(true));
            if entries.len() <= SMALL {
                return Self {
                    entries,
                    index: None,
                };
            }
            index = sorted_index(&entries);
        }
        Self {
            entries,
            index: Some(Box::new(index)),
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the map has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Finds `key`. `Ok(entry position)` if present, otherwise `Err(slot)` where `slot`
    /// is the insertion point in the sorted index (0 when there is no index).
    fn find(&self, key: &str) -> Result<usize, usize> {
        match &self.index {
            None => self.entries.iter().position(|(k, _)| k == key).ok_or(0),
            Some(index) => index
                .binary_search_by(|&i| self.entries[i].0.as_str().cmp(key))
                .map(|slot| index[slot]),
        }
    }

    /// Returns the value for `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.find(key).ok().map(|i| &self.entries[i].1)
    }

    /// Returns a mutable reference to the value for `key`.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.find(key).ok().map(|i| &mut self.entries[i].1)
    }

    /// `true` if the map contains `key`.
    pub fn contains_key(&self, key: &str) -> bool {
        self.find(key).is_ok()
    }

    /// Inserts a value. If the key already exists its value is replaced, the entry keeps
    /// its position, and the old value is returned. Otherwise the entry is appended.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> Option<Value> {
        let key = key.into();
        let value = value.into();
        match self.find(&key) {
            Ok(i) => Some(mem::replace(&mut self.entries[i].1, value)),
            Err(slot) => {
                let pos = self.entries.len();
                self.entries.push((key, value));
                match &mut self.index {
                    Some(index) => index.insert(slot, pos),
                    None if self.entries.len() > SMALL => {
                        self.index = Some(Box::new(sorted_index(&self.entries)));
                    }
                    None => {}
                }
                None
            }
        }
    }

    /// Removes `key` and returns its value. The remaining entries keep their order.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let pos = self.find(key).ok()?;
        let (_, value) = self.entries.remove(pos);
        if self.entries.len() <= SMALL {
            self.index = None;
        } else if let Some(index) = &mut self.index {
            index.retain(|&i| i != pos);
            for i in index.iter_mut() {
                if *i > pos {
                    *i -= 1;
                }
            }
        }
        Some(value)
    }

    /// Iterates over `(key, value)` pairs in insertion order.
    pub fn iter(&self) -> Iter<'_> {
        Iter(self.entries.iter())
    }

    /// Iterates over `(key, mutable value)` pairs in insertion order.
    pub fn iter_mut(&mut self) -> IterMut<'_> {
        IterMut(self.entries.iter_mut())
    }

    /// Iterates over the keys in insertion order.
    pub fn keys(&self) -> Keys<'_> {
        Keys(self.entries.iter())
    }

    /// Iterates over the values in insertion order.
    pub fn values(&self) -> Values<'_> {
        Values(self.entries.iter())
    }

    /// Iterates over mutable values in insertion order.
    pub fn values_mut(&mut self) -> ValuesMut<'_> {
        ValuesMut(self.entries.iter_mut())
    }
}

fn sorted_index(entries: &[(String, Value)]) -> Vec<usize> {
    let mut index: Vec<usize> = (0..entries.len()).collect();
    index.sort_unstable_by(|&a, &b| entries[a].0.cmp(&entries[b].0).then(a.cmp(&b)));
    index
}

impl core::ops::Index<&str> for Map {
    type Output = Value;
    /// Returns the value for `key`, or [`Value::Null`] if it is missing. Never panics.
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&crate::value::NULL)
    }
}

impl PartialEq for Map {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl fmt::Debug for Map {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Map {
    /// Collects pairs; for duplicate keys the last value wins at the first position.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Map::from_entries(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl<K: Into<String>, V: Into<Value>> Extend<(K, V)> for Map {
    /// Inserts every pair, with the same semantics as [`Map::insert`].
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        let new: Vec<(String, Value)> = iter
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        if new.len() <= 16 {
            for (k, v) in new {
                self.insert(k, v);
            }
        } else {
            let mut all = mem::take(&mut self.entries);
            all.extend(new);
            *self = Map::from_entries(all);
        }
    }
}

impl IntoIterator for Map {
    type Item = (String, Value);
    type IntoIter = IntoIter;
    fn into_iter(self) -> IntoIter {
        IntoIter(self.entries.into_iter())
    }
}

impl<'a> IntoIterator for &'a Map {
    type Item = (&'a String, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut Map {
    type Item = (&'a String, &'a mut Value);
    type IntoIter = IterMut<'a>;
    fn into_iter(self) -> IterMut<'a> {
        self.iter_mut()
    }
}

macro_rules! iterator {
    ($(#[$doc:meta])* $name:ident<$lt:lifetime>, $inner:ty, $item:ty, |$e:ident| $map:expr) => {
        $(#[$doc])*
        #[derive(Debug)]
        pub struct $name<$lt>($inner);

        impl<$lt> Iterator for $name<$lt> {
            type Item = $item;
            fn next(&mut self) -> Option<$item> {
                self.0.next().map(|$e| $map)
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                self.0.size_hint()
            }
        }
        impl<$lt> DoubleEndedIterator for $name<$lt> {
            fn next_back(&mut self) -> Option<$item> {
                self.0.next_back().map(|$e| $map)
            }
        }
        impl<$lt> ExactSizeIterator for $name<$lt> {}
        impl<$lt> FusedIterator for $name<$lt> {}
    };
}

iterator!(
    /// Iterator over `(&key, &value)`, see [`Map::iter`].
    Iter<'a>, slice::Iter<'a, (String, Value)>, (&'a String, &'a Value), |e| (&e.0, &e.1)
);
iterator!(
    /// Iterator over `(&key, &mut value)`, see [`Map::iter_mut`].
    IterMut<'a>, slice::IterMut<'a, (String, Value)>, (&'a String, &'a mut Value),
    |e| (&e.0, &mut e.1)
);
iterator!(
    /// Iterator over keys, see [`Map::keys`].
    Keys<'a>, slice::Iter<'a, (String, Value)>, &'a String, |e| &e.0
);
iterator!(
    /// Iterator over values, see [`Map::values`].
    Values<'a>, slice::Iter<'a, (String, Value)>, &'a Value, |e| &e.1
);
iterator!(
    /// Iterator over mutable values, see [`Map::values_mut`].
    ValuesMut<'a>, slice::IterMut<'a, (String, Value)>, &'a mut Value, |e| &mut e.1
);

/// Owning iterator over `(key, value)`, see [`Map::into_iter`](IntoIterator::into_iter).
#[derive(Debug)]
pub struct IntoIter(alloc::vec::IntoIter<(String, Value)>);

impl Iterator for IntoIter {
    type Item = (String, Value);
    fn next(&mut self) -> Option<(String, Value)> {
        self.0.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl DoubleEndedIterator for IntoIter {
    fn next_back(&mut self) -> Option<(String, Value)> {
        self.0.next_back()
    }
}
impl ExactSizeIterator for IntoIter {}
impl FusedIterator for IntoIter {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::vec;

    fn check_invariants(m: &Map) {
        match &m.index {
            None => assert!(m.len() <= SMALL),
            Some(index) => {
                assert!(m.len() > SMALL);
                assert_eq!(index.len(), m.len());
                for w in index.windows(2) {
                    assert!(m.entries[w[0]].0 < m.entries[w[1]].0);
                }
            }
        }
        for (i, (k, _)) in m.entries.iter().enumerate() {
            assert_eq!(m.find(k), Ok(i));
        }
    }

    #[test]
    fn insert_get_remove_keep_order() {
        for n in [3usize, 8, 9, 50, 300] {
            let mut m = Map::new();
            // Insert in a scrambled order so the sorted index differs from insertion order.
            let keys: Vec<String> = (0..n).map(|i| format!("k{}", (i * 7919) % n)).collect();
            for (i, k) in keys.iter().enumerate() {
                assert_eq!(m.insert(k.as_str(), i), None);
                check_invariants(&m);
            }
            assert_eq!(m.len(), n);
            assert_eq!(m.keys().cloned().collect::<Vec<_>>(), keys);
            assert_eq!(m.insert(keys[0].as_str(), "new"), Some(Value::from(0)));
            assert_eq!(m.keys().next(), Some(&keys[0]));
            assert_eq!(m.get(&keys[0]), Some(&Value::from("new")));
            assert!(m.get("missing").is_none() && !m.contains_key("missing"));
            // Remove every other key; the rest keep their relative order.
            for k in keys.iter().step_by(2) {
                assert!(m.remove(k).is_some());
                check_invariants(&m);
            }
            assert!(m.remove("missing").is_none());
            let expect: Vec<&String> = keys.iter().skip(1).step_by(2).collect();
            assert_eq!(m.keys().collect::<Vec<_>>(), expect);
        }
    }

    #[test]
    fn duplicates_last_wins_first_position() {
        for n in [4usize, 20] {
            let mut entries: Vec<(String, Value)> =
                (0..n).map(|i| (format!("k{i}"), Value::from(i))).collect();
            entries.push(("k1".into(), Value::from("last")));
            entries.push(("k0".into(), Value::from("x")));
            entries.push(("k0".into(), Value::from("y")));
            let m = Map::from_entries(entries);
            check_invariants(&m);
            assert_eq!(m.len(), n);
            assert_eq!(m.keys().take(2).collect::<Vec<_>>(), ["k0", "k1"]);
            assert_eq!(m["k0"], "y");
            assert_eq!(m["k1"], "last");
        }
        // Collapsing below the threshold drops the index.
        let m: Map = (0..12).map(|i| (format!("k{}", i % 3), i)).collect();
        check_invariants(&m);
        assert_eq!(m.len(), 3);
        assert_eq!(m["k0"], 9);
    }

    #[test]
    fn iterators_and_traits() {
        let mut m: Map = vec![("a", 1), ("b", 2), ("c", 3)].into_iter().collect();
        for v in m.values_mut() {
            *v = Value::from(v.as_i64().unwrap() * 10);
        }
        for (_, v) in &mut m {
            *v = Value::from(v.as_i64().unwrap() + 1);
        }
        assert_eq!(
            m.values().map(|v| v.as_i64().unwrap()).collect::<Vec<_>>(),
            [11, 21, 31]
        );
        assert_eq!(m.iter().next_back().map(|(k, _)| k.as_str()), Some("c"));
        assert_eq!(m.iter().len(), 3);
        m.extend([("d", 4), ("a", 0)]);
        assert_eq!(m.keys().collect::<Vec<_>>(), ["a", "b", "c", "d"]);
        m.extend((0..20).map(|i| (format!("x{i}"), i)));
        check_invariants(&m);
        assert_eq!(m.len(), 24);
        let owned: Vec<(String, Value)> = m.clone().into_iter().collect();
        assert_eq!(owned[3], ("d".into(), Value::from(4)));
        assert_eq!(
            format!("{:?}", Map::from_iter([("k", true)])),
            r#"{"k": Bool(true)}"#
        );
        assert!(Map::with_capacity(4).is_empty());
    }

    #[test]
    fn equality_ignores_order() {
        let a: Map = [("x", 1), ("y", 2)].into_iter().collect();
        let b: Map = [("y", 2), ("x", 1)].into_iter().collect();
        let c: Map = [("y", 2), ("x", 3)].into_iter().collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, Map::new());
    }
}
