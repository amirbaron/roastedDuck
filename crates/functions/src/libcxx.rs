//! The iteration order of libc++'s `std::unordered_map`/`unordered_set` with unique keys.
//!
//! DuckDB's reference build uses libc++, and some results (`list_distinct`, `entropy`'s
//! summation order, ...) come out in hash-table order. This emulates libc++'s
//! `__hash_table`: a singly linked node list, buckets pointing at the node *before* their
//! first node, prime (or power-of-two) bucket counts, a maximum load factor of 1 and the
//! same rehash policy, so that iteration visits keys in exactly the same order.

/// What a bucket points at: the list head (`__p1_`) or a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Before {
    Head,
    Node(usize),
}

/// A hash table recording only key identity and order.
#[derive(Debug)]
pub(crate) struct UnorderedOrder<K> {
    keys: Vec<K>,
    hashes: Vec<u64>,
    next: Vec<Option<usize>>,
    head: Option<usize>,
    buckets: Vec<Option<Before>>,
}

impl<K> Default for UnorderedOrder<K> {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            hashes: Vec::new(),
            next: Vec::new(),
            head: None,
            buckets: Vec::new(),
        }
    }
}

/// libc++'s `__constrain_hash`.
fn constrain(hash: u64, bucket_count: usize) -> usize {
    let count = bucket_count as u64;
    if count.is_power_of_two() {
        (hash & (count - 1)) as usize
    } else if hash < count {
        hash as usize
    } else {
        (hash % count) as usize
    }
}

/// libc++'s `__is_hash_power2`.
fn is_hash_power2(count: usize) -> bool {
    count > 2 && count.is_power_of_two()
}

/// libc++'s `__next_prime`: the smallest prime at least `n`.
fn next_prime(n: usize) -> usize {
    if n <= 2 {
        return 2;
    }
    let mut candidate = n | 1;
    loop {
        let mut divisor = 3;
        let mut prime = true;
        while divisor * divisor <= candidate {
            if candidate.is_multiple_of(divisor) {
                prime = false;
                break;
            }
            divisor += 2;
        }
        if prime {
            return candidate;
        }
        candidate += 2;
    }
}

impl<K> UnorderedOrder<K> {
    fn link_next(&self, before: Before) -> Option<usize> {
        match before {
            Before::Head => self.head,
            Before::Node(node) => self.next[node],
        }
    }

    fn set_next(&mut self, before: Before, value: Option<usize>) {
        match before {
            Before::Head => self.head = value,
            Before::Node(node) => self.next[node] = value,
        }
    }

    /// The index of the key equal to `key` with hash `hash`, if present.
    fn find(&self, hash: u64, key: &K, eq: &impl Fn(&K, &K) -> bool) -> Option<usize> {
        let count = self.buckets.len();
        if count == 0 {
            return None;
        }
        let bucket = constrain(hash, count);
        let before = self.buckets[bucket]?;
        let mut node = self.link_next(before);
        while let Some(index) = node {
            let node_hash = self.hashes[index];
            if node_hash != hash && constrain(node_hash, count) != bucket {
                break;
            }
            if node_hash == hash && eq(&self.keys[index], key) {
                return Some(index);
            }
            node = self.next[index];
        }
        None
    }

    /// libc++'s `__do_rehash<true>`.
    fn rehash_to(&mut self, count: usize) {
        self.buckets = vec![None; count];
        let Some(first) = self.head else {
            return;
        };
        let mut previous = first;
        let mut previous_bucket = constrain(self.hashes[first], count);
        self.buckets[previous_bucket] = Some(Before::Head);
        let mut current = self.next[previous];
        while let Some(node) = current {
            let bucket = constrain(self.hashes[node], count);
            if bucket == previous_bucket {
                previous = node;
            } else if let Some(before) = self.buckets[bucket] {
                // Move the node to the front of its bucket's run.
                self.next[previous] = self.next[node];
                self.next[node] = self.link_next(before);
                self.set_next(before, Some(node));
            } else {
                self.buckets[bucket] = Some(Before::Node(previous));
                previous = node;
                previous_bucket = bucket;
            }
            current = self.next[previous];
        }
    }

    /// libc++'s `__rehash_unique`.
    fn rehash(&mut self, requested: usize) {
        let count = if requested == 1 {
            2
        } else if requested & (requested - 1) != 0 {
            next_prime(requested)
        } else {
            requested
        };
        if count > self.buckets.len() {
            self.rehash_to(count);
        }
    }

    /// Inserts `key` unless an equal key is present (`emplace`); returns whether it was
    /// inserted and the key's index in insertion order.
    pub(crate) fn insert(&mut self, hash: u64, key: K, eq: impl Fn(&K, &K) -> bool) -> (bool, usize) {
        if let Some(existing) = self.find(hash, &key, &eq) {
            return (false, existing);
        }
        let count = self.buckets.len();
        let size = self.keys.len();
        if size + 1 > count || count == 0 {
            let grow = 2 * count + usize::from(!is_hash_power2(count));
            self.rehash(grow.max(size + 1));
        }
        let index = self.keys.len();
        self.keys.push(key);
        self.hashes.push(hash);
        self.next.push(None);
        let count = self.buckets.len();
        let bucket = constrain(hash, count);
        match self.buckets[bucket] {
            None => {
                self.next[index] = self.head;
                self.head = Some(index);
                self.buckets[bucket] = Some(Before::Head);
                if let Some(following) = self.next[index] {
                    let following_bucket = constrain(self.hashes[following], count);
                    self.buckets[following_bucket] = Some(Before::Node(index));
                }
            }
            Some(before) => {
                self.next[index] = self.link_next(before);
                self.set_next(before, Some(index));
            }
        }
        (true, index)
    }

    /// The keys' insertion indices in iteration order.
    pub(crate) fn order(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.keys.len());
        let mut node = self.head;
        while let Some(index) = node {
            order.push(index);
            node = self.next[index];
        }
        order
    }

    /// The keys in insertion order.
    pub(crate) fn keys(&self) -> &[K] {
        &self.keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order_of(values: &[u64]) -> Vec<u64> {
        let mut table = UnorderedOrder::default();
        for &value in values {
            table.insert(value, value, |a, b| a == b);
        }
        table
            .order()
            .into_iter()
            .map(|index| table.keys()[index])
            .collect()
    }

    #[test]
    fn matches_libcxx_iteration() {
        // Verified against libc++: std::unordered_set<int64_t> iteration after inserting
        // these values in order.
        assert_eq!(order_of(&[1, 2, 3]), vec![3, 2, 1]);
        assert_eq!(order_of(&[3, 1, 3, 2]), vec![2, 1, 3]);
        assert_eq!(order_of(&[]), Vec::<u64>::new());
    }
}
