//! Small ordered containers used where DuckDB's iteration order is observable.
//!
//! DuckDB serializes hash maps and hash sets in their iteration order. To reproduce
//! serialized bytes exactly after a round trip, these containers keep the order in which
//! entries were inserted (or deserialized), independently of lookup indexing.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

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
pub struct CaseInsensitiveMap<V, K = String> {
    entries: Vec<(K, V)>,
    lookup: OnceLock<Box<CaseInsensitiveLookup>>,
}

/// Wide schemas need indexed name lookup. Keep small parse/catalog maps compact
/// and build this offset-only index on demand; entry order and spelling stay in
/// `entries`. Hash collisions retain every candidate for byte-wise comparison.
#[derive(Default)]
struct CaseInsensitiveLookup {
    buckets: HashMap<u64, Vec<usize>>,
}

const INDEXED_CASE_INSENSITIVE_MAP_MIN: usize = 256;

fn folded_key_hash(key: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in key {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

impl CaseInsensitiveLookup {
    fn insert(&mut self, hash: u64, index: usize) {
        self.buckets
            .entry(hash)
            .or_insert_with(|| Vec::with_capacity(1))
            .push(index);
    }

    fn find<K: AsRef<[u8]>, V>(&self, entries: &[(K, V)], key: &[u8], hash: u64) -> Option<usize> {
        self.buckets
            .get(&hash)?
            .iter()
            .copied()
            .find(|index| entries[*index].0.as_ref().eq_ignore_ascii_case(key))
    }

    fn find_key<K: AsRef<[u8]>>(&self, entries: &[K], key: &[u8], hash: u64) -> Option<usize> {
        self.buckets
            .get(&hash)?
            .iter()
            .copied()
            .find(|index| entries[*index].as_ref().eq_ignore_ascii_case(key))
    }
}

impl<V: Clone, K: Clone> Clone for CaseInsensitiveMap<V, K> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            lookup: OnceLock::new(),
        }
    }
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "The transient lookup index must not change the existing entry-only debug representation"
)]
impl<V: std::fmt::Debug, K: std::fmt::Debug> std::fmt::Debug for CaseInsensitiveMap<V, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaseInsensitiveMap")
            .field("entries", &self.entries)
            .finish()
    }
}

impl<V: PartialEq, K: PartialEq> PartialEq for CaseInsensitiveMap<V, K> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<V: Eq, K: Eq> Eq for CaseInsensitiveMap<V, K> {}

impl<V: Hash, K: Hash> Hash for CaseInsensitiveMap<V, K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.entries.hash(state);
    }
}

impl<V> CaseInsensitiveMap<V> {
    /// An empty map with UTF-8 keys.
    pub const fn new() -> Self {
        Self::new_with_keys()
    }

    /// Entries in insertion order, with keys in their original casing.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &V)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&str, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (k.as_str(), v))
    }
}

impl<V> CaseInsensitiveMap<V, crate::ByteString> {
    /// Entries with their original name bytes, including original ASCII casing.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&crate::ByteString, &V)> {
        self.iter_keys()
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&crate::ByteString, &mut V)> {
        self.entries.iter_mut().map(|(k, v)| (&*k, v))
    }
}

impl<V, K> CaseInsensitiveMap<V, K> {
    /// An empty map with the key representation selected by its type.
    pub const fn new_with_keys() -> Self {
        Self {
            entries: Vec::new(),
            lookup: OnceLock::new(),
        }
    }

    pub fn iter_keys(&self) -> impl ExactSizeIterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends a serialized entry without changing its original casing.
    pub fn push(&mut self, key: K, value: V) {
        self.lookup.take();
        self.entries.push((key, value));
    }
}

impl<V, K: AsRef<[u8]>> CaseInsensitiveMap<V, K> {
    fn position(&self, key: &[u8]) -> Option<usize> {
        if self.entries.len() < INDEXED_CASE_INSENSITIVE_MAP_MIN {
            return self
                .entries
                .iter()
                .position(|(stored, _)| stored.as_ref().eq_ignore_ascii_case(key));
        }
        let lookup = self.lookup.get_or_init(|| {
            let mut lookup = CaseInsensitiveLookup::default();
            for (index, (stored, _)) in self.entries.iter().enumerate() {
                lookup.insert(folded_key_hash(stored.as_ref()), index);
            }
            Box::new(lookup)
        });
        lookup.find(&self.entries, key, folded_key_hash(key))
    }

    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&V> {
        self.position(key.as_ref()).map(|index| &self.entries[index].1)
    }

    pub fn get_mut(&mut self, key: impl AsRef<[u8]>) -> Option<&mut V> {
        let index = self.position(key.as_ref())?;
        Some(&mut self.entries[index].1)
    }

    pub fn contains_key(&self, key: impl AsRef<[u8]>) -> bool {
        self.get(key).is_some()
    }

    /// Inserts or replaces (matching keys case-insensitively); returns the old value.
    pub fn insert(&mut self, key: impl Into<K>, value: V) -> Option<V> {
        let key = key.into();
        if let Some(slot) = self.get_mut(&key) {
            return Some(std::mem::replace(slot, value));
        }
        if let Some(lookup) = self.lookup.get_mut() {
            lookup.insert(folded_key_hash(key.as_ref()), self.entries.len());
        }
        self.entries.push((key, value));
        None
    }

    /// Removes an entry, preserving the order of the others.
    pub fn remove(&mut self, key: impl AsRef<[u8]>) -> Option<V> {
        let index = self.position(key.as_ref())?;
        self.lookup.take();
        Some(self.entries.remove(index).1)
    }
}

impl<V, K> Default for CaseInsensitiveMap<V, K> {
    fn default() -> Self {
        Self::new_with_keys()
    }
}

impl<V, K> IntoIterator for CaseInsensitiveMap<V, K> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<S: Into<K>, V, K: AsRef<[u8]>> FromIterator<(S, V)> for CaseInsensitiveMap<V, K> {
    fn from_iter<I: IntoIterator<Item = (S, V)>>(iter: I) -> Self {
        let mut map = Self::new_with_keys();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

/// An insertion-ordered set of strings compared ASCII case-insensitively
/// (DuckDB's `case_insensitive_set_t`).
pub struct CaseInsensitiveSet<K = String> {
    entries: Vec<K>,
    lookup: OnceLock<Box<CaseInsensitiveLookup>>,
}

impl<K: Clone> Clone for CaseInsensitiveSet<K> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries.clone(),
            lookup: OnceLock::new(),
        }
    }
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "The transient lookup index must not change the existing entry-only debug representation"
)]
impl<K: std::fmt::Debug> std::fmt::Debug for CaseInsensitiveSet<K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaseInsensitiveSet")
            .field("entries", &self.entries)
            .finish()
    }
}

impl<K: PartialEq> PartialEq for CaseInsensitiveSet<K> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<K: Eq> Eq for CaseInsensitiveSet<K> {}

impl<K: Hash> Hash for CaseInsensitiveSet<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.entries.hash(state);
    }
}

impl<K> Default for CaseInsensitiveSet<K> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            lookup: OnceLock::new(),
        }
    }
}

impl CaseInsensitiveSet<String> {
    /// An empty text set.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            lookup: OnceLock::new(),
        }
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }
}

impl CaseInsensitiveSet<crate::ByteString> {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &crate::ByteString> {
        self.entries.iter()
    }
}

impl<K> CaseInsensitiveSet<K> {
    /// The stored keys, retaining their original byte spelling and insertion order.
    pub fn iter_keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.entries.iter()
    }
}

impl<K: AsRef<[u8]>> CaseInsensitiveSet<K> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, value: impl AsRef<[u8]>) -> bool {
        let value = value.as_ref();
        if self.entries.len() < INDEXED_CASE_INSENSITIVE_MAP_MIN {
            return self
                .entries
                .iter()
                .any(|entry| entry.as_ref().eq_ignore_ascii_case(value));
        }
        let lookup = self.lookup.get_or_init(|| {
            let mut lookup = CaseInsensitiveLookup::default();
            for (index, entry) in self.entries.iter().enumerate() {
                lookup.insert(folded_key_hash(entry.as_ref()), index);
            }
            Box::new(lookup)
        });
        lookup
            .find_key(&self.entries, value, folded_key_hash(value))
            .is_some()
    }

    /// Adds a value unless an equal one (ignoring ASCII case) is present.
    pub fn insert(&mut self, value: impl Into<K>) -> bool {
        let value = value.into();
        if self.contains(value.as_ref()) {
            return false;
        }
        if let Some(lookup) = self.lookup.get_mut() {
            lookup.insert(folded_key_hash(value.as_ref()), self.entries.len());
        }
        self.entries.push(value);
        true
    }
}

impl<K> IntoIterator for CaseInsensitiveSet<K> {
    type Item = K;
    type IntoIter = std::vec::IntoIter<K>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<K: AsRef<[u8]>, S: Into<K>> FromIterator<S> for CaseInsensitiveSet<K> {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        let mut set = Self::default();
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
    fn wide_sets_keep_original_byte_spelling_and_trait_values() {
        let mut set = CaseInsensitiveSet::<crate::ByteString>::default();
        for i in 0..1024 {
            assert!(set.insert(crate::ByteString::from(format!("Field{i}"))));
        }
        assert!(set.insert(b"A\xe9\0X".as_slice()));
        assert!(!set.insert(b"a\xe9\0x".as_slice()));
        assert!(set.insert(b"A\xc9\0X".as_slice()));
        assert!(!set.contains(b"a\xef\xbf\xbd\0x"));
        let copied = set.clone();
        let debug = format!("{set:?}");
        let hash = |set: &CaseInsensitiveSet<crate::ByteString>| {
            let mut state = std::collections::hash_map::DefaultHasher::new();
            set.hash(&mut state);
            state.finish()
        };
        let before = hash(&set);
        assert!(copied.contains("FIELD1023"));
        assert_eq!(copied, set);
        assert_eq!(format!("{copied:?}"), debug);
        assert_eq!(hash(&copied), before);
        assert!(!set.insert("FIELD512"));
        let names: Vec<_> = set.iter().map(crate::ByteString::as_bytes).collect();
        assert_eq!(names[512], b"Field512");
        assert_eq!(&names[1024..], [b"A\xe9\0X".as_slice(), b"A\xc9\0X".as_slice()]);
    }

    #[test]
    fn wide_case_insensitive_maps_preserve_order_and_first_serialized_duplicate() {
        let mut map = CaseInsensitiveMap::new();
        for i in 0..1024 {
            assert_eq!(map.insert(format!("Field{i}"), i), None);
        }
        assert_eq!(map.get("fIeLd512"), Some(&512));
        assert_eq!(map.insert("FIELD512", 9000), Some(512));
        assert_eq!(map.remove("field100"), Some(100));
        map.push("fIeLd512".to_owned(), 7777);
        assert_eq!(map.get("field512"), Some(&9000));
        assert_eq!(map.remove("FIELD512"), Some(9000));
        assert_eq!(map.get("field512"), Some(&7777));
        *map.get_mut("FIELD512").unwrap() = 8888;
        for i in 1024..2048 {
            assert_eq!(map.insert(format!("Field{i}"), i), None);
        }
        let expected: Vec<_> = (0..1024)
            .filter(|i| ![100, 512].contains(i))
            .map(|i| (format!("Field{i}"), i))
            .chain(std::iter::once(("fIeLd512".to_owned(), 8888)))
            .chain((1024..2048).map(|i| (format!("Field{i}"), i)))
            .collect();
        assert_eq!(map.into_iter().collect::<Vec<_>>(), expected);
    }

    #[test]
    fn wide_byte_map_lookup_preserves_invalid_utf8_and_embedded_nuls() {
        let mut map = CaseInsensitiveMap::<usize, crate::ByteString>::default();
        for i in 0..1024 {
            map.insert(crate::ByteString::from(format!("field{i}")), i);
        }
        map.push(b"A\xe9\0X".as_slice().into(), 1);
        map.push(b"a\xe9\0x".as_slice().into(), 2);
        map.push(b"A\xc9\0X".as_slice().into(), 3);
        assert_eq!(map.get(b"a\xe9\0x"), Some(&1));
        assert_eq!(map.get(b"a\xc9\0x"), Some(&3));
        assert_eq!(map.get(b"a\xef\xbf\xbd\0x"), None);
        assert_eq!(map.remove(b"a\xe9\0x"), Some(1));
        assert_eq!(map.get(b"A\xe9\0X"), Some(&2));
    }

    #[test]
    fn lookup_state_does_not_change_map_hash_debug_equality_or_clone() {
        let mut map = CaseInsensitiveMap::new();
        for i in 0..1024 {
            map.push(format!("Field{i}"), i);
        }
        let clone = map.clone();
        let debug = format!("{map:?}");
        let hash = |map: &CaseInsensitiveMap<usize>| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            map.hash(&mut hasher);
            hasher.finish()
        };
        let original_hash = hash(&map);
        assert_eq!(map.get("FIELD512"), Some(&512));
        assert_eq!(map, clone);
        assert_eq!(hash(&map), original_hash);
        assert_eq!(format!("{map:?}"), debug);
        let copied = map.clone();
        assert!(copied.lookup.get().is_none());
        assert_eq!(copied.get("FIELD1023"), Some(&1023));
        assert_eq!(copied, map);
        assert_eq!(hash(&copied), original_hash);
    }

    #[test]
    fn lookup_hash_collisions_compare_full_keys_and_keep_first_match() {
        let entries = [("Alpha", 1), ("Beta", 2), ("aLPHa", 3)];
        let mut lookup = CaseInsensitiveLookup::default();
        for index in 0..entries.len() {
            lookup.insert(42, index);
        }
        assert_eq!(lookup.find(&entries, b"ALPHA", 42), Some(0));
        assert_eq!(lookup.find(&entries, b"beta", 42), Some(1));
        assert_eq!(lookup.find(&entries, b"missing", 42), None);
        assert_eq!(lookup.find(&entries, b"Alpha", 43), None);
        let keys = entries.map(|(key, _)| key);
        assert_eq!(lookup.find_key(&keys, b"ALPHA", 42), Some(0));
        assert_eq!(lookup.find_key(&keys, b"beta", 42), Some(1));
        assert_eq!(lookup.find_key(&keys, b"missing", 42), None);
    }

    #[test]
    fn small_schema_maps_keep_linear_lookup_storage() {
        let mut map = CaseInsensitiveMap::new();
        for i in 0..16 {
            map.insert(format!("Field{i}"), i);
        }
        for i in 0..16 {
            assert_eq!(map.get(format!("FIELD{i}")), Some(&i));
        }
        assert!(map.lookup.get().is_none());
        let set: CaseInsensitiveSet = (0..16).map(|i| format!("Field{i}")).collect();
        assert!(set.contains("FIELD15"));
        assert!(set.lookup.get().is_none());
    }

    #[test]
    fn byte_sets_fold_ascii_without_decoding_or_truncating() {
        let mut set = CaseInsensitiveSet::<crate::ByteString>::default();
        assert!(set.insert(b"A\xe9\0X".as_slice()));
        assert!(!set.insert(b"a\xe9\0x".as_slice()));
        assert!(set.insert(b"A\xe9\0Y".as_slice()));
        assert!(set.insert(b"A\xc9\0X".as_slice()));
        assert_eq!(set.len(), 3);
        assert!(set.contains(b"a\xe9\0x"));
        assert!(!set.contains(b"a\xef\xbf\xbd\0x"));
        assert_eq!(set.iter().next().unwrap().as_bytes(), b"A\xe9\0X");
    }

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
