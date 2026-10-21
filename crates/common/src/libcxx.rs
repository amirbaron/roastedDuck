//! The iteration order of libc++'s `std::unordered_map`/`unordered_set` with unique keys,
//! and libc++'s `std::hash<std::string>`.
//!
//! DuckDB's reference build uses libc++, and some results (`list_distinct`, `entropy`'s
//! summation order, the field order of a shredded VARIANT, ...) come out in hash-table
//! order. This emulates libc++'s `__hash_table`: a singly linked node list, buckets
//! pointing at the node *before* their first node, prime (or power-of-two) bucket counts,
//! a maximum load factor of 1 and the same rehash policy, so that iteration visits keys in
//! exactly the same order.

/// What a bucket points at: the list head (`__p1_`) or a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Before {
    Head,
    Node(usize),
}

/// A hash table recording only key identity and order.
#[derive(Debug)]
pub struct UnorderedOrder<K> {
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
    pub fn insert(&mut self, hash: u64, key: K, eq: impl Fn(&K, &K) -> bool) -> (bool, usize) {
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
    pub fn order(&self) -> Vec<usize> {
        let mut order = Vec::with_capacity(self.keys.len());
        let mut node = self.head;
        while let Some(index) = node {
            order.push(index);
            node = self.next[index];
        }
        order
    }

    /// The keys in insertion order.
    pub fn keys(&self) -> &[K] {
        &self.keys
    }
}

/// libc++'s `std::hash<std::string>` on 64-bit platforms
/// (`__murmur2_or_cityhash<size_t, 64>`, i.e. `CityHash64`, without libc++'s ABI fix of the 4 to
/// 8 byte case). The names follow libc++'s.
#[allow(clippy::many_single_char_names)]
pub fn string_hash(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    if len <= 16 {
        return hash_len_0_to_16(bytes);
    }
    if len <= 32 {
        return hash_len_17_to_32(bytes);
    }
    if len <= 64 {
        return hash_len_33_to_64(bytes);
    }
    let len64 = len as u64;
    // Hash the end first, then keep 56 bytes of state while looping over 64-byte chunks.
    let mut x = load64(bytes, len - 40);
    let mut y = load64(bytes, len - 16).wrapping_add(load64(bytes, len - 56));
    let mut z = hash_len_16(
        load64(bytes, len - 48).wrapping_add(len64),
        load64(bytes, len - 24),
    );
    let mut v = weak_hash_len_32_with_seeds(bytes, len - 64, len64, z);
    let mut w = weak_hash_len_32_with_seeds(bytes, len - 32, y.wrapping_add(K1), x);
    x = x.wrapping_mul(K1).wrapping_add(load64(bytes, 0));
    let mut remaining = (len - 1) & !63;
    let mut offset = 0;
    loop {
        x = rotate(
            x.wrapping_add(y)
                .wrapping_add(v.0)
                .wrapping_add(load64(bytes, offset + 8)),
            37,
        )
        .wrapping_mul(K1);
        y = rotate(y.wrapping_add(v.1).wrapping_add(load64(bytes, offset + 48)), 42).wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(load64(bytes, offset + 40));
        z = rotate(z.wrapping_add(w.0), 33).wrapping_mul(K1);
        v = weak_hash_len_32_with_seeds(bytes, offset, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak_hash_len_32_with_seeds(
            bytes,
            offset + 32,
            z.wrapping_add(w.1),
            y.wrapping_add(load64(bytes, offset + 16)),
        );
        std::mem::swap(&mut z, &mut x);
        offset += 64;
        remaining -= 64;
        if remaining == 0 {
            break;
        }
    }
    hash_len_16(
        hash_len_16(v.0, w.0)
            .wrapping_add(shift_mix(y).wrapping_mul(K1))
            .wrapping_add(z),
        hash_len_16(v.1, w.1).wrapping_add(x),
    )
}

const K0: u64 = 0xc3a5_c85c_97cb_3127;
const K1: u64 = 0xb492_b66f_be98_f273;
const K2: u64 = 0x9ae1_6a3b_2f90_404f;
const K3: u64 = 0xc949_d7c7_509e_6557;

/// `__loadword<size_t>`: the little-endian 8 bytes at `offset`.
fn load64(bytes: &[u8], offset: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(word)
}

/// `__loadword<uint32_t>`.
fn load32(bytes: &[u8], offset: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(word)
}

/// `__rotate`: a right rotation, the identity for 0.
fn rotate(value: u64, shift: u32) -> u64 {
    value.rotate_right(shift)
}

/// `__shift_mix`.
fn shift_mix(value: u64) -> u64 {
    value ^ (value >> 47)
}

/// `__hash_len_16`.
fn hash_len_16(u: u64, v: u64) -> u64 {
    const MUL: u64 = 0x9ddf_ea08_eb38_2d69;
    let mut a = (u ^ v).wrapping_mul(MUL);
    a ^= a >> 47;
    let mut b = (v ^ a).wrapping_mul(MUL);
    b ^= b >> 47;
    b.wrapping_mul(MUL)
}

/// `__hash_len_0_to_16`.
#[allow(clippy::many_single_char_names)]
fn hash_len_0_to_16(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let len64 = len as u64;
    if len > 8 {
        let a = load64(bytes, 0);
        let b = load64(bytes, len - 8);
        return hash_len_16(a, rotate(b.wrapping_add(len64), len as u32)) ^ b;
    }
    if len >= 4 {
        let a = load32(bytes, 0);
        let b = load32(bytes, len - 4);
        // `__a << 3` is a 32-bit shift that drops the high bits.
        return hash_len_16(len64.wrapping_add(u64::from(a << 3)), u64::from(b));
    }
    if len > 0 {
        let a = u32::from(bytes[0]);
        let b = u32::from(bytes[len >> 1]);
        let c = u32::from(bytes[len - 1]);
        let y = a + (b << 8);
        let z = len as u32 + (c << 2);
        return shift_mix(u64::from(y).wrapping_mul(K2) ^ u64::from(z).wrapping_mul(K3)).wrapping_mul(K2);
    }
    K2
}

/// `__hash_len_17_to_32`.
fn hash_len_17_to_32(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let a = load64(bytes, 0).wrapping_mul(K1);
    let b = load64(bytes, 8);
    let c = load64(bytes, len - 8).wrapping_mul(K2);
    let d = load64(bytes, len - 16).wrapping_mul(K0);
    hash_len_16(
        rotate(a.wrapping_sub(b), 43)
            .wrapping_add(rotate(c, 30))
            .wrapping_add(d),
        a.wrapping_add(rotate(b ^ K3, 20))
            .wrapping_sub(c)
            .wrapping_add(len as u64),
    )
}

/// `__weak_hash_len_32_with_seeds` of the 32 bytes at `offset`.
#[allow(clippy::many_single_char_names)]
fn weak_hash_len_32_with_seeds(bytes: &[u8], offset: usize, a: u64, b: u64) -> (u64, u64) {
    let (w, x, y, z) = (
        load64(bytes, offset),
        load64(bytes, offset + 8),
        load64(bytes, offset + 16),
        load64(bytes, offset + 24),
    );
    let mut a = a.wrapping_add(w);
    let mut b = rotate(b.wrapping_add(a).wrapping_add(z), 21);
    let c = a;
    a = a.wrapping_add(x).wrapping_add(y);
    b = b.wrapping_add(rotate(a, 44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

/// `__hash_len_33_to_64`.
#[allow(clippy::many_single_char_names)]
fn hash_len_33_to_64(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut z = load64(bytes, 24);
    let mut a = load64(bytes, 0).wrapping_add(
        (len as u64)
            .wrapping_add(load64(bytes, len - 16))
            .wrapping_mul(K0),
    );
    let mut b = rotate(a.wrapping_add(z), 52);
    let mut c = rotate(a, 37);
    a = a.wrapping_add(load64(bytes, 8));
    c = c.wrapping_add(rotate(a, 7));
    a = a.wrapping_add(load64(bytes, 16));
    let vf = a.wrapping_add(z);
    let vs = b.wrapping_add(rotate(a, 31)).wrapping_add(c);
    a = load64(bytes, 16).wrapping_add(load64(bytes, len - 32));
    z = z.wrapping_add(load64(bytes, len - 8));
    b = rotate(a.wrapping_add(z), 52);
    c = rotate(a, 37);
    a = a.wrapping_add(load64(bytes, len - 24));
    c = c.wrapping_add(rotate(a, 7));
    a = a.wrapping_add(load64(bytes, len - 16));
    let wf = a.wrapping_add(z);
    let ws = b.wrapping_add(rotate(a, 31)).wrapping_add(c);
    let r = shift_mix(
        vf.wrapping_add(ws)
            .wrapping_mul(K2)
            .wrapping_add(vs.wrapping_add(wf).wrapping_mul(K0)),
    );
    shift_mix(r.wrapping_mul(K0).wrapping_add(vs)).wrapping_mul(K2)
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

    #[test]
    fn string_hash_matches_libcxx() {
        // `std::hash<std::string>` of libc++ (Apple clang, arm64).
        let long = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()\
                    abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#$%^&*()xyz";
        let cases: [(&[u8], u64); 19] = [
            (b"", 0x9ae1_6a3b_2f90_404f),
            (b"a", 0x2420_662c_d003_acfa),
            (b"ab", 0x17f0_a30f_f3cc_3f3e),
            (b"abc", 0x3a91_2f48_3a4e_ce31),
            (b"abcd", 0x7c9d_3ea7_7898_749f),
            (b"abcde", 0xb491_e138_a905_341a),
            (b"abcdefgh", 0x87c6_9099_911b_ab7e),
            (b"abcdefghi", 0x66c7_fa4e_b3fa_b8d1),
            (b"abcdefghijklmnop", 0xb7c6_0922_8035_52c1),
            (b"abcdefghijklmnopq", 0x034b_b4fe_133a_33e6),
            (b"abcdefghijklmnopqrstuvwxyz012345", 0x3eca_4491_8b3b_7a1a),
            (b"abcdefghijklmnopqrstuvwxyz0123456", 0xa0af_67b2_5a0a_a17d),
            (
                b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ!@",
                0x6347_497c_0d7b_bc74,
            ),
            (
                b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ!@#",
                0x3996_85ea_ab6b_4f91,
            ),
            (long.as_bytes(), 0x2c15_b265_6778_ad7a),
            (b"\xff\xfe\x80", 0x0184_bfa0_7ef5_d8df),
            (b"almost_a_number", 0x5b0f_044b_7630_3222),
            (b"a_number", 0x738f_d6f1_8b33_643a),
            (b"\xff\xff\xff\xff\xff", 0x7bf9_04e3_ef30_c76b),
        ];
        for (bytes, expected) in cases {
            assert_eq!(string_hash(bytes), expected, "{bytes:?}");
        }
    }

    #[test]
    fn string_keys_iterate_like_libcxx() {
        // std::unordered_map<std::string, int> after emplacing "a" to "z" in order.
        let mut table = UnorderedOrder::default();
        for key in b'a'..=b'z' {
            table.insert(string_hash(&[key]), key, |a, b| a == b);
        }
        let order: String = table
            .order()
            .into_iter()
            .map(|index| char::from(table.keys()[index]))
            .collect();
        assert_eq!(order, "zxiwynqtrlvpkohmgsfcdejuba");
    }
}
