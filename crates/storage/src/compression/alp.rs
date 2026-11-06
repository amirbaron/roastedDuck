//! Adaptive Lossless floating-Point compression (DuckDB's `compression/alp/*`).
//!
//! Values are compressed in vectors of [`ALP_VECTOR_SIZE`]: each value is multiplied by
//! `10^exponent / 10^factor` and rounded to an integer, the integers are stored with a
//! frame of reference and bit packing, and values that do not round-trip are stored as
//! exceptions. Since storage version 7 a vector may also be stored uncompressed (exponent
//! [`UNCOMPRESSED_MODE_SENTINEL`]). Segment layout: a `u32` offset of the end of the
//! metadata, the vectors, and one `u32` vector offset per vector growing downwards.

use std::marker::PhantomData;

use std::collections::HashMap;

use rustdb_common::Result;
use rustdb_types::{LogicalType, PhysicalType, Vector};

use super::bitpacking_primitives::{
    minimum_bit_width_unsigned, pack_buffer_u64, required_size, unpack_buffer_u64,
};
use super::primitive::{
    Primitive, align8, corrupt, load, slice, unified_values, update_numeric_stats, write_values,
};
use super::{Analyzer, CodecScan, CompressionInfo, CompressionType, Compressor, SegmentScan, Sequential};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// Values per ALP vector (`AlpConstants::ALP_VECTOR_SIZE`).
pub(crate) const ALP_VECTOR_SIZE: usize = 1024;
/// `AlpConstants::HEADER_SIZE`.
pub(crate) const HEADER_SIZE: usize = 4;
/// The exponent marking an uncompressed vector (`UNCOMPRESSED_MODE_SENTINEL`).
pub(crate) const UNCOMPRESSED_MODE_SENTINEL: u8 = u8::MAX;
/// `AlpConstants::FACT_ARR`.
pub(crate) const FACT_ARR: [i64; 19] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
];

/// A floating point type ALP can compress (`AlpTypedConstants<T>` and the typed parts of
/// `AlpCompression` / `AlpDecompression`).
pub(crate) trait AlpFloat: Primitive + PartialOrd {
    /// The integer with the same bits (`FloatingToExact`).
    type Exact: Primitive;
    /// `MAGIC_NUMBER` (2^22 + 2^23 for floats, 2^51 + 2^52 for doubles).
    const MAGIC_NUMBER: Self;
    /// `MAX_EXPONENT`.
    const MAX_EXPONENT: u8;
    fn exp(index: usize) -> Self;
    fn frac(index: usize) -> Self;
    fn from_i64(value: i64) -> Self;
    /// A truncating cast to `i64` (DuckDB's `LossyNumericCast<int64_t>`).
    fn to_i64(self) -> i64;
    fn mul(self, other: Self) -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
    fn is_finite(self) -> bool;
    fn is_negative_zero(self) -> bool;
    fn to_f64(self) -> f64;
}

/// `AlpTypedConstants<float>::EXP_ARR`.
const EXP_ARR_F32: [f32; 11] = [
    1.0,
    10.0,
    100.0,
    1_000.0,
    10_000.0,
    100_000.0,
    1_000_000.0,
    10_000_000.0,
    100_000_000.0,
    1_000_000_000.0,
    10_000_000_000.0,
];
/// `AlpTypedConstants<float>::FRAC_ARR`.
const FRAC_ARR_F32: [f32; 11] = [
    1.0,
    0.1,
    0.01,
    0.001,
    0.0001,
    0.00001,
    0.000_001,
    0.000_000_1,
    0.000_000_01,
    0.000_000_001,
    0.000_000_000_1,
];
/// `AlpTypedConstants<double>::EXP_ARR`.
const EXP_ARR_F64: [f64; 24] = [
    1.0,
    10.0,
    100.0,
    1_000.0,
    10_000.0,
    100_000.0,
    1_000_000.0,
    10_000_000.0,
    100_000_000.0,
    1_000_000_000.0,
    10_000_000_000.0,
    100_000_000_000.0,
    1_000_000_000_000.0,
    10_000_000_000_000.0,
    100_000_000_000_000.0,
    1_000_000_000_000_000.0,
    10_000_000_000_000_000.0,
    100_000_000_000_000_000.0,
    1_000_000_000_000_000_000.0,
    10_000_000_000_000_000_000.0,
    100_000_000_000_000_000_000.0,
    1_000_000_000_000_000_000_000.0,
    10_000_000_000_000_000_000_000.0,
    100_000_000_000_000_000_000_000.0,
];
/// `AlpTypedConstants<double>::FRAC_ARR`.
const FRAC_ARR_F64: [f64; 21] = [
    1.0,
    0.1,
    0.01,
    0.001,
    0.0001,
    0.00001,
    0.000_001,
    0.000_000_1,
    0.000_000_01,
    0.000_000_001,
    0.000_000_000_1,
    0.000_000_000_01,
    0.000_000_000_001,
    0.000_000_000_000_1,
    0.000_000_000_000_01,
    0.000_000_000_000_001,
    0.000_000_000_000_000_1,
    0.000_000_000_000_000_01,
    0.000_000_000_000_000_001,
    0.000_000_000_000_000_000_1,
    0.000_000_000_000_000_000_01,
];

impl AlpFloat for f32 {
    type Exact = u32;
    const MAGIC_NUMBER: Self = 12_582_912.0;
    const MAX_EXPONENT: u8 = 10;
    #[inline]
    fn exp(index: usize) -> Self {
        EXP_ARR_F32.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn frac(index: usize) -> Self {
        FRAC_ARR_F32.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn from_i64(value: i64) -> Self {
        value as Self
    }
    #[inline]
    fn to_i64(self) -> i64 {
        self as i64
    }
    #[inline]
    fn mul(self, other: Self) -> Self {
        self * other
    }
    #[inline]
    fn add(self, other: Self) -> Self {
        self + other
    }
    #[inline]
    fn sub(self, other: Self) -> Self {
        self - other
    }
    #[inline]
    fn is_finite(self) -> bool {
        Self::is_finite(self)
    }
    #[inline]
    fn is_negative_zero(self) -> bool {
        self == 0.0 && self.is_sign_negative()
    }
    #[inline]
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
}

impl AlpFloat for f64 {
    type Exact = u64;
    const MAGIC_NUMBER: Self = 6_755_399_441_055_744.0;
    const MAX_EXPONENT: u8 = 18;
    #[inline]
    fn exp(index: usize) -> Self {
        EXP_ARR_F64.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn frac(index: usize) -> Self {
        FRAC_ARR_F64.get(index).copied().unwrap_or(Self::NAN)
    }
    #[inline]
    fn from_i64(value: i64) -> Self {
        value as Self
    }
    #[inline]
    fn to_i64(self) -> i64 {
        self as i64
    }
    #[inline]
    fn mul(self, other: Self) -> Self {
        self * other
    }
    #[inline]
    fn add(self, other: Self) -> Self {
        self + other
    }
    #[inline]
    fn sub(self, other: Self) -> Self {
        self - other
    }
    #[inline]
    fn is_finite(self) -> bool {
        Self::is_finite(self)
    }
    #[inline]
    fn is_negative_zero(self) -> bool {
        self == 0.0 && self.is_sign_negative()
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self
    }
}

/// `AlpCompression::DecodeValue`.
#[inline]
pub(crate) fn decode_value<T: AlpFloat>(encoded: i64, exponent: u8, factor: u8) -> T {
    let fact = FACT_ARR.get(usize::from(factor)).copied().unwrap_or(1);
    T::from_i64(encoded)
        .mul(T::from_i64(fact))
        .mul(T::frac(usize::from(exponent)))
}

/// The scan state of an ALP segment (DuckDB's `AlpScanState`).
#[derive(Debug)]
pub(crate) struct AlpScanState<T> {
    data: PersistentData,
    count: usize,
    /// Offset of the next vector's metadata entry (`metadata_ptr`).
    metadata: usize,
    total_value_count: usize,
    /// The decoded current vector and the read position in it.
    decoded: Vec<T>,
    index: usize,
    unpacked: Vec<u64>,
    _marker: PhantomData<T>,
}

impl<T: AlpFloat> AlpScanState<T> {
    fn left_in_vector(&self) -> usize {
        ALP_VECTOR_SIZE - self.total_value_count % ALP_VECTOR_SIZE
    }

    fn vector_finished(&self) -> bool {
        self.total_value_count.is_multiple_of(ALP_VECTOR_SIZE)
    }

    /// `SkipVector`.
    fn skip_vector(&mut self) {
        self.metadata = self.metadata.wrapping_sub(4);
        self.total_value_count += ALP_VECTOR_SIZE.min(self.count - self.total_value_count);
    }

    /// `LoadVector`: decodes the next vector into `self.decoded`.
    fn load_vector(&mut self, block_size: usize) -> Result<()> {
        self.index = 0;
        self.metadata = self.metadata.wrapping_sub(4);
        let bytes = self.data.bytes();
        let data_offset = load::<u32>(bytes, self.metadata)? as usize;
        if data_offset >= block_size {
            return Err(corrupt(format!(
                "Corrupted ALP segment: stored data_byte_offset ({data_offset}) exceeds the segments block size \
                 ({block_size})"
            )));
        }
        let vector_size = ALP_VECTOR_SIZE.min(self.count - self.total_value_count);
        let mut pointer = data_offset;
        let exponent: u8 = load(bytes, pointer)?;
        pointer += 1;
        self.decoded.clear();
        if exponent == UNCOMPRESSED_MODE_SENTINEL {
            let copy_size = vector_size * T::SIZE;
            if pointer + copy_size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALP segment: stored vector_size is invalid, to-copy bytes ({copy_size}) would exceed \
                     bytes remaining in the block ({})",
                    block_size as i64 - pointer as i64
                )));
            }
            let raw = slice(bytes, pointer, copy_size)?;
            self.decoded.extend(raw.chunks_exact(T::SIZE).map(T::load));
            return Ok(());
        }
        let factor: u8 = load(bytes, pointer)?;
        let exceptions_count = usize::from(load::<u16>(bytes, pointer + 1)?);
        let frame_of_reference: u64 = load(bytes, pointer + 3)?;
        let bit_width: u8 = load(bytes, pointer + 11)?;
        pointer += 12;
        if exceptions_count > vector_size {
            return Err(corrupt(format!(
                "Corrupted ALP segment: exceptions_count ({exceptions_count}) exceeds vector_size ({vector_size})"
            )));
        }
        if factor > exponent {
            return Err(corrupt(format!(
                "Corrupted ALP segment: v_factor ({factor}) exceeds v_exponent ({exponent})"
            )));
        }
        if bit_width > 64 {
            return Err(corrupt(format!(
                "Corrupted ALP segment: Invalid bit_width encountered: {bit_width}"
            )));
        }
        // The payload checks of `LoadVector`, against the sizes of DuckDB's vector state
        // buffers and the block.
        let mut read_bytes = 0;
        let mut check = |size: usize, max: usize, what: &str| {
            if size > max || data_offset + read_bytes + size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALP segment: {what} payload too large"
                )));
            }
            read_bytes += size;
            Ok(())
        };
        let packed_size = if bit_width > 0 {
            required_size(vector_size, bit_width)
        } else {
            0
        };
        check(packed_size, ALP_VECTOR_SIZE * 8, "encoded")?;
        let exceptions_size = exceptions_count * T::SIZE;
        let positions_size = exceptions_count * 2;
        if exceptions_count > 0 {
            check(exceptions_size, ALP_VECTOR_SIZE * T::SIZE, "exceptions")?;
            check(positions_size, ALP_VECTOR_SIZE * 2, "exceptions_positions")?;
        }
        let positions = slice(bytes, pointer + packed_size + exceptions_size, positions_size)?;
        for position in positions.as_chunks::<2>().0 {
            let position = u16::from_le_bytes([position[0], position[1]]);
            if usize::from(position) >= vector_size {
                return Err(corrupt(format!(
                    "Corrupted ALP segment: exception position ({position}) exceeds vector_size ({vector_size})"
                )));
            }
        }
        // `AlpDecompression::Decompress`: unpack, add the frame of reference, decode, patch.
        self.unpacked.clear();
        self.unpacked.resize(vector_size.next_multiple_of(32), 0);
        if bit_width > 0 {
            unpack_buffer_u64(
                slice(bytes, pointer, packed_size)?,
                vector_size,
                bit_width,
                &mut self.unpacked,
            );
        }
        pointer += packed_size;
        self.decoded
            .extend(self.unpacked[..vector_size].iter().map(|&value| {
                decode_value::<T>(value.wrapping_add(frame_of_reference) as i64, exponent, factor)
            }));
        let exceptions = slice(bytes, pointer, exceptions_size)?;
        for (exception, position) in exceptions.chunks_exact(T::SIZE).zip(positions.as_chunks::<2>().0) {
            let position = usize::from(u16::from_le_bytes([position[0], position[1]]));
            self.decoded[position] = T::load(exception);
        }
        Ok(())
    }

    /// `ScanVector`: reads (or skips) `count` values up to the vector boundary.
    fn scan_vector(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        out: Option<(&mut Vector, usize)>,
    ) -> Result<()> {
        if self.vector_finished() && self.total_value_count < self.count {
            self.load_vector(segment.block_size())?;
        }
        if let Some((result, offset)) = out {
            let values = self
                .decoded
                .get(self.index..self.index + count)
                .ok_or_else(|| corrupt("Corrupted ALP segment: vector shorter than its count"))?;
            write_values(result, offset, values)?;
        }
        self.index += count;
        self.total_value_count += count;
        Ok(())
    }
}

impl<T: AlpFloat> CodecScan for AlpScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let metadata = load::<u32>(data.bytes(), 0)? as usize;
        Ok(Self {
            data,
            count: segment.count() as usize,
            metadata,
            total_value_count: 0,
            decoded: Vec::with_capacity(ALP_VECTOR_SIZE),
            index: 0,
            unpacked: Vec::with_capacity(ALP_VECTOR_SIZE),
            _marker: PhantomData,
        })
    }

    /// `AlpScanState::Skip`.
    fn skip(&mut self, segment: &ColumnSegment, mut skip_count: usize) -> Result<()> {
        if self.total_value_count != 0 && !self.vector_finished() {
            let to_skip = skip_count.min(self.left_in_vector());
            self.scan_vector(segment, to_skip, None)?;
            skip_count -= to_skip;
        }
        for _ in 0..skip_count / ALP_VECTOR_SIZE {
            self.skip_vector();
        }
        skip_count %= ALP_VECTOR_SIZE;
        if skip_count > 0 {
            self.scan_vector(segment, skip_count, None)?;
        }
        Ok(())
    }

    /// `AlpScanPartial`.
    fn scan_partial(
        &mut self,
        segment: &ColumnSegment,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            let to_scan = (count - scanned).min(self.left_in_vector());
            self.scan_vector(segment, to_scan, Some((&mut *result, result_offset + scanned)))?;
            scanned += to_scan;
        }
        Ok(())
    }
}

/// `AlpCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Float => Sequential::<AlpScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<AlpScanState<f64>>::boxed(segment),
        _ => return None,
    })
}

/// `AlpConstants::RG_SAMPLES_DUCKDB_JUMP`: every 7th vector of a row group is sampled.
pub(crate) const RG_SAMPLES_DUCKDB_JUMP: usize = (122_880 / 8) / 2048;
/// `AlpConstants::SAMPLES_PER_VECTOR`.
pub(crate) const SAMPLES_PER_VECTOR: usize = 32;
/// `AlpConstants::MAX_COMBINATIONS`.
const MAX_COMBINATIONS: usize = 5;
/// `AlpConstants::SAMPLING_EARLY_EXIT_THRESHOLD`.
const SAMPLING_EARLY_EXIT_THRESHOLD: usize = 2;
/// `AlpConstants::ENCODING_UPPER_LIMIT`.
const ENCODING_UPPER_LIMIT: f64 = 9_223_372_036_854_774_784.0;
/// `AlpConstants::ENCODING_LOWER_LIMIT`.
const ENCODING_LOWER_LIMIT: f64 = -9_223_372_036_854_774_784.0;
/// `AlpConstants::COMPACT_BLOCK_THRESHOLD`.
pub(crate) const COMPACT_BLOCK_THRESHOLD: f64 = 0.80;
/// The fixed part of a compressed vector: exponent, factor, exception count, frame of
/// reference and bit width.
const VECTOR_HEADER_SIZE: usize = 1 + 1 + 2 + 8 + 1;

/// `AlpUtils::GetSamplingParameters`: the values looked at, the sampling increment and the
/// number of sampled values of a vector of `count` values.
pub(crate) fn sampling_parameters(count: usize) -> (usize, usize, usize) {
    let lookup = count.min(ALP_VECTOR_SIZE);
    let increments = 1.max((lookup as f64 / SAMPLES_PER_VECTOR as f64).ceil() as usize);
    let sampled = (lookup as f64 / increments as f64).ceil() as usize;
    (lookup, increments, sampled)
}

/// `AlpUtils::MustSkipSamplingFromCurrentVector`.
pub(crate) fn must_skip_sampling(vectors_count: usize, vectors_sampled: usize, count: usize) -> bool {
    if !vectors_count.is_multiple_of(RG_SAMPLES_DUCKDB_JUMP) {
        return true;
    }
    count < SAMPLES_PER_VECTOR && vectors_sampled != 0
}

/// `AlpUtils::FindAndReplaceNullsInVector`: NULL slots get the first value whose index is
/// not in `positions` (which, like DuckDB's buffer, may hold stale entries past `nulls`).
pub(crate) fn replace_nulls<T: Copy + Default>(
    values: &mut [T],
    positions: &[u16],
    count: usize,
    nulls: usize,
) {
    if nulls == 0 {
        return;
    }
    let replacement = (0..count)
        .find(|&i| usize::from(positions[i]) != i)
        .map_or_else(T::default, |i| values[i]);
    for &position in &positions[..nulls] {
        values[usize::from(position)] = replacement;
    }
}

/// An exponent/factor combination and how good it is (`AlpCombination`).
#[derive(Clone, Copy, Debug)]
struct Combination {
    exponent: u8,
    factor: u8,
    appearances: u64,
    estimated_size: u64,
}

/// `CompareALPCombinations`: whether `a` is better than `b`.
fn better(a: &Combination, b: &Combination) -> bool {
    a.appearances > b.appearances
        || (a.appearances == b.appearances && a.estimated_size < b.estimated_size)
        || (a.appearances == b.appearances && a.estimated_size == b.estimated_size && b.exponent < a.exponent)
        || (a.appearances == b.appearances
            && a.estimated_size == b.estimated_size
            && b.exponent == a.exponent
            && b.factor < a.factor)
}

/// `AlpCompression::IsImpossibleToEncode`.
fn impossible_to_encode<T: AlpFloat>(n: T) -> bool {
    let value = n.to_f64();
    !n.is_finite() || value > ENCODING_UPPER_LIMIT || value < ENCODING_LOWER_LIMIT || n.is_negative_zero()
}

/// `AlpCompression::EncodeValue` (with `NumberToInt64`).
fn encode_value<T: AlpFloat>(value: T, exponent: u8, factor: u8) -> i64 {
    let encoded = value
        .mul(T::exp(usize::from(exponent)))
        .mul(T::frac(usize::from(factor)));
    if impossible_to_encode(encoded) {
        return ENCODING_UPPER_LIMIT as i64;
    }
    encoded.add(T::MAGIC_NUMBER).sub(T::MAGIC_NUMBER).to_i64()
}

/// `DryCompressToEstimateSize`: the estimated bits of `values` with the combination.
fn dry_compress<T: AlpFloat>(values: &[T], exponent: u8, factor: u8, penalize_exceptions: bool) -> u64 {
    let mut exceptions = 0u64;
    let mut non_exceptions = 0u64;
    let mut max_encoded = i64::MIN;
    let mut min_encoded = i64::MAX;
    for &value in values {
        let encoded = encode_value(value, exponent, factor);
        if decode_value::<T>(encoded, exponent, factor) == value {
            non_exceptions += 1;
            max_encoded = max_encoded.max(encoded);
            min_encoded = min_encoded.min(encoded);
        } else {
            exceptions += 1;
        }
    }
    if penalize_exceptions && non_exceptions < 2 {
        return u64::MAX;
    }
    let delta = (max_encoded as u64).wrapping_sub(min_encoded as u64);
    let bits_per_value = (delta.wrapping_add(1) as f64).log2().ceil() as u32;
    let exact_bits = T::SIZE as u64 * 8;
    values.len() as u64 * u64::from(bits_per_value) + exceptions * (exact_bits + 16)
}

/// The state of the ALP compression of one column (`AlpCompressionData`).
#[derive(Debug)]
struct AlpData<T> {
    combinations: Vec<Combination>,
    exponent: u8,
    factor: u8,
    exceptions_count: usize,
    bit_width: u8,
    bp_size: usize,
    frame_of_reference: u64,
    encoded: Vec<i64>,
    exceptions: Vec<T>,
    /// Exception positions; entries past the exception count keep stale values.
    exceptions_positions: Vec<u16>,
    packed: Vec<u8>,
}

impl<T: AlpFloat> AlpData<T> {
    fn new() -> Self {
        Self {
            combinations: Vec::new(),
            exponent: 0,
            factor: 0,
            exceptions_count: 0,
            bit_width: 0,
            bp_size: 0,
            frame_of_reference: 0,
            encoded: vec![0; ALP_VECTOR_SIZE],
            exceptions: vec![T::default(); ALP_VECTOR_SIZE],
            exceptions_positions: vec![0; ALP_VECTOR_SIZE],
            packed: vec![0; ALP_VECTOR_SIZE * 8],
        }
    }

    /// `RequiredSpace`.
    fn required_space(&self) -> usize {
        self.bp_size + self.exceptions_count * (T::SIZE + 2) + VECTOR_HEADER_SIZE
    }

    /// `FindTopKCombinations`: the best combination of every sampled vector, ranked by how
    /// often it is best.
    fn find_top_k_combinations(&mut self, samples: &[Vec<T>]) {
        self.combinations.clear();
        let mut counts: HashMap<(u8, u8), u64> = HashMap::new();
        let exact_bits = T::SIZE as u64 * 8;
        for sample in samples {
            let n = sample.len() as u64;
            let mut best = Combination {
                exponent: T::MAX_EXPONENT,
                factor: T::MAX_EXPONENT,
                appearances: 0,
                estimated_size: n * (exact_bits + 16) + n * exact_bits,
            };
            for exponent in (0..=T::MAX_EXPONENT).rev() {
                for factor in (0..=exponent).rev() {
                    let current = Combination {
                        exponent,
                        factor,
                        appearances: 0,
                        estimated_size: dry_compress(sample, exponent, factor, true),
                    };
                    if better(&current, &best) {
                        best = current;
                    }
                }
            }
            *counts.entry((best.exponent, best.factor)).or_insert(0) += 1;
        }
        let mut ranked: Vec<Combination> = counts
            .into_iter()
            .map(|((exponent, factor), appearances)| Combination {
                exponent,
                factor,
                appearances,
                estimated_size: 0,
            })
            .collect();
        ranked.sort_by(|a, b| {
            if better(a, b) {
                std::cmp::Ordering::Less
            } else if better(b, a) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        ranked.truncate(MAX_COMBINATIONS);
        self.combinations = ranked;
    }

    /// `FindBestFactorAndExponent`: the best of the top combinations for a vector.
    fn find_best_factor_and_exponent(&mut self, values: &[T]) {
        let increments = 1.max((values.len() as f64 / SAMPLES_PER_VECTOR as f64).ceil() as usize);
        let sample: Vec<T> = values.iter().step_by(increments).copied().collect();
        let (mut best_exponent, mut best_factor) = (0, 0);
        let mut best_bits = u64::MAX;
        let mut worse = 0;
        for combination in &self.combinations {
            let size = dry_compress(&sample, combination.exponent, combination.factor, false);
            if size >= best_bits {
                worse += 1;
                if worse == SAMPLING_EARLY_EXIT_THRESHOLD {
                    break;
                }
                continue;
            }
            best_bits = size;
            best_exponent = combination.exponent;
            best_factor = combination.factor;
            worse = 0;
        }
        self.exponent = best_exponent;
        self.factor = best_factor;
    }

    /// `AlpCompression::Compress`; with `write` the integers are frame-of-reference encoded
    /// and bit packed into `packed`.
    fn compress(&mut self, values: &[T], null_positions: &[u16], write: bool) {
        let n = values.len();
        if self.combinations.len() > 1 {
            self.find_best_factor_and_exponent(values);
        } else if let Some(only) = self.combinations.first() {
            self.exponent = only.exponent;
            self.factor = only.factor;
        }
        let mut exceptions = 0;
        for (i, &value) in values.iter().enumerate() {
            let encoded = encode_value(value, self.exponent, self.factor);
            let decoded: T = decode_value(encoded, self.exponent, self.factor);
            self.encoded[i] = encoded;
            self.exceptions_positions[exceptions] = i as u16;
            exceptions += usize::from(decoded != value);
        }
        let replacement = (0..n)
            .find(|&i| usize::from(self.exceptions_positions[i]) != i)
            .map_or(0, |i| self.encoded[i]);
        for index in 0..exceptions {
            let position = usize::from(self.exceptions_positions[index]);
            self.encoded[position] = replacement;
            self.exceptions[index] = values[position];
        }
        self.exceptions_count = exceptions;
        for &position in null_positions {
            self.encoded[usize::from(position)] = replacement;
        }
        let (min, max) = self.encoded[..n]
            .iter()
            .fold((i64::MAX, i64::MIN), |(min, max), &v| (min.min(v), max.max(v)));
        let min_max_diff = (max as u64).wrapping_sub(min as u64);
        let bit_width = minimum_bit_width_unsigned(u128::from(min_max_diff), 64);
        self.bp_size = required_size(n, bit_width);
        if write && bit_width > 0 {
            let unsigned: Vec<u64> = self.encoded[..n]
                .iter()
                .map(|&v| (v as u64).wrapping_sub(min as u64))
                .collect();
            pack_buffer_u64(&unsigned, bit_width, &mut self.packed[..self.bp_size]);
        }
        self.bit_width = bit_width;
        self.frame_of_reference = min as u64;
    }
}

/// `AlpAnalyzeState`: samples vectors and estimates the compressed size from them.
#[derive(Debug)]
struct AlpAnalyzer<T> {
    ty: LogicalType,
    info: CompressionInfo,
    total_bytes_used: usize,
    current_bytes_used: usize,
    vectors_sampled: usize,
    total_values: usize,
    vectors_count: usize,
    rowgroup_sample: Vec<Vec<T>>,
    complete_vectors_sampled: Vec<Vec<T>>,
    data: AlpData<T>,
}

impl<T: AlpFloat> AlpAnalyzer<T> {
    fn flush_segment(&mut self) {
        self.total_bytes_used += self.current_bytes_used + 4;
        self.current_bytes_used = 0;
    }

    fn has_enough_space(&self, vector_size: usize) -> bool {
        align8(self.current_bytes_used + vector_size) <= self.info.block_size - 4
    }
}

impl<T: AlpFloat> Analyzer for AlpAnalyzer<T> {
    /// `AlpAnalyze`.
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        let skip = must_skip_sampling(self.vectors_count, self.vectors_sampled, count);
        self.vectors_count += 1;
        self.total_values += count;
        if skip {
            return Ok(true);
        }
        let (lookup, increments, sampled) = sampling_parameters(count);
        let mut positions = vec![0u16; lookup];
        let mut values = Vec::with_capacity(lookup);
        let mut nulls = 0;
        for (i, (value, valid)) in unified_values::<T>(input, lookup).enumerate() {
            positions[nulls] = i as u16;
            nulls += usize::from(!valid);
            values.push(value);
        }
        replace_nulls(&mut values, &positions, lookup, nulls);
        let sample: Vec<T> = values.iter().step_by(increments).copied().collect();
        debug_assert_eq!(sample.len(), sampled);
        self.complete_vectors_sampled.push(values);
        self.rowgroup_sample.push(sample);
        self.vectors_sampled += 1;
        Ok(true)
    }

    /// `AlpFinalAnalyze`.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        let samples = std::mem::take(&mut self.rowgroup_sample);
        self.data.find_top_k_combinations(&samples);
        let mut compressed_values = 0;
        let vectors = std::mem::take(&mut self.complete_vectors_sampled);
        for vector in &vectors {
            self.data.compress(vector, &[], false);
            let uncompressed_size = 1 + T::SIZE * vector.len();
            let compressed_size = self.data.required_space();
            let should_compress = compressed_size < uncompressed_size || self.info.storage_version < 7;
            let vector_size = if should_compress {
                compressed_size
            } else {
                uncompressed_size
            };
            if !self.has_enough_space(vector_size) {
                self.flush_segment();
            }
            self.current_bytes_used += vector_size;
            compressed_values += vector.len();
        }
        self.complete_vectors_sampled = vectors;
        self.flush_segment();
        if compressed_values == 0 {
            return Ok(None);
        }
        let factor = self.total_values / compressed_values;
        Ok(Some((align8(self.total_bytes_used) * factor) as u64))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let mut data = AlpData::<T>::new();
        data.combinations = self.data.combinations;
        let physical = self.ty.physical_type();
        Ok(Box::new(AlpCompressor {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            physical,
            info: self.info,
            buffer: vec![0; self.info.block_size],
            count: 0,
            data_bytes_used: 0,
            metadata_offset: self.info.block_size,
            next_vector_start: HEADER_SIZE as u32,
            input: Vec::with_capacity(ALP_VECTOR_SIZE),
            null_positions: vec![0; ALP_VECTOR_SIZE],
            nulls: 0,
            data,
        }))
    }
}

/// `AlpCompressionState`: vectors grow from the 4-byte header, their offsets from the end
/// of the block downwards.
#[derive(Debug)]
struct AlpCompressor<T> {
    ty: LogicalType,
    physical: PhysicalType,
    info: CompressionInfo,
    buffer: Vec<u8>,
    count: u64,
    stats: BaseStatistics,
    data_bytes_used: usize,
    /// Start of the vector offsets written so far (`metadata_ptr`).
    metadata_offset: usize,
    next_vector_start: u32,
    input: Vec<T>,
    /// NULL positions of the vector; entries past `nulls` keep stale values.
    null_positions: Vec<u16>,
    nulls: usize,
    data: AlpData<T>,
}

impl<T: AlpFloat> AlpCompressor<T> {
    fn used_space(&self) -> usize {
        HEADER_SIZE + self.data_bytes_used
    }

    fn has_enough_space(&self, vector_size: usize) -> bool {
        align8(self.used_space() + vector_size) < self.metadata_offset - 4
    }

    fn create_empty_segment(&mut self) {
        self.buffer.fill(0);
        self.count = 0;
        self.stats = BaseStatistics::create_empty(self.ty.clone());
        self.metadata_offset = self.info.block_size;
        self.next_vector_start = HEADER_SIZE as u32;
    }

    /// `CompressVector`.
    fn compress_vector(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let n = self.input.len();
        replace_nulls(&mut self.input, &self.null_positions, n, self.nulls);
        let input = std::mem::take(&mut self.input);
        self.data
            .compress(&input, &self.null_positions[..self.nulls], true);
        let uncompressed_size = 1 + T::SIZE * n;
        let compressed_size = self.data.required_space();
        let should_compress = compressed_size < uncompressed_size || self.info.storage_version < 7;
        let vector_size = if should_compress {
            compressed_size
        } else {
            uncompressed_size
        };
        if !self.has_enough_space(vector_size) {
            self.flush_segment(sink)?;
            self.create_empty_segment();
        }
        if self.nulls > 0 {
            self.stats.set_has_null_fast();
        }
        if n != self.nulls {
            self.stats.set_has_no_null_fast();
            for &value in &input {
                update_numeric_stats(&mut self.stats, self.physical, value);
            }
        }
        self.count += n as u64;
        let mut offset = self.used_space();
        if should_compress {
            // `FlushCompressedVector`.
            self.buffer[offset] = self.data.exponent;
            self.buffer[offset + 1] = self.data.factor;
            self.buffer[offset + 2..offset + 4]
                .copy_from_slice(&(self.data.exceptions_count as u16).to_le_bytes());
            self.buffer[offset + 4..offset + 12].copy_from_slice(&self.data.frame_of_reference.to_le_bytes());
            self.buffer[offset + 12] = self.data.bit_width;
            offset += VECTOR_HEADER_SIZE;
            let bp_size = self.data.bp_size;
            self.buffer[offset..offset + bp_size].copy_from_slice(&self.data.packed[..bp_size]);
            offset += bp_size;
            for exception in &self.data.exceptions[..self.data.exceptions_count] {
                exception.store(&mut self.buffer[offset..]);
                offset += T::SIZE;
            }
            for position in &self.data.exceptions_positions[..self.data.exceptions_count] {
                self.buffer[offset..offset + 2].copy_from_slice(&position.to_le_bytes());
                offset += 2;
            }
        } else {
            // `FlushUncompressedVector`.
            self.buffer[offset] = UNCOMPRESSED_MODE_SENTINEL;
            offset += 1;
            for value in &input {
                value.store(&mut self.buffer[offset..]);
                offset += T::SIZE;
            }
        }
        self.data_bytes_used += vector_size;
        self.metadata_offset -= 4;
        self.buffer[self.metadata_offset..self.metadata_offset + 4]
            .copy_from_slice(&self.next_vector_start.to_le_bytes());
        self.next_vector_start = self.used_space() as u32;
        self.input = input;
        self.input.clear();
        self.nulls = 0;
        Ok(())
    }

    /// `FlushSegment`: compacts the vector offsets next to the data when the block is less
    /// than 80% used.
    fn flush_segment(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let block_size = self.info.block_size;
        let metadata_start = align8(self.used_space());
        let metadata_bytes = block_size - self.metadata_offset;
        let mut total = block_size;
        let used = f64::from((metadata_start + metadata_bytes) as f32 / block_size as f32);
        if used < COMPACT_BLOCK_THRESHOLD {
            self.buffer
                .copy_within(self.metadata_offset..block_size, metadata_start);
            total = metadata_start + metadata_bytes;
        }
        self.buffer[..4].copy_from_slice(&(total as u32).to_le_bytes());
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        sink.flush(FinishedSegment {
            data: self.buffer[..total].to_vec(),
            count: self.count,
            stats,
            compression: CompressionType::Alp,
            overflow_blocks: Vec::new(),
        })?;
        self.data_bytes_used = 0;
        Ok(())
    }
}

impl<T: AlpFloat> Compressor for AlpCompressor<T> {
    /// `AlpCompressionState::Append`.
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        for (value, valid) in unified_values::<T>(input, count) {
            let index = self.input.len();
            self.null_positions[self.nulls] = index as u16;
            self.nulls += usize::from(!valid);
            self.input.push(value);
            if self.input.len() == ALP_VECTOR_SIZE {
                self.compress_vector(sink)?;
            }
        }
        Ok(())
    }

    /// `AlpCompressionState::Finalize`.
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        if !self.input.is_empty() {
            self.compress_vector(sink)?;
        }
        self.flush_segment(sink)
    }
}

fn analyzer<T: AlpFloat>(ty: &LogicalType, info: &CompressionInfo) -> Box<dyn Analyzer> {
    Box::new(AlpAnalyzer::<T> {
        ty: ty.clone(),
        info: *info,
        total_bytes_used: 0,
        current_bytes_used: 0,
        vectors_sampled: 0,
        total_values: 0,
        vectors_count: 0,
        rowgroup_sample: Vec::new(),
        complete_vectors_sampled: Vec::new(),
        data: AlpData::new(),
    })
}

/// Whether blocks smaller than the default allocation can use ALP: before storage version
/// 7 (v1.5) they cannot fall back to uncompressed vectors.
pub(crate) fn supports_block_size(info: &CompressionInfo) -> bool {
    info.block_size + info.block_header_size >= crate::storage_info::DEFAULT_BLOCK_ALLOC_SIZE
        || info.storage_version >= 7
}

/// `AlpCompressionFun::GetFunction(type).init_analyze`.
pub(crate) fn init_analyze(
    ty: &LogicalType,
    physical: PhysicalType,
    info: &CompressionInfo,
) -> Option<Box<dyn Analyzer>> {
    if !supports_block_size(info) {
        return None;
    }
    match physical {
        PhysicalType::Float => Some(analyzer::<f32>(ty, info)),
        PhysicalType::Double => Some(analyzer::<f64>(ty, info)),
        _ => None,
    }
}
