//! The iteration order of `std::unordered_map` / `std::unordered_set` in libc++.
//!
//! DuckDB serializes several hash containers (`named_param_map`, the `EXCLUDE`,
//! `REPLACE` and `RENAME` lists of a star expression, ...) in their iteration order,
//! which depends on the keys' hashes and on the history of insertions and rehashes.
//! The reference build uses libc++, whose `__hash_table` keeps all elements in one
//! singly linked list and every bucket points at the node *before* its first element.
//! [`HashOrder`] replays that algorithm on hashes alone, so the parser can produce
//! entries in exactly the order DuckDB writes them.
//!
//! Copy-assigning a map copies its node list as is (same order, same bucket count), so
//! copies need no emulation.

const NIL: usize = usize::MAX;
/// Index of the list's before-begin anchor (`__first_node_`).
const ANCHOR: usize = 0;

/// The node list and bucket array of a libc++ hash table, without the values.
///
/// Node `i` (1-based; 0 is the anchor) stands for the `i`-th inserted element; callers
/// keep the elements themselves and map iteration results back through these ids. The
/// anchor is only allocated with the first element, so empty tables cost nothing.
#[derive(Clone, Debug, Default)]
pub(crate) struct HashOrder {
    hashes: Vec<u64>,
    next: Vec<usize>,
    buckets: Vec<usize>,
    size: usize,
}

/// `std::__constrain_hash`.
const fn constrain(hash: u64, bucket_count: usize) -> usize {
    let bc = bucket_count as u64;
    (if bc.is_power_of_two() {
        hash & (bc - 1)
    } else if hash < bc {
        hash
    } else {
        hash % bc
    }) as usize
}

/// `std::__is_hash_power2`.
const fn is_hash_power2(bucket_count: usize) -> bool {
    bucket_count > 2 && bucket_count.is_power_of_two()
}

/// `std::__next_prime`: the smallest prime not below `n`.
fn next_prime(n: usize) -> usize {
    let is_prime = |candidate: usize| {
        let mut divisor = 2;
        while divisor * divisor <= candidate {
            if candidate.is_multiple_of(divisor) {
                return false;
            }
            divisor += 1;
        }
        true
    };
    let mut candidate = n.max(2);
    while !is_prime(candidate) {
        candidate += 1;
    }
    candidate
}

impl HashOrder {
    /// An empty table with no buckets (a default-constructed container).
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// `clear()`: removes all elements but keeps the buckets.
    pub(crate) fn clear(&mut self) {
        let bucket_count = self.buckets.len();
        *self = Self {
            buckets: vec![NIL; bucket_count],
            ..Self::default()
        };
    }

    /// The node ids in iteration order.
    pub(crate) fn order(&self) -> Vec<usize> {
        let mut result = Vec::with_capacity(self.size);
        let mut node = self.next.first().copied().unwrap_or(NIL);
        while node != NIL {
            result.push(node);
            node = self.next[node];
        }
        result
    }

    /// Finds an element with this hash for which `eq` holds (`find`).
    pub(crate) fn find(&self, hash: u64, mut eq: impl FnMut(usize) -> bool) -> Option<usize> {
        let bucket_count = self.buckets.len();
        if bucket_count == 0 {
            return None;
        }
        let chash = constrain(hash, bucket_count);
        let before = self.buckets[chash];
        if before == NIL {
            return None;
        }
        let mut node = self.next[before];
        while node != NIL
            && (self.hashes[node] == hash || constrain(self.hashes[node], bucket_count) == chash)
        {
            if self.hashes[node] == hash && eq(node) {
                return Some(node);
            }
            node = self.next[node];
        }
        None
    }

    /// `__emplace_unique`: inserts unless an equal element exists. Returns the new node
    /// id, or `Err` with the existing one.
    pub(crate) fn insert_unique(&mut self, hash: u64, eq: impl FnMut(usize) -> bool) -> Result<usize, usize> {
        if let Some(existing) = self.find(hash, eq) {
            return Err(existing);
        }
        if self.next.is_empty() {
            self.hashes.push(0);
            self.next.push(NIL);
        }
        self.hashes.push(hash);
        self.next.push(NIL);
        let node = self.hashes.len() - 1;
        let bucket_count = self.buckets.len();
        if bucket_count == 0 || (self.size + 1) as f32 > bucket_count as f32 {
            let wanted = (2 * bucket_count + usize::from(!is_hash_power2(bucket_count))).max(self.size + 1);
            self.rehash(wanted);
        }
        let bucket_count = self.buckets.len();
        let chash = constrain(hash, bucket_count);
        let before = self.buckets[chash];
        if before == NIL {
            self.next[node] = self.next[ANCHOR];
            self.next[ANCHOR] = node;
            self.buckets[chash] = ANCHOR;
            let following = self.next[node];
            if following != NIL {
                self.buckets[constrain(self.hashes[following], bucket_count)] = node;
            }
        } else {
            self.next[node] = self.next[before];
            self.next[before] = node;
        }
        self.size += 1;
        Ok(node)
    }

    /// `__rehash`: grows the bucket array to (the next prime above) `n`.
    fn rehash(&mut self, n: usize) {
        let n = if n == 1 {
            2
        } else if n.is_power_of_two() {
            n
        } else {
            next_prime(n)
        };
        if n > self.buckets.len() {
            self.do_rehash(n);
        }
    }

    /// `__do_rehash`: redistributes the node list over `bucket_count` buckets, moving
    /// every node behind the first node already seen for its bucket.
    fn do_rehash(&mut self, bucket_count: usize) {
        self.buckets = vec![NIL; bucket_count];
        let mut pp = ANCHOR;
        let mut cp = self.next[pp];
        if cp == NIL {
            return;
        }
        let mut phash = constrain(self.hashes[cp], bucket_count);
        self.buckets[phash] = pp;
        pp = cp;
        cp = self.next[cp];
        while cp != NIL {
            let chash = constrain(self.hashes[cp], bucket_count);
            if chash == phash {
                pp = cp;
            } else if self.buckets[chash] == NIL {
                self.buckets[chash] = pp;
                pp = cp;
                phash = chash;
            } else {
                self.next[pp] = self.next[cp];
                let head = self.buckets[chash];
                self.next[cp] = self.next[head];
                self.next[head] = cp;
            }
            cp = self.next[pp];
        }
    }
}

pub(crate) use roastedduck_common::libcxx::ci_hash;

/// A `case_insensitive_map_t` or `case_insensitive_set_t` whose iteration order follows
/// libc++, built by unique insertions.
#[derive(Clone, Debug)]
pub struct CiHashMap<V, K = String> {
    order: HashOrder,
    entries: Vec<(K, V)>,
}

impl<V> CiHashMap<V> {
    pub fn new() -> Self {
        Self::new_with_keys()
    }
}

impl<V, K> CiHashMap<V, K> {
    pub fn new_with_keys() -> Self {
        Self {
            order: HashOrder::new(),
            entries: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<V, K: AsRef<[u8]>> CiHashMap<V, K> {
    fn position(&self, key: impl AsRef<[u8]>) -> Option<usize> {
        let entries = &self.entries;
        self.order
            .find(ci_hash(key.as_ref()), |node| {
                entries[node - 1].0.as_ref().eq_ignore_ascii_case(key.as_ref())
            })
            .map(|node| node - 1)
    }

    pub fn get(&self, key: impl AsRef<[u8]>) -> Option<&V> {
        self.position(key).map(|index| &self.entries[index].1)
    }

    pub fn contains_key(&self, key: impl AsRef<[u8]>) -> bool {
        self.position(key).is_some()
    }

    /// `map[key] = value`: inserts, or overwrites the value of an existing key.
    pub fn set(&mut self, key: impl Into<K>, value: V) {
        let key = key.into();
        let entries = &self.entries;
        match self.order.insert_unique(ci_hash(&key), |node| {
            entries[node - 1].0.as_ref().eq_ignore_ascii_case(key.as_ref())
        }) {
            Ok(_) => self.entries.push((key, value)),
            Err(node) => self.entries[node - 1].1 = value,
        }
    }

    /// `map.insert({key, value})`: inserts unless the key exists; returns whether it did.
    pub fn insert(&mut self, key: impl Into<K>, value: V) -> bool {
        let key = key.into();
        if self.contains_key(&key) {
            return false;
        }
        self.set(key, value);
        true
    }

    /// `clear()`: keeps the buckets.
    pub fn clear(&mut self) {
        self.order.clear();
        self.entries.clear();
    }

    /// The entries in iteration order.
    pub fn iter_keys(&self) -> impl Iterator<Item = (&K, &V)> {
        self.order.order().into_iter().map(|node| {
            let (key, value) = &self.entries[node - 1];
            (key, value)
        })
    }

    /// Consumes the map, yielding the entries in iteration order.
    pub fn into_ordered(self) -> Vec<(K, V)> {
        let mut slots: Vec<Option<(K, V)>> = self.entries.into_iter().map(Some).collect();
        self.order
            .order()
            .into_iter()
            .filter_map(|node| slots[node - 1].take())
            .collect()
    }
}

impl<V, K> Default for CiHashMap<V, K> {
    fn default() -> Self {
        Self::new_with_keys()
    }
}
impl<V> CiHashMap<V> {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.iter_keys().map(|(key, value)| (key.as_str(), value))
    }
}
impl<V> CiHashMap<V, roastedduck_common::ByteString> {
    pub fn iter(&self) -> impl Iterator<Item = (&roastedduck_common::ByteString, &V)> {
        self.iter_keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names<V>(map: &CiHashMap<V>) -> Vec<String> {
        map.iter().map(|(key, _)| key.to_owned()).collect()
    }

    #[test]
    fn primes_and_hashes() {
        assert_eq!(next_prime(5), 5);
        assert_eq!(next_prime(11), 11);
        assert_eq!(next_prime(24), 29);
        assert_eq!(ci_hash("A"), ci_hash("a"));
    }

    /// Iteration orders produced by libc++ (generated by a small C++ program): per case
    /// the keys, the order after unique insertions, after copying the map into a fresh
    /// one, after copying it again into that copy, and after `clear()` and inserting `zz`
    /// and `a1`.
    #[test]
    fn matches_libcxx() {
        let fixture = include_str!("hash_order_fixture.txt");
        let lines: Vec<&str> = fixture.lines().collect();
        assert_eq!(lines.len() % 5, 0);
        for case in lines.chunks(5) {
            let field = |line: &str, label: &str| -> Vec<String> {
                let rest = line
                    .strip_prefix(label)
                    .unwrap_or_else(|| panic!("expected {label}: {line}"));
                rest.split_whitespace().map(str::to_owned).collect()
            };
            let keys = field(case[0], "case");
            let mut map = CiHashMap::new();
            for key in &keys {
                map.set(key.clone(), ());
            }
            let unique = field(case[1], "unique");
            assert_eq!(names(&map), unique, "{}", case[0]);
            // Copies keep the order.
            assert_eq!(field(case[2], "copy"), unique, "{}", case[0]);
            assert_eq!(field(case[3], "copy2"), unique, "{}", case[0]);
            map.clear();
            map.set("zz".to_owned(), ());
            map.set("a1".to_owned(), ());
            assert_eq!(names(&map), field(case[4], "cleared"), "{}", case[0]);
        }
    }
}
