//! The bloom filter a hash join builds over its keys and pushes into its probe-side
//! scans (DuckDB's `BloomFilter` and `BFTableFilter`).

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rustdb_common::Result;
use rustdb_row::hash::{hash_fixed, hash_selected, hash_vector};
use rustdb_storage::statistics::{BaseStatistics, FilterPropagateResult, NumericValue, StatsData};
use rustdb_storage::table_filter::ExpressionFilter;
use rustdb_types::{LogicalType, Vector, VectorKind};

/// At most this many 64-bit sectors (`MAX_NUM_SECTORS`).
const MAX_NUM_SECTORS: u64 = 1 << 26;
/// Bits per key (`MIN_NUM_BITS_PER_KEY`) and the minimum size (`MIN_NUM_BITS`).
const MIN_NUM_BITS_PER_KEY: u64 = 12;
const MIN_NUM_BITS: u64 = 512;
/// A sector is 64 bits (`LOG_SECTOR_SIZE`).
const LOG_SECTOR_SIZE: u32 = 6;
/// Six bits per byte select the bit positions (`SHIFT_MASK`).
const SHIFT_MASK: u64 = 0x3F3F_3F3F_3F3F_3F3F;
/// Bits set per key (`N_BITS`).
const N_BITS: u32 = 4;

/// A register-blocked bloom filter of 64-bit sectors (DuckDB's `BloomFilter`): a hash
/// selects a sector by its low bits and four bits in it by its high bytes.
#[derive(Debug)]
pub struct BloomFilter {
    /// A power-of-two number of sectors.
    sectors: Box<[AtomicU64]>,
}

impl BloomFilter {
    /// A filter sized for `rows` keys (`BloomFilter::Initialize`).
    pub fn new(rows: u64) -> Self {
        let min_bits = MIN_NUM_BITS.max(rows.saturating_mul(MIN_NUM_BITS_PER_KEY));
        let num_sectors = (min_bits.next_power_of_two() >> LOG_SECTOR_SIZE).min(MAX_NUM_SECTORS);
        Self {
            sectors: (0..num_sectors).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// The bits of `hash` within its sector (`GetMask`): the low six bits of each of the
    /// hash's four high bytes.
    #[inline]
    const fn mask(hash: u64) -> u64 {
        let shifts = hash & SHIFT_MASK;
        let mut mask = 0;
        let mut byte = 8 - N_BITS;
        while byte < 8 {
            mask |= 1 << ((shifts >> (8 * byte)) & 0xFF);
            byte += 1;
        }
        mask
    }

    #[inline]
    fn sector(&self, hash: u64) -> &AtomicU64 {
        // The sector count is a power of two: masking with it minus one keeps the index
        // in bounds without a check.
        &self.sectors[hash as usize & (self.sectors.len() - 1)]
    }

    /// Adds a key hash (`InsertOne`).
    #[inline]
    pub fn insert(&self, hash: u64) {
        self.sector(hash).fetch_or(Self::mask(hash), Ordering::Relaxed);
    }

    /// Moves the rows of `rows` whose hash (`hashes[i]` for `rows[i]`) may have been
    /// inserted to its front, keeping their order, and returns how many there are
    /// (`LookupHashes` with the selection compacted).
    fn retain_found(&self, rows: &mut [u32], hashes: &[u64]) -> usize {
        // The sectors in locals: through `self`, the atomics would make every iteration
        // load them again.
        let sectors = &self.sectors[..];
        let mask = sectors.len() - 1;
        let mut kept = 0;
        for index in 0..rows.len().min(hashes.len()) {
            let hash = hashes[index];
            let bits = Self::mask(hash);
            rows[kept] = rows[index];
            kept += usize::from(sectors[hash as usize & mask].load(Ordering::Relaxed) & bits == bits);
        }
        kept
    }

    /// Whether a key with this hash may have been inserted (`LookupOne`).
    #[inline]
    pub fn lookup(&self, hash: u64) -> bool {
        let mask = Self::mask(hash);
        self.sector(hash).load(Ordering::Relaxed) & mask == mask
    }
}

thread_local! {
    /// The key hashes of the vector a scan thread is filtering (`BFTableFilterState::hashes_v`).
    static HASHES: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The probe-side filter `column IN BF(key)` (DuckDB's `BFTableFilter`): rows whose hash
/// the bloom filter does not contain cannot match.
#[derive(Debug)]
pub struct BloomTableFilter {
    filter: Arc<BloomFilter>,
    /// The build key, as `EXPLAIN` shows it.
    key_column_name: String,
}

impl BloomTableFilter {
    pub fn new(filter: Arc<BloomFilter>, key_column_name: String) -> Self {
        Self {
            filter,
            key_column_name,
        }
    }
}

/// DuckDB's `TemplatedCheckStatistics`: probes every value of an integer range of at
/// most 2049 values.
fn check_integer_range(filter: &BloomFilter, min: NumericValue, max: NumericValue) -> FilterPropagateResult {
    macro_rules! range {
        ($min:expr, $max:expr, $physical:expr) => {{
            let (min, max) = ($min, $max);
            if min > max {
                return FilterPropagateResult::NoPruningPossible;
            }
            // The difference must not overflow the type (`TrySubtractOperator`).
            match max.checked_sub(min) {
                Some(range) if u128::try_from(range).is_ok_and(|range| range <= 2048) => {}
                _ => return FilterPropagateResult::NoPruningPossible,
            }
            let mut hits = 0u64;
            let mut total = 0u64;
            let mut value = min;
            loop {
                hits += u64::from(filter.lookup(hash_fixed($physical, &value.to_ne_bytes())));
                total += 1;
                if value == max {
                    break;
                }
                value += 1;
            }
            if hits == 0 {
                FilterPropagateResult::FilterAlwaysFalse
            } else if hits == total {
                FilterPropagateResult::FilterAlwaysTrue
            } else {
                FilterPropagateResult::NoPruningPossible
            }
        }};
    }
    use rustdb_types::PhysicalType as P;
    match (min, max) {
        (NumericValue::I8(min), NumericValue::I8(max)) => range!(min, max, P::Int8),
        (NumericValue::I16(min), NumericValue::I16(max)) => range!(min, max, P::Int16),
        (NumericValue::I32(min), NumericValue::I32(max)) => range!(min, max, P::Int32),
        (NumericValue::I64(min), NumericValue::I64(max)) => range!(min, max, P::Int64),
        (NumericValue::I128(min), NumericValue::I128(max)) => range!(min, max, P::Int128),
        (NumericValue::U8(min), NumericValue::U8(max)) => range!(min, max, P::UInt8),
        (NumericValue::U16(min), NumericValue::U16(max)) => range!(min, max, P::UInt16),
        (NumericValue::U32(min), NumericValue::U32(max)) => range!(min, max, P::UInt32),
        (NumericValue::U64(min), NumericValue::U64(max)) => range!(min, max, P::UInt64),
        (NumericValue::U128(min), NumericValue::U128(max)) => range!(min, max, P::UInt128),
        _ => FilterPropagateResult::NoPruningPossible,
    }
}

impl ExpressionFilter for BloomTableFilter {
    /// DuckDB's `BFTableFilter::Filter`: hashes the keys and keeps the rows found.
    fn select(&self, vector: &Vector, count: usize, sel: &mut Vec<u32>) -> Result<()> {
        if vector.kind() == VectorKind::Constant {
            let mut hash = Vector::flat_for_overwrite(LogicalType::UBIGINT, 1);
            hash_vector(vector, count, &mut hash)?;
            if !self.filter.lookup(hash.unified::<u64>().values[0]) {
                sel.clear();
            }
            return Ok(());
        }
        // Only the rows still selected are hashed (`HashInternal` of the sliced keys).
        HASHES.with_borrow_mut(|hashes| {
            hashes.resize(sel.len().max(hashes.len()), 0);
            let hashes = &mut hashes[..sel.len()];
            hash_selected(vector, sel, hashes)?;
            // Branchless compaction (`LookupHashes` writes every index, counts the found).
            let kept = self.filter.retain_found(sel, hashes);
            sel.truncate(kept);
            Ok(())
        })
    }

    fn check_statistics(&self, stats: &BaseStatistics) -> FilterPropagateResult {
        match stats.data() {
            StatsData::Numeric(numeric) if numeric.has_min_max() => match (numeric.min, numeric.max) {
                (Some(min), Some(max)) => check_integer_range(&self.filter, min, max),
                _ => FilterPropagateResult::NoPruningPossible,
            },
            _ => FilterPropagateResult::NoPruningPossible,
        }
    }

    fn to_sql(&self, column_name: &str) -> String {
        format!("{column_name} IN BF({})", self.key_column_name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
