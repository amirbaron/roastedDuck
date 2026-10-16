//! Approximate distinct counts of table columns (DuckDB's `DistinctStatistics` with its
//! `HyperLogLog`): a 64-register `HyperLogLog` over a sample of every appended vector,
//! extrapolated to the whole column with a Good-Turing estimate. The optimizer's
//! cardinality estimates use the count.

use rustdb_common::Result;
use rustdb_row::hash::hash_vector;
use rustdb_types::{LogicalType, PhysicalType, STANDARD_VECTOR_SIZE, Vector, VectorKind};

/// Bits of the hash that select a register (`HyperLogLog::P`).
const P: u32 = 6;
/// The remaining hash bits (`Q`).
const Q: usize = 64 - P as usize;
/// The number of registers (`M`).
pub const HLL_REGISTERS: usize = 1 << P;
/// `1 / (2 log(2))` (`ALPHA`).
const ALPHA: f64 = 0.721_347_520_444_481_7;

/// The share of each vector of integral columns that is sampled
/// (`INTEGRAL_SAMPLE_RATE`), and of other columns (`BASE_SAMPLE_RATE`).
const INTEGRAL_SAMPLE_RATE: f64 = 0.3;
const BASE_SAMPLE_RATE: f64 = 0.1;

/// A `HyperLogLog` sketch with 64 registers (DuckDB's `HyperLogLog`, after Ertl's "New
/// cardinality estimation algorithms for `HyperLogLog` sketches").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HyperLogLog {
    k: [u8; HLL_REGISTERS],
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self {
            k: [0; HLL_REGISTERS],
        }
    }
}

impl HyperLogLog {
    /// A sketch with the given registers.
    pub const fn from_registers(k: [u8; HLL_REGISTERS]) -> Self {
        Self { k }
    }

    pub const fn registers(&self) -> &[u8; HLL_REGISTERS] {
        &self.k
    }

    /// Adds a hash (Algorithm 1, `InsertElement`).
    #[inline]
    pub fn insert_element(&mut self, hash: u64) {
        let index = (hash & ((1 << P) - 1)) as usize;
        let rest = (hash >> P) | (1u64 << Q);
        #[allow(clippy::cast_possible_truncation, reason = "at most 59")]
        let zeros = (rest.trailing_zeros() + 1) as u8;
        self.update(index, zeros);
    }

    /// Raises register `index` to `zeros` (`Update`).
    #[inline]
    pub fn update(&mut self, index: usize, zeros: u8) {
        self.k[index] = self.k[index].max(zeros);
    }

    /// Merges another sketch (Algorithm 2).
    pub fn merge(&mut self, other: &Self) {
        for (index, &zeros) in other.k.iter().enumerate() {
            self.update(index, zeros);
        }
    }

    /// The estimated number of distinct hashes (`Count`).
    pub fn count(&self) -> u64 {
        let mut c = [0u32; Q + 2];
        for &register in &self.k {
            c[register as usize] += 1;
        }
        estimate_cardinality(&c)
    }

    /// Adds the hashes of the valid rows among the first `count` of `input` (`Update`).
    fn update_vector(&mut self, input: &Vector, hashes: &Vector, count: usize) {
        let hash_values = hashes.values::<u64>();
        if hashes.kind() == VectorKind::Constant {
            if !input.is_null(0) {
                self.insert_element(hash_values[0]);
            }
            return;
        }
        for (row, &hash) in hash_values.iter().enumerate().take(count) {
            if !input.is_null(row) {
                self.insert_element(hash);
            }
        }
    }
}

/// Redis' `hllSigma` (also used by DuckDB).
pub(crate) fn hll_sigma(mut x: f64) -> f64 {
    #[allow(clippy::float_cmp, reason = "the series is defined for x != 1")]
    if x == 1.0 {
        return f64::INFINITY;
    }
    let mut y = 1.0;
    let mut z = x;
    loop {
        x *= x;
        let z_prime = z;
        z += x * y;
        y += y;
        #[allow(clippy::float_cmp, reason = "iterates until the sum stops changing")]
        if z_prime == z {
            return z;
        }
    }
}

/// Redis' `hllTau` (also used by DuckDB).
pub(crate) fn hll_tau(mut x: f64) -> f64 {
    #[allow(clippy::float_cmp, reason = "the series is defined on (0, 1)")]
    if x == 0.0 || x == 1.0 {
        return 0.0;
    }
    let mut y = 1.0;
    let mut z = 1.0 - x;
    loop {
        x = x.sqrt();
        let z_prime = z;
        y *= 0.5;
        z -= (1.0 - x).powi(2) * y;
        #[allow(clippy::float_cmp, reason = "iterates until the sum stops changing")]
        if z_prime == z {
            return z / 3.0;
        }
    }
}

/// Algorithm 6 (`EstimateCardinality`) over the register histogram `c`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "llroundl of a non-negative estimate"
)]
fn estimate_cardinality(c: &[u32; Q + 2]) -> u64 {
    let m = HLL_REGISTERS as f64;
    let mut z = m * hll_tau((m - f64::from(c[Q])) / m);
    for k in (1..=Q).rev() {
        z += f64::from(c[k]);
        z *= 0.5;
    }
    z += m * hll_sigma(f64::from(c[0]) / m);
    (ALPHA * m * m / z).round() as u64
}

/// The distinct statistics of a column (DuckDB's `DistinctStatistics`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DistinctStatistics {
    pub log: HyperLogLog,
    /// Rows added to the sketch.
    pub sample_count: u64,
    /// Rows seen.
    pub total_count: u64,
}

impl DistinctStatistics {
    /// Whether distinct statistics are kept for columns of `ty` (`TypeIsSupported`): not
    /// for nested types, BOOLEAN and BIT.
    pub fn type_is_supported(ty: &LogicalType) -> bool {
        !matches!(
            ty.physical_type(),
            PhysicalType::List
                | PhysicalType::Struct
                | PhysicalType::Array
                | PhysicalType::Bit
                | PhysicalType::Bool
        )
    }

    pub fn merge(&mut self, other: &Self) {
        self.log.merge(&other.log);
        self.sample_count += other.sample_count;
        self.total_count += other.total_count;
    }

    /// Adds a sample of the first `count` rows of an appended vector: 30% of a standard
    /// vector for integers, 10% otherwise (`UpdateSample`).
    pub fn update_sample(&mut self, input: &Vector, count: usize) -> Result<()> {
        self.total_count += count as u64;
        let sample_rate = if input.logical_type().is_integral() {
            INTEGRAL_SAMPLE_RATE
        } else {
            BASE_SAMPLE_RATE
        };
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "LossyNumericCast"
        )]
        let sample = ((sample_rate * STANDARD_VECTOR_SIZE as f64) as usize)
            .max(1)
            .min(count);
        self.update_internal(input, sample)
    }

    /// Adds all `count` rows (`Update`).
    pub fn update(&mut self, input: &Vector, count: usize) -> Result<()> {
        self.total_count += count as u64;
        self.update_internal(input, count)
    }

    fn update_internal(&mut self, input: &Vector, count: usize) -> Result<()> {
        self.sample_count += count as u64;
        let mut hashes = Vector::flat(LogicalType::UBIGINT, count.max(1));
        hash_vector(input, count, &mut hashes)?;
        self.log.update_vector(input, &hashes, count);
        Ok(())
    }

    /// The estimated number of distinct values of the column (`GetCount`): the sample's
    /// distinct values plus a Good-Turing estimate of those in the unsampled rows.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "DuckDB's double estimate"
    )]
    pub fn count(&self) -> u64 {
        if self.sample_count == 0 || self.total_count == 0 {
            return 0;
        }
        let u = self.log.count().min(self.sample_count) as f64;
        let s = self.sample_count as f64;
        let n = self.total_count as f64;
        // Assume this proportion of the sampled values occurred only once.
        let u1 = (u / s).powi(2) * u;
        let estimate = (u + u1 / s * (n - s)) as u64;
        estimate.min(self.total_count)
    }
}

/// The storage formats of a sketch (DuckDB's `HLLStorageType`).
pub const HLL_V1: u8 = 1;
pub const HLL_V2: u8 = 2;

/// The Redis `HyperLogLog` DuckDB stored before (`HLLV1` over `duckdb_hll`): a dense
/// sketch of 4096 6-bit registers after a 16-byte header. DuckDB's copy addresses the
/// registers one byte after the header's end, so the serialized image (the header plus
/// 3072 register bytes) misses the last register byte.
struct RedisHll {
    bytes: Vec<u8>,
}

impl RedisHll {
    /// `HLL_P`, `HLL_REGISTERS` and `HLL_Q` of DuckDB's Redis copy.
    const P: u32 = 12;
    const REGISTERS: usize = 1 << Self::P;
    const Q: usize = 64 - Self::P as usize;
    const BITS: usize = 6;
    const REGISTER_MAX: u64 = (1 << Self::BITS) - 1;
    /// `HLL_HDR_SIZE` (`sizeof(struct hllhdr)`, which includes `registers[1]`) and
    /// `HLL_DENSE_SIZE` (the serialized size).
    const HEADER_SIZE: usize = 17;
    const DENSE_SIZE: usize = Self::HEADER_SIZE + (Self::REGISTERS * Self::BITS).div_ceil(8);
    /// Where the registers start (`hdr->registers + 1`).
    const REGISTER_OFFSET: usize = 17;

    /// An empty dense sketch (`hll_create` + `hllSparseToDense`).
    fn new() -> Self {
        // Room for the registers past the serialized image and the byte the register
        // accessors read after the last one.
        let mut bytes = vec![0u8; Self::DENSE_SIZE + 2];
        bytes[..4].copy_from_slice(b"HYLL");
        Self { bytes }
    }

    /// A sketch from its serialized image.
    fn from_image(image: &[u8]) -> Self {
        let mut hll = Self::new();
        let len = image.len().min(Self::DENSE_SIZE);
        hll.bytes[..len].copy_from_slice(&image[..len]);
        hll
    }

    fn image(&self) -> &[u8] {
        &self.bytes[..Self::DENSE_SIZE]
    }

    /// `HLL_DENSE_GET_REGISTER`.
    fn get_register(&self, index: usize) -> u8 {
        let byte = Self::REGISTER_OFFSET + index * Self::BITS / 8;
        let fb = (index * Self::BITS) & 7;
        let b0 = u64::from(self.bytes[byte]);
        let b1 = u64::from(self.bytes[byte + 1]);
        #[allow(clippy::cast_possible_truncation, reason = "masked to 6 bits")]
        {
            (((b0 >> fb) | (b1 << (8 - fb))) & Self::REGISTER_MAX) as u8
        }
    }

    /// `HLL_DENSE_SET_REGISTER`.
    #[allow(clippy::cast_possible_truncation, reason = "byte-wise register packing")]
    fn set_register(&mut self, index: usize, value: u8) {
        let byte = Self::REGISTER_OFFSET + index * Self::BITS / 8;
        let fb = (index * Self::BITS) & 7;
        let fb8 = 8 - fb;
        let value = u64::from(value);
        self.bytes[byte] &= !((Self::REGISTER_MAX << fb) as u8);
        self.bytes[byte] |= (value << fb) as u8;
        self.bytes[byte + 1] &= !((Self::REGISTER_MAX >> fb8) as u8);
        self.bytes[byte + 1] |= (value >> fb8) as u8;
    }

    /// `hllCount` over the dense registers.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "llroundl of a non-negative estimate"
    )]
    fn count(&self) -> u64 {
        let mut histogram = [0u32; Self::Q + 2];
        for index in 0..Self::REGISTERS {
            histogram[self.get_register(index) as usize] += 1;
        }
        let m = Self::REGISTERS as f64;
        let mut z = m * hll_tau((m - f64::from(histogram[Self::Q + 1])) / m);
        for j in (1..=Self::Q).rev() {
            z += f64::from(histogram[j]);
            z *= 0.5;
        }
        z += m * hll_sigma(f64::from(histogram[0]) / m);
        (ALPHA * m * m / z).round() as u64
    }

    /// DuckDB's `HLLV1::ToNew`: each new register is the maximum of the 64 old ones it
    /// covers.
    fn to_new(&self) -> HyperLogLog {
        let mult = Self::REGISTERS / HLL_REGISTERS;
        let mut result = HyperLogLog::default();
        for i in 0..HLL_REGISTERS {
            let max_old = (0..mult)
                .map(|j| self.get_register(i * mult + j))
                .max()
                .unwrap_or(0);
            result.update(i, max_old);
        }
        result
    }

    /// DuckDB's `HLLV1::FromNew`: the first old register of every group takes the new
    /// register, the others a default chosen so that the counts roughly agree.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "DuckDB's register arithmetic"
    )]
    fn from_new(new_hll: &HyperLogLog) -> Self {
        let mut hll = Self::new();
        let new_count = new_hll.count();
        if new_count == 0 {
            return hll;
        }
        let mult = Self::REGISTERS / HLL_REGISTERS;
        let maximum_zeros = Self::Q as u8;
        let mut avg = 0.0;
        for i in 0..HLL_REGISTERS {
            let max_new = new_hll.k[i].min(maximum_zeros);
            hll.set_register(i * mult, max_new);
            avg += f64::from(max_new);
        }
        avg /= HLL_REGISTERS as f64;
        // The average always overestimates.
        if avg > 10.0 {
            avg *= 0.75;
        } else if avg > 2.0 {
            avg -= 2.0;
        }
        let mut default_val: f64 = 0.0;
        for opt_idx in 0..5 {
            let old_count = hll.count();
            let (newd, oldd) = (new_count as f64, old_count as f64);
            if newd.max(oldd) / newd.min(oldd) < 1.2 {
                break;
            }
            let delta = avg / f64::from(1u32 << (opt_idx + 1));
            if old_count > new_count {
                default_val = if delta > default_val {
                    0.0
                } else {
                    default_val - delta
                };
            } else {
                default_val += delta;
            }
            let floor_fraction = 1.0 - (default_val - default_val.floor());
            for i in 0..HLL_REGISTERS {
                let max_new = new_hll.k[i].min(maximum_zeros);
                let register = if (i as f64) / (HLL_REGISTERS as f64) < floor_fraction {
                    default_val.floor() as u8
                } else {
                    default_val.ceil() as u8
                };
                let register = register.min(max_new);
                for j in 1..mult {
                    hll.set_register(i * mult + j, register);
                }
            }
        }
        hll
    }
}

impl HyperLogLog {
    /// The size of the stored form of `storage_type` (`HLLV1::GetSize`, `sizeof(k)`).
    pub const fn storage_size(storage_type: u8) -> Option<usize> {
        match storage_type {
            HLL_V1 => Some(RedisHll::DENSE_SIZE),
            HLL_V2 => Some(HLL_REGISTERS),
            _ => None,
        }
    }

    /// The sketch from its stored form (DuckDB's `HyperLogLog::Deserialize`).
    pub fn from_storage(storage_type: u8, data: &[u8]) -> Option<Self> {
        match storage_type {
            HLL_V1 => Some(RedisHll::from_image(data).to_new()),
            HLL_V2 => {
                let mut k = [0u8; HLL_REGISTERS];
                let len = data.len().min(HLL_REGISTERS);
                k[..len].copy_from_slice(&data[..len]);
                Some(Self { k })
            }
            _ => None,
        }
    }

    /// The stored form (DuckDB's `HyperLogLog::Serialize`): the registers from storage
    /// version v1.1 on (`ShouldSerialize(3)`), else the Redis sketch.
    pub fn to_storage(&self, v2: bool) -> (u8, Vec<u8>) {
        if v2 {
            (HLL_V2, self.k.to_vec())
        } else {
            (HLL_V1, RedisHll::from_new(self).image().to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_small_sets_exactly_enough() {
        let mut log = HyperLogLog::default();
        assert_eq!(log.count(), 0);
        for value in 0..5u64 {
            log.insert_element(rustdb_row::hash::murmur_hash64(value));
        }
        assert!((4..=6).contains(&log.count()));
    }
}
