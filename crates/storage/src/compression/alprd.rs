//! ALP for "real doubles" (DuckDB's `compression/alprd/*`).
//!
//! Each value's bits are cut in a left part (dictionary encoded with up to 8 entries per
//! segment, other left parts stored as exceptions) and a right part (bit packed). Values
//! are stored in vectors of [`ALP_VECTOR_SIZE`]; since storage version 7 a vector may be
//! stored uncompressed (exception count [`UNCOMPRESSED_MODE_SENTINEL`]). Segment layout: a
//! `u32` offset of the end of the metadata, the right and left bit widths, the dictionary
//! size and dictionary, the vectors, and one `u32` vector offset per vector growing
//! downwards.

use std::marker::PhantomData;

use std::collections::HashMap;

use roastedduck_common::Result;
use roastedduck_types::{LogicalType, PhysicalType, Vector};

use super::alp::{
    ALP_VECTOR_SIZE, COMPACT_BLOCK_THRESHOLD, must_skip_sampling, replace_nulls, sampling_parameters,
};
use super::bitpacking_primitives::{pack_buffer_u64, required_size, unpack_buffer_u64};
use super::primitive::{
    Primitive, align8, corrupt, load, slice, unified_values, update_numeric_stats, write_values,
};
use super::{Analyzer, CodecScan, CompressionInfo, CompressionType, Compressor, SegmentScan, Sequential};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// `AlpRDConstants::MAX_DICTIONARY_SIZE`.
pub(crate) const MAX_DICTIONARY_SIZE: usize = 8;
/// `AlpRDConstants::HEADER_SIZE`: metadata pointer, bit widths and dictionary size.
pub(crate) const HEADER_SIZE: usize = 4 + 1 + 1 + 1;
/// The exception count marking an uncompressed vector (`UNCOMPRESSED_MODE_SENTINEL`).
pub(crate) const UNCOMPRESSED_MODE_SENTINEL: u16 = u16::MAX;

/// A floating point type stored as its bit pattern (`FloatingToExact`).
pub(crate) trait RdFloat: Primitive {
    /// Bits of the exact (integer) type.
    const BITS: u32;
    fn from_bits_u64(bits: u64) -> Self;
    fn to_bits_u64(self) -> u64;
}

impl RdFloat for f32 {
    const BITS: u32 = 32;
    #[inline]
    fn from_bits_u64(bits: u64) -> Self {
        Self::from_bits(bits as u32)
    }
    #[inline]
    fn to_bits_u64(self) -> u64 {
        u64::from(self.to_bits())
    }
}

impl RdFloat for f64 {
    const BITS: u32 = 64;
    #[inline]
    fn from_bits_u64(bits: u64) -> Self {
        Self::from_bits(bits)
    }
    #[inline]
    fn to_bits_u64(self) -> u64 {
        self.to_bits()
    }
}

/// The scan state of an ALPRD segment (DuckDB's `AlpRDScanState`).
#[derive(Debug)]
pub(crate) struct AlpRdScanState<T> {
    data: PersistentData,
    count: usize,
    metadata: usize,
    total_value_count: usize,
    right_bit_width: u8,
    left_bit_width: u8,
    dictionary: [u16; MAX_DICTIONARY_SIZE],
    decoded: Vec<T>,
    index: usize,
    left: Vec<u64>,
    right: Vec<u64>,
    _marker: PhantomData<T>,
}

impl<T: RdFloat> AlpRdScanState<T> {
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

    /// `LoadVector` + `AlpRDDecompression::Decompress`.
    fn load_vector(&mut self, block_size: usize) -> Result<()> {
        self.index = 0;
        self.metadata = self.metadata.wrapping_sub(4);
        let bytes = self.data.bytes();
        let data_offset = load::<u32>(bytes, self.metadata)? as usize;
        if data_offset >= block_size {
            return Err(corrupt(format!(
                "Corrupted ALPRD segment: stored data_byte_offset ({data_offset}) exceeds the segments block size \
                 ({block_size})"
            )));
        }
        let vector_size = ALP_VECTOR_SIZE.min(self.count - self.total_value_count);
        let mut pointer = data_offset;
        let exceptions_count = load::<u16>(bytes, pointer)?;
        pointer += 2;
        self.decoded.clear();
        if exceptions_count == UNCOMPRESSED_MODE_SENTINEL {
            let copy_size = vector_size * T::SIZE;
            if pointer + copy_size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALPRD segment: stored vector_size is invalid, to-copy bytes ({copy_size}) would exceed \
                     bytes remaining in the block ({})",
                    block_size as i64 - pointer as i64
                )));
            }
            let raw = slice(bytes, pointer, copy_size)?;
            self.decoded.extend(raw.chunks_exact(T::SIZE).map(T::load));
            return Ok(());
        }
        let exceptions_count = usize::from(exceptions_count);
        let left_size = required_size(vector_size, self.left_bit_width);
        let right_size = required_size(vector_size, self.right_bit_width);
        let padded = vector_size.next_multiple_of(32);
        self.left.clear();
        self.left.resize(padded, 0);
        self.right.clear();
        self.right.resize(padded, 0);
        // The payload checks of `LoadVector`, against the sizes of DuckDB's vector state
        // buffers and the block (the right parts reuse the left parts' message).
        let mut read_bytes = 0;
        let mut check = |size: usize, max: usize, what: &str| {
            if size > max || data_offset + read_bytes + size > block_size {
                return Err(corrupt(format!(
                    "Corrupted ALPRD segment: {what} payload too large"
                )));
            }
            read_bytes += size;
            Ok(())
        };
        check(left_size, ALP_VECTOR_SIZE * 8, "left_encoded")?;
        check(right_size, ALP_VECTOR_SIZE * 8, "left_encoded")?;
        if exceptions_count > 0 {
            check(exceptions_count * 2, ALP_VECTOR_SIZE * 2, "exceptions")?;
            check(exceptions_count * 2, ALP_VECTOR_SIZE * 2, "exceptions_positions")?;
        }
        let left = slice(bytes, pointer, left_size)?;
        unpack_buffer_u64(left, vector_size, self.left_bit_width, &mut self.left);
        pointer += left_size;
        let right = slice(bytes, pointer, right_size)?;
        unpack_buffer_u64(right, vector_size, self.right_bit_width, &mut self.right);
        pointer += right_size;
        let right_bit_width = u32::from(self.right_bit_width);
        let mask = if T::BITS == 64 {
            u64::MAX
        } else {
            (1u64 << T::BITS) - 1
        };
        let combine = |left: u16, right: u64| {
            T::from_bits_u64((u64::from(left).checked_shl(right_bit_width).unwrap_or(0) | right) & mask)
        };
        for row in 0..vector_size {
            let left = self
                .dictionary
                .get(self.left[row] as usize)
                .copied()
                .unwrap_or_default();
            self.decoded.push(combine(left, self.right[row]));
        }
        if exceptions_count > 0 {
            let exceptions = slice(bytes, pointer, exceptions_count * 2)?;
            let positions = slice(bytes, pointer + exceptions_count * 2, exceptions_count * 2)?;
            for (exception, position) in exceptions
                .as_chunks::<2>()
                .0
                .iter()
                .zip(positions.as_chunks::<2>().0)
            {
                let left = u16::from_le_bytes([exception[0], exception[1]]);
                let position = usize::from(u16::from_le_bytes([position[0], position[1]]));
                if position >= vector_size {
                    return Err(corrupt(format!(
                        "Corrupted ALPRD segment: exception position ({position}) exceeds vector_size ({vector_size})"
                    )));
                }
                self.decoded[position] = combine(left, self.right[position]);
            }
        }
        Ok(())
    }

    /// `ScanVector`.
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
                .ok_or_else(|| corrupt("Corrupted ALPRD segment: vector shorter than its count"))?;
            write_values(result, offset, values)?;
        }
        self.index += count;
        self.total_value_count += count;
        Ok(())
    }
}

impl<T: RdFloat> CodecScan for AlpRdScanState<T> {
    fn init(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let bytes = data.bytes();
        let block_size = segment.block_size();
        let metadata = load::<u32>(bytes, 0)? as usize;
        if segment.block_offset() + metadata > block_size {
            return Err(corrupt(
                "Corrupted ALPRD segment: metadata_offset value is corrupted",
            ));
        }
        if segment.block_offset() + 4 + HEADER_SIZE > block_size {
            return Err(corrupt(
                "Corrupted ALPRD segment: reading header bytes would exceed block space",
            ));
        }
        let right_bit_width: u8 = load(bytes, 4)?;
        let left_bit_width: u8 = load(bytes, 5)?;
        let dictionary_size = usize::from(load::<u8>(bytes, 6)?);
        if dictionary_size > MAX_DICTIONARY_SIZE {
            return Err(corrupt(
                "Corrupt database file: ALPRD dictionary size exceeds maximum",
            ));
        }
        if 4 + HEADER_SIZE + dictionary_size * 2 > metadata {
            return Err(corrupt(
                "Corrupted ALPRD segment: actual_dictionary_size is corrupted",
            ));
        }
        let mut dictionary = [0u16; MAX_DICTIONARY_SIZE];
        for (index, entry) in dictionary.iter_mut().enumerate().take(dictionary_size) {
            *entry = load(bytes, HEADER_SIZE + index * 2)?;
        }
        Ok(Self {
            data,
            count: segment.count() as usize,
            metadata,
            total_value_count: 0,
            right_bit_width,
            left_bit_width,
            dictionary,
            decoded: Vec::with_capacity(ALP_VECTOR_SIZE),
            index: 0,
            left: Vec::new(),
            right: Vec::new(),
            _marker: PhantomData,
        })
    }

    /// `AlpRDScanState::Skip`.
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

    /// `AlpRDScanPartial`.
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

/// `AlpRDCompressionFun::GetFunction(type).init_scan`.
pub(crate) fn init_scan(
    segment: &ColumnSegment,
    physical: PhysicalType,
) -> Option<Result<Box<dyn SegmentScan>>> {
    Some(match physical {
        PhysicalType::Float => Sequential::<AlpRdScanState<f32>>::boxed(segment),
        PhysicalType::Double => Sequential::<AlpRdScanState<f64>>::boxed(segment),
        _ => return None,
    })
}

/// `AlpRDConstants::CUTTING_LIMIT`: the left part has at most 16 bits.
const CUTTING_LIMIT: u32 = 16;

/// The dictionary of left parts chosen for a column (`AlpRDCompressionData` from the
/// analysis).
#[derive(Clone, Debug, Default)]
struct Dictionary {
    right_bit_width: u8,
    left_bit_width: u8,
    entries: Vec<u16>,
    /// Left part -> dictionary index (entries past the dictionary only mark exceptions).
    map: HashMap<u16, u16>,
}

/// `BuildLeftPartsDictionary`: the estimated bits per value when cutting at
/// `right_bit_width`, and the dictionary (left parts by descending frequency; DuckDB's
/// unstable sort orders equal frequencies arbitrarily, here by ascending left part).
fn build_left_parts_dictionary(values: &[u64], right_bit_width: u8, persist: Option<&mut Dictionary>) -> f64 {
    let mut counts: HashMap<u64, i32> = HashMap::new();
    for &value in values {
        *counts.entry(value >> right_bit_width).or_insert(0) += 1;
    }
    let mut sorted: Vec<(i32, u64)> = counts.into_iter().map(|(left, count)| (count, left)).collect();
    sorted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let exceptions: u32 = sorted
        .iter()
        .skip(MAX_DICTIONARY_SIZE)
        .map(|(count, _)| *count as u32)
        .sum();
    let dictionary_size = sorted.len().min(MAX_DICTIONARY_SIZE);
    let left_bit_width = 1u8.max((dictionary_size as f64).log2().ceil() as u8);
    if let Some(dictionary) = persist {
        dictionary.entries = sorted[..dictionary_size]
            .iter()
            .map(|(_, left)| *left as u16)
            .collect();
        dictionary.map.clear();
        for (index, &left) in dictionary.entries.iter().enumerate() {
            dictionary.map.entry(left).or_insert(index as u16);
        }
        for (index, (_, left)) in sorted.iter().enumerate().skip(dictionary_size + 1) {
            dictionary.map.entry(*left as u16).or_insert(index as u16);
        }
        dictionary.left_bit_width = left_bit_width;
        dictionary.right_bit_width = right_bit_width;
    }
    // `EstimateCompressionSize` (with the exception count narrowed to 16 bits).
    let exceptions_size = f64::from(u32::from(exceptions as u16) * 32);
    f64::from(right_bit_width) + f64::from(left_bit_width) + exceptions_size / values.len() as f64
}

/// `FindBestDictionary`: the cut with the smallest estimated size.
fn find_best_dictionary(values: &[u64], bits: u32, dictionary: &mut Dictionary) -> f64 {
    let mut right_bit_width = 0u8;
    let mut best = f64::from(i32::MAX);
    for i in 1..=CUTTING_LIMIT {
        let candidate = (bits - i) as u8;
        let estimated = build_left_parts_dictionary(values, candidate, None);
        if estimated <= best {
            right_bit_width = candidate;
            best = estimated;
        }
    }
    build_left_parts_dictionary(values, right_bit_width, Some(dictionary))
}

/// `AlpRDAnalyzeState`.
#[derive(Debug)]
struct AlpRdAnalyzer<T> {
    ty: LogicalType,
    info: CompressionInfo,
    vectors_count: usize,
    total_values: usize,
    vectors_sampled: usize,
    rowgroup_sample: Vec<u64>,
    dictionary: Dictionary,
    _marker: PhantomData<T>,
}

impl<T: RdFloat> Analyzer for AlpRdAnalyzer<T> {
    /// `AlpRDAnalyze`.
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        let skip = must_skip_sampling(self.vectors_count, self.vectors_sampled, count);
        self.vectors_count += 1;
        self.total_values += count;
        if skip {
            return Ok(true);
        }
        let (lookup, increments, sampled) = sampling_parameters(count);
        let mut positions = vec![0u16; lookup];
        let mut sample = Vec::with_capacity(sampled);
        let mut nulls = 0;
        for (value, valid) in unified_values::<T>(input, lookup).step_by(increments) {
            positions[nulls] = sample.len() as u16;
            nulls += usize::from(!valid);
            sample.push(value.to_bits_u64());
        }
        replace_nulls(&mut sample, &positions, sampled, nulls);
        self.rowgroup_sample.extend(sample);
        self.vectors_sampled += 1;
        Ok(true)
    }

    /// `AlpRDFinalAnalyze`.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        if self.total_values == 0 {
            return Ok(None);
        }
        let samples = self.rowgroup_sample.len() as f64;
        let factor_of_sampling = 1.0 / (samples / self.total_values as f64);
        let bits_per_value = find_best_dictionary(&self.rowgroup_sample, T::BITS, &mut self.dictionary);
        let compressed_bytes = bits_per_value * samples / 8.0;
        let per_segment_overhead = (HEADER_SIZE + MAX_DICTIONARY_SIZE * 2) as f64;
        let per_vector_overhead = (4 + 2) as f64;
        let vectors = (self.total_values as f64 / ALP_VECTOR_SIZE as f64).ceil() as u32;
        let estimated_size = compressed_bytes * factor_of_sampling + f64::from(vectors) * per_vector_overhead;
        let blocks = (estimated_size / (self.info.block_size as f64 - per_segment_overhead)).ceil() as u32;
        Ok(Some(
            (estimated_size + f64::from(blocks) * per_segment_overhead) as u64,
        ))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let physical = self.ty.physical_type();
        let dictionary_bytes = self.dictionary.entries.len() * 2;
        Ok(Box::new(AlpRdCompressor::<T> {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            physical,
            info: self.info,
            buffer: vec![0; self.info.block_size],
            count: 0,
            data_bytes_used: 0,
            metadata_offset: self.info.block_size,
            next_vector_start: (HEADER_SIZE + dictionary_bytes) as u32,
            dictionary_bytes,
            dictionary: self.dictionary,
            input: Vec::with_capacity(ALP_VECTOR_SIZE),
            null_positions: vec![0; ALP_VECTOR_SIZE],
            nulls: 0,
            _marker: PhantomData,
        }))
    }
}

/// `AlpRDCompressionState`.
#[derive(Debug)]
struct AlpRdCompressor<T> {
    ty: LogicalType,
    physical: PhysicalType,
    info: CompressionInfo,
    buffer: Vec<u8>,
    count: u64,
    stats: BaseStatistics,
    data_bytes_used: usize,
    metadata_offset: usize,
    next_vector_start: u32,
    dictionary_bytes: usize,
    dictionary: Dictionary,
    /// The vector's values as bits.
    input: Vec<u64>,
    null_positions: Vec<u16>,
    nulls: usize,
    _marker: PhantomData<T>,
}

impl<T: RdFloat> AlpRdCompressor<T> {
    fn used_space(&self) -> usize {
        HEADER_SIZE + self.dictionary_bytes + self.data_bytes_used
    }

    fn has_enough_space(&self, vector_size: usize) -> bool {
        align8(self.used_space() + vector_size) < self.metadata_offset - 4
    }

    fn create_empty_segment(&mut self) {
        self.buffer.fill(0);
        self.count = 0;
        self.stats = BaseStatistics::create_empty(self.ty.clone());
        self.metadata_offset = self.info.block_size;
        self.next_vector_start = (HEADER_SIZE + self.dictionary_bytes) as u32;
    }

    /// `CompressVector` with `AlpRDCompression::Compress`.
    fn compress_vector(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let n = self.input.len();
        replace_nulls(&mut self.input, &self.null_positions, n, self.nulls);
        let dictionary = &self.dictionary;
        let right_bit_width = u32::from(dictionary.right_bit_width);
        let dictionary_size = dictionary.entries.len() as u16;
        let mut rights = Vec::with_capacity(n);
        let mut lefts = Vec::with_capacity(n);
        let mut exceptions = Vec::new();
        for (position, &bits) in self.input.iter().enumerate() {
            rights.push(bits & ((1u64 << right_bit_width) - 1));
            let left = (bits >> right_bit_width) as u16;
            let index = dictionary.map.get(&left).copied().unwrap_or(dictionary_size);
            lefts.push(u64::from(index));
            if index >= dictionary_size {
                exceptions.push((left, position as u16));
            }
        }
        let left_size = required_size(n, dictionary.left_bit_width);
        let right_size = required_size(n, dictionary.right_bit_width);
        let compressed_size = left_size + right_size + exceptions.len() * 4 + 2;
        let uncompressed_size = 2 + (T::BITS as usize / 8) * n;
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
            for &bits in &self.input {
                update_numeric_stats(&mut self.stats, self.physical, T::from_bits_u64(bits));
            }
        }
        self.count += n as u64;
        let mut offset = self.used_space();
        let value_size = T::BITS as usize / 8;
        if should_compress {
            self.buffer[offset..offset + 2].copy_from_slice(&(exceptions.len() as u16).to_le_bytes());
            offset += 2;
            pack_buffer_u64(
                &lefts,
                self.dictionary.left_bit_width,
                &mut self.buffer[offset..offset + left_size],
            );
            offset += left_size;
            pack_buffer_u64(
                &rights,
                self.dictionary.right_bit_width,
                &mut self.buffer[offset..offset + right_size],
            );
            offset += right_size;
            for (left, _) in &exceptions {
                self.buffer[offset..offset + 2].copy_from_slice(&left.to_le_bytes());
                offset += 2;
            }
            for (_, position) in &exceptions {
                self.buffer[offset..offset + 2].copy_from_slice(&position.to_le_bytes());
                offset += 2;
            }
        } else {
            self.buffer[offset..offset + 2].copy_from_slice(&UNCOMPRESSED_MODE_SENTINEL.to_le_bytes());
            offset += 2;
            for &bits in &self.input {
                self.buffer[offset..offset + value_size].copy_from_slice(&bits.to_le_bytes()[..value_size]);
                offset += value_size;
            }
        }
        self.data_bytes_used += vector_size;
        self.metadata_offset -= 4;
        self.buffer[self.metadata_offset..self.metadata_offset + 4]
            .copy_from_slice(&self.next_vector_start.to_le_bytes());
        self.next_vector_start = self.used_space() as u32;
        self.input.clear();
        self.nulls = 0;
        Ok(())
    }

    /// `FlushSegment`: compacts the vector offsets when the block is less than 80% used and
    /// writes the segment header with the dictionary.
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
        self.buffer[4] = self.dictionary.right_bit_width;
        self.buffer[5] = self.dictionary.left_bit_width;
        self.buffer[6] = self.dictionary.entries.len() as u8;
        for (index, entry) in self.dictionary.entries.iter().enumerate() {
            self.buffer[HEADER_SIZE + 2 * index..HEADER_SIZE + 2 * index + 2]
                .copy_from_slice(&entry.to_le_bytes());
        }
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        sink.flush(FinishedSegment {
            data: self.buffer[..total].to_vec(),
            count: self.count,
            stats,
            compression: CompressionType::AlpRd,
            overflow_blocks: Vec::new(),
        })?;
        self.data_bytes_used = 0;
        Ok(())
    }
}

impl<T: RdFloat> Compressor for AlpRdCompressor<T> {
    /// `AlpRDCompressionState::Append`.
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        for (value, valid) in unified_values::<T>(input, count) {
            let index = self.input.len();
            self.null_positions[self.nulls] = index as u16;
            self.nulls += usize::from(!valid);
            self.input.push(value.to_bits_u64());
            if self.input.len() == ALP_VECTOR_SIZE {
                self.compress_vector(sink)?;
            }
        }
        Ok(())
    }

    /// `AlpRDCompressionState::Finalize`.
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        if !self.input.is_empty() {
            self.compress_vector(sink)?;
        }
        self.flush_segment(sink)
    }
}

fn analyzer<T: RdFloat>(ty: &LogicalType, info: &CompressionInfo) -> Box<dyn Analyzer> {
    Box::new(AlpRdAnalyzer::<T> {
        ty: ty.clone(),
        info: *info,
        vectors_count: 0,
        total_values: 0,
        vectors_sampled: 0,
        rowgroup_sample: Vec::new(),
        dictionary: Dictionary::default(),
        _marker: PhantomData,
    })
}

/// `AlpRDCompressionFun::GetFunction(type).init_analyze`.
pub(crate) fn init_analyze(
    ty: &LogicalType,
    physical: PhysicalType,
    info: &CompressionInfo,
) -> Option<Box<dyn Analyzer>> {
    if !super::alp::supports_block_size(info) {
        return None;
    }
    match physical {
        PhysicalType::Float => Some(analyzer::<f32>(ty, info)),
        PhysicalType::Double => Some(analyzer::<f64>(ty, info)),
        _ => None,
    }
}
