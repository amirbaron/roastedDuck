//! Split-block bloom filters (DuckDB's `ParquetBloomFilter` and `ParquetBloomBlock`).

/// The salts of the eight 32-bit words of a block.
const SALT: [u32; 8] = [
    0x47b6_137b,
    0x4497_4d91,
    0x8824_ad5b,
    0xa2b7_289d,
    0x7054_95c7,
    0x2df1_424b,
    0x9efc_4947,
    0x5c6b_fb31,
];

/// The bytes of one block (eight `u32` words).
const BLOCK_BYTES: usize = 32;

/// A bloom filter over xxHash64 values.
#[derive(Clone, Debug)]
pub struct BloomFilter {
    /// Little-endian `u32` words, eight per block.
    words: Vec<u32>,
}

impl BloomFilter {
    /// A filter sized for `num_entries` values at a false positive ratio.
    pub fn new(num_entries: usize, false_positive_ratio: f64) -> Self {
        let k = 8.0f64;
        let n = num_entries as f64;
        let m = -k * n / (1.0 - false_positive_ratio.powf(1.0 / k)).ln();
        let bits = (m / k) as u64;
        let blocks = (bits.next_power_of_two() / 32).max(1) as usize;
        Self {
            words: vec![0; blocks * 8],
        }
    }

    /// A filter read from a file.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let (chunks, _) = bytes.as_chunks::<4>();
        let words = chunks.iter().map(|chunk| u32::from_le_bytes(*chunk)).collect();
        Self { words }
    }

    fn block_count(&self) -> usize {
        self.words.len() / 8
    }

    fn block_index(&self, hash: u64) -> usize {
        (((hash >> 32) * self.block_count() as u64) >> 32) as usize
    }

    /// `FilterInsert`.
    pub fn insert(&mut self, hash: u64) {
        if self.block_count() == 0 {
            return;
        }
        let block = self.block_index(hash) * 8;
        let key = hash as u32;
        for (i, salt) in SALT.iter().enumerate() {
            self.words[block + i] |= 1 << (key.wrapping_mul(*salt) >> 27);
        }
    }

    /// `FilterCheck`: whether the value may be present.
    pub fn check(&self, hash: u64) -> bool {
        if self.block_count() == 0 {
            return true;
        }
        let block = self.block_index(hash) * 8;
        let key = hash as u32;
        SALT.iter()
            .enumerate()
            .all(|(i, salt)| self.words[block + i] >> (key.wrapping_mul(*salt) >> 27) & 1 == 1)
    }

    /// The serialized filter.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    pub fn byte_len(&self) -> usize {
        self.words.len() / 8 * BLOCK_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_values_are_found() {
        let mut filter = BloomFilter::new(100, 0.01);
        for value in 0..100u64 {
            filter.insert(crate::writer::types::xxhash64(&value.to_le_bytes()));
        }
        for value in 0..100u64 {
            assert!(filter.check(crate::writer::types::xxhash64(&value.to_le_bytes())));
        }
        let round_trip = BloomFilter::from_bytes(&filter.to_bytes());
        assert_eq!(round_trip.words, filter.words);
    }
}
