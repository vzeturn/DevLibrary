//! An insertion-ordered string map, the Rust form of the C# `Dictionary<string, T>` the
//! contracts carry (`SyncRow.Key`, `SyncRow.Columns`, ...): System.Text.Json writes a
//! dictionary in insertion order, so the order is part of the wire bytes.

/// Insertion-ordered map with ordinal (case-sensitive) keys. Inserting an existing key
/// replaces the value in place, like the C# indexer. Equality ignores order (two maps
/// are equal when they hold the same pairs).
#[derive(Debug, Clone)]
pub struct OrderedMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        OrderedMap {
            entries: Vec::new(),
        }
    }
}

impl<V: PartialEq> PartialEq for OrderedMap<V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .entries
                .iter()
                .all(|(k, v)| other.get(k).is_some_and(|o| o == v))
    }
}

impl<V: Eq> Eq for OrderedMap<V> {}

impl<V> OrderedMap<V> {
    /// An empty map.
    pub fn new() -> Self {
        OrderedMap::default()
    }

    /// An empty map with room for `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        OrderedMap {
            entries: Vec::with_capacity(n),
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the map is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inserts or replaces (in place); returns the old value.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        let key = key.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            return Some(std::mem::replace(&mut slot.1, value));
        }
        self.entries.push((key, value));
        None
    }

    /// The value of `key` (ordinal).
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The value of `key`, mutable.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// The first entry whose key equals `key` ignoring case (`OrdinalIgnoreCase`).
    pub fn get_ignore_case(&self, key: &str) -> Option<(&str, &V)> {
        self.entries
            .iter()
            .find(|(k, _)| crate::util::stj::eq_ignore_case(k, key))
            .map(|(k, v)| (k.as_str(), v))
    }

    /// Whether `key` is present (ordinal).
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Removes `key`, keeping the order of the others.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// Values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    /// The entries as a slice.
    pub fn as_slice(&self) -> &[(String, V)] {
        &self.entries
    }
}

impl<V> IntoIterator for OrderedMap<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<(String, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K: Into<String>, V> FromIterator<(K, V)> for OrderedMap<V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut m = OrderedMap::new();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

impl<K: Into<String>, V> Extend<(K, V)> for OrderedMap<V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

/// A row key or column map: column name to canonical value (`None` = SQL NULL).
pub type StrMap = OrderedMap<Option<String>>;

/// Builds a [`StrMap`] from `(name, value)` pairs (test and fixture helper).
pub fn str_map<'a>(pairs: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> StrMap {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.map(str::to_owned)))
        .collect()
}
