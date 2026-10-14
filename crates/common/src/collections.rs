//! Small ordered containers used where DuckDB's iteration order is observable.
//!
//! DuckDB serializes hash maps and hash sets in their iteration order. To reproduce
//! serialized bytes exactly after a round trip, these containers keep the order in which
//! entries were inserted (or deserialized) instead of hashing.

use std::borrow::Borrow;

/// An insertion-ordered map backed by a vector.
///
/// Models DuckDB's `InsertionOrderPreservingMap` and stands in for `unordered_map`s whose
/// order is fixed by the serialized form. Lookups are linear, which is the right trade-off
/// for the handful of entries these maps hold in parse trees and catalog entries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OrderedMap<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> OrderedMap<K, V> {
    /// An empty map.
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Mutable entries in insertion order.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&K, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.entries.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = &V> {
        self.entries.iter().map(|(_, v)| v)
    }

    /// Appends an entry without checking for an existing key (used when deserializing,
    /// where duplicates cannot occur in well-formed input).
    pub fn push(&mut self, key: K, value: V) {
        self.entries.push((key, value));
    }
}

impl<K: PartialEq, V> OrderedMap<K, V> {
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries
            .iter()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.entries
            .iter_mut()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.get(key).is_some()
    }

    /// Inserts or replaces; a replaced entry keeps its position. Returns the old value.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(slot) = self.get_mut(&key) {
            return Some(std::mem::replace(slot, value));
        }
        self.entries.push((key, value));
        None
    }

    /// Removes an entry, preserving the order of the others.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        let index = self.entries.iter().position(|(k, _)| k.borrow() == key)?;
        Some(self.entries.remove(index).1)
    }
}

impl<K, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: PartialEq, V> FromIterator<(K, V)> for OrderedMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

impl<K, V> IntoIterator for OrderedMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

/// A string-keyed, insertion-ordered map with ASCII case-insensitive keys
/// (DuckDB's `case_insensitive_map_t`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CaseInsensitiveMap<V> {
    entries: Vec<(String, V)>,
}

impl<V> CaseInsensitiveMap<V> {
    /// An empty map.
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in insertion order, with keys in their original casing.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Mutable entries in insertion order.
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&str, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (k.as_str(), v))
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Inserts or replaces (matching keys case-insensitively); returns the old value.
    pub fn insert(&mut self, key: impl Into<String>, value: V) -> Option<V> {
        let key = key.into();
        if let Some(slot) = self.get_mut(&key) {
            return Some(std::mem::replace(slot, value));
        }
        self.entries.push((key, value));
        None
    }

    /// Removes an entry, preserving the order of the others.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let index = self
            .entries
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(key))?;
        Some(self.entries.remove(index).1)
    }

    /// Appends an entry without checking for an existing key.
    pub fn push(&mut self, key: String, value: V) {
        self.entries.push((key, value));
    }
}

impl<V> Default for CaseInsensitiveMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> IntoIterator for CaseInsensitiveMap<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<(String, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K: Into<String>, V> FromIterator<(K, V)> for CaseInsensitiveMap<V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// An insertion-ordered set of strings compared ASCII case-insensitively
/// (DuckDB's `case_insensitive_set_t`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct CaseInsensitiveSet {
    entries: Vec<String>,
}

impl CaseInsensitiveSet {
    /// An empty set.
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }

    pub fn contains(&self, value: &str) -> bool {
        self.entries.iter().any(|v| v.eq_ignore_ascii_case(value))
    }

    /// Adds a value unless an equal one (ignoring case) is present; returns whether it was added.
    pub fn insert(&mut self, value: impl Into<String>) -> bool {
        let value = value.into();
        if self.contains(&value) {
            return false;
        }
        self.entries.push(value);
        true
    }
}

impl IntoIterator for CaseInsensitiveSet {
    type Item = String;
    type IntoIter = std::vec::IntoIter<String>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<S: Into<String>> FromIterator<S> for CaseInsensitiveSet {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut set = Self::new();
        for value in iter {
            set.insert(value);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_map_keeps_insertion_order_on_replace() {
        let mut map = OrderedMap::new();
        map.insert("b", 1);
        map.insert("a", 2);
        assert_eq!(map.insert("b", 3), Some(1));
        assert_eq!(map.iter().collect::<Vec<_>>(), [(&"b", &3), (&"a", &2)]);
        assert_eq!(map.remove("b"), Some(3));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn case_insensitive_lookups() {
        let mut map = CaseInsensitiveMap::new();
        map.insert("Foo", 1);
        assert_eq!(map.get("FOO"), Some(&1));
        assert_eq!(map.insert("foo", 2), Some(1));
        assert_eq!(map.iter().next(), Some(("Foo", &2)));

        let mut set = CaseInsensitiveSet::new();
        assert!(set.insert("Col"));
        assert!(!set.insert("col"));
        assert!(set.contains("COL"));
    }
}
