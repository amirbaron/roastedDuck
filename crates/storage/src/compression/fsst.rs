//! FSST compression of strings (DuckDB's `fsst.cpp`; written by storage versions before
//! `DICT_FSST`).
//!
//! Segment layout: a header (`dict_size`, `dict_end`, `bitpacking_width`,
//! `fsst_symbol_table_offset`, all `u32`), the bit-packed compressed length of every row,
//! the serialized symbol table, and the compressed strings growing backwards from
//! `dict_end`. Row `i`'s string ends `sum(lengths[0..=i])` bytes before `dict_end`.

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, StringT, Vector};

use super::bitpacking_primitives::{
    GROUP_SIZE, minimum_bit_width_unsigned, pack_buffer_u64, required_size, round_up_to_group,
    unpack_buffer_u32,
};
use super::libfsst::{FSST_DECODER_SIZE, FsstDecoder, FsstEncoder};
use super::primitive::{corrupt, load, unified_strings, update_string_stats};
use super::uncompressed::string_block_limit;
use super::{Analyzer, CompressionInfo, CompressionType, Compressor, SegmentScan};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;
use crate::table_filter::TableFilter;
use rustdb_types::compare::CompareOp;

/// The size of `fsst_compression_header_t`.
pub(crate) const FSST_HEADER_SIZE: usize = 16;

/// The scan state of an FSST segment (DuckDB's `FSSTScanState`).
#[derive(Debug)]
pub(crate) struct FsstScanState {
    data: PersistentData,
    dict_end: usize,
    width: u8,
    /// `None` when the segment holds only empty strings and NULLs.
    decoder: Option<FsstDecoder>,
    /// The last row whose dictionary offset is known and that offset
    /// (`last_known_row` / `last_known_index`).
    last_known: Option<(usize, u32)>,
    lengths: Vec<u32>,
}

impl FsstScanState {
    /// `StringInitScan` (`ParseFSSTSegmentHeader`).
    pub(crate) fn new(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let bytes = data.bytes();
        let dict_end = load::<u32>(bytes, 4)? as usize;
        let width = load::<u32>(bytes, 8)? as u8;
        let symbol_table_offset = load::<u32>(bytes, 12)? as usize;
        if symbol_table_offset > segment.block_size() {
            return Err(corrupt(
                "invalid fsst_symbol_table_offset in FSSTStorage::ParseFSSTSegmentHeader",
            ));
        }
        let decoder = bytes
            .get(symbol_table_offset..)
            .and_then(FsstDecoder::import)
            .map(|(decoder, _)| decoder);
        Ok(Self {
            data,
            dict_end,
            width,
            decoder,
            last_known: None,
            lengths: Vec::new(),
        })
    }
}

impl SegmentScan for FsstScanState {
    /// `StringScanPartial` (`StartScan`, `DecompressString`, `EndScan`).
    fn scan(
        &mut self,
        _segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        self.scan_rows(start, count, result, result_offset, None)
    }

    /// `FSSTStorage::Select`: only the selected strings are decompressed.
    fn select(
        &mut self,
        _segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        sel: &[u32],
    ) -> Result<bool> {
        self.scan_rows(start, count, result, 0, Some(sel))?;
        Ok(true)
    }

    /// `= ''` and `<> ''` are decided by the compressed lengths alone (an empty string,
    /// and only an empty string, has no codes), so only the selected strings are decoded.
    /// rustdb-specific: DuckDB decompresses every string to compare it.
    fn filter(
        &mut self,
        _segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        sel: &mut Vec<u32>,
        filter: &TableFilter,
        needs_values: bool,
    ) -> Result<bool> {
        let TableFilter::ConstantComparison { op, constant } = filter else {
            return Ok(false);
        };
        let want_empty = match op {
            CompareOp::Equal => true,
            CompareOp::NotEqual => false,
            _ => return Ok(false),
        };
        if sel.len() != count || constant.as_bytes() != Some(&b""[..]) {
            return Ok(false);
        }
        let alignment = start % GROUP_SIZE;
        let total = round_up_to_group(alignment + count);
        let mut lengths = std::mem::take(&mut self.lengths);
        lengths.resize(total, 0);
        let src_offset = FSST_HEADER_SIZE + (start - alignment) * self.width as usize / 8;
        let src = self.data.bytes().get(src_offset..).unwrap_or(&[]);
        unpack_buffer_u32(src, total, self.width, &mut lengths);
        sel.clear();
        sel.extend(
            lengths[alignment..alignment + count]
                .iter()
                .enumerate()
                .filter(|&(_, &length)| (length == 0) == want_empty)
                .map(|(row, _)| row as u32),
        );
        self.lengths = lengths;
        if !needs_values {
            // The strings themselves are not needed: nothing to decode.
            return Ok(true);
        }
        let selected = std::mem::take(sel);
        let scanned = self.scan_rows(start, count, result, 0, Some(&selected));
        *sel = selected;
        scanned.map(|()| true)
    }
}

impl FsstScanState {
    /// Scans rows `start..start + count` into `result` from `result_offset`; with `wanted`
    /// (ascending rows relative to `start`) the other rows are left empty.
    fn scan_rows(
        &mut self,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
        wanted: Option<&[u32]>,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        if start == 0 || self.last_known.is_some_and(|(row, _)| row >= start) {
            self.last_known = None;
        }
        // `CalculateBpDeltaOffsets`.
        let (delta_decode_start, mut offset) =
            self.last_known.map_or((0, 0), |(row, offset)| (row + 1, offset));
        let alignment = delta_decode_start % GROUP_SIZE;
        let bitunpack_start = delta_decode_start - alignment;
        let total = round_up_to_group(start + count - bitunpack_start);
        self.lengths.resize(total, 0);
        let src_offset = FSST_HEADER_SIZE + bitunpack_start * self.width as usize / 8;
        let src = self.data.bytes().get(src_offset..).unwrap_or(&[]);
        unpack_buffer_u32(src, total, self.width, &mut self.lengths);
        // Delta decode the dictionary offsets up to the first scanned row.
        for &length in &self.lengths[alignment..start - bitunpack_start] {
            offset = offset.wrapping_add(length);
        }
        let scanned = &self.lengths[start - bitunpack_start..start - bitunpack_start + count];
        let selected_bytes: usize = match wanted {
            None => scanned.iter().map(|&length| length as usize).sum(),
            Some(rows) => rows.iter().map(|&row| scanned[row as usize] as usize).sum(),
        };
        // Each compressed byte decodes to at most 8 bytes, and the last decoded word may
        // reach 8 bytes further: with this capacity `decoded` never reallocates, so the
        // strings pointing into it stay valid.
        let mut decoded: Vec<u8> = Vec::with_capacity(selected_bytes * 8 + 8);
        let target = result
            .values_mut::<StringT>()
            .get_mut(result_offset..result_offset + count)
            .ok_or_else(|| Error::internal("scan target vector too small"))?;
        let mut wanted = wanted.map(|rows| rows.iter().peekable());
        for (row, (&length, slot)) in scanned.iter().zip(target.iter_mut()).enumerate() {
            offset = offset.wrapping_add(length);
            let selected = match &mut wanted {
                None => true,
                Some(rows) => rows.next_if(|&&next| next as usize == row).is_some(),
            };
            if length == 0 || !selected {
                *slot = StringT::default();
                continue;
            }
            let decoder = self
                .decoder
                .as_ref()
                .ok_or_else(|| corrupt("FSST segment with strings but without a symbol table"))?;
            let position = self
                .dict_end
                .checked_sub(offset as usize)
                .ok_or_else(|| corrupt("FSST string offset out of range - corrupt database file"))?;
            let compressed = self
                .data
                .bytes()
                .get(position..position + length as usize)
                .ok_or_else(|| corrupt("FSST string out of range - corrupt database file"))?;
            let begin = decoded.len();
            // SAFETY: `decoded` has room for `compressed.len() * 8 + 8` more bytes: its
            // capacity covers 8 bytes per compressed byte of every selected string, plus 8.
            let length = unsafe { decoder.decompress_to(compressed, decoded.as_mut_ptr().add(begin)) };
            // SAFETY: `decompress_to` initialised those `length` bytes.
            unsafe { decoded.set_len(begin + length) };
            let string = &decoded[begin..];
            if string.len() <= StringT::INLINE_LENGTH {
                *slot = StringT::inline(string);
                // Inlined: the bytes are not needed any more.
                decoded.truncate(begin);
            } else {
                // SAFETY: `decoded` keeps its allocation (see its capacity above) and is
                // kept alive by `result` below.
                *slot = unsafe { StringT::from_raw(string) };
            }
        }
        self.last_known = Some((start + count - 1, offset));
        if !decoded.is_empty() {
            let keep_alive: Arc<dyn std::any::Any + Send + Sync> = Arc::new(decoded);
            result.add_keep_alive(keep_alive);
        }
        Ok(())
    }
}

/// `FSSTStorage::MINIMUM_COMPRESSION_RATIO`.
const MINIMUM_COMPRESSION_RATIO: f64 = 1.2;
/// `FSSTStorage::ANALYSIS_SAMPLE_SIZE`: the share of vectors sampled.
const ANALYSIS_SAMPLE_SIZE: f64 = 0.25;

/// A PCG32 generator (DuckDB's `RandomEngine`, which DuckDB seeds randomly; a fixed seed
/// keeps rustdb's sampling reproducible).
#[derive(Debug)]
pub(crate) struct RandomEngine {
    state: u64,
}

impl RandomEngine {
    const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
    const INCREMENT: u64 = 1_442_695_040_888_963_407;

    pub(crate) fn new(seed: u64) -> Self {
        let mut engine = Self { state: 0 };
        engine.next_u32();
        engine.state = engine.state.wrapping_add(seed);
        engine.next_u32();
        engine
    }

    fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(Self::MULTIPLIER).wrapping_add(Self::INCREMENT);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rotation = (old >> 59) as u32;
        xorshifted.rotate_right(rotation)
    }

    /// `NextRandom`: a value in `[0, 1)`.
    pub(crate) fn next_random(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }
}

/// `MinimumBitWidth(size_t)`.
fn bit_width(value: usize) -> u8 {
    minimum_bit_width_unsigned(value as u128, 64)
}

/// `FSSTAnalyzeState`: a sample of the strings to build a symbol table from.
#[derive(Debug)]
struct FsstAnalyzer {
    ty: LogicalType,
    info: CompressionInfo,
    count: usize,
    strings: Vec<Vec<u8>>,
    total_size: usize,
    random: RandomEngine,
    have_valid_row: bool,
    empty_strings: usize,
    encoder: Option<FsstEncoder>,
}

impl Analyzer for FsstAnalyzer {
    /// `FSSTStorage::StringAnalyze`.
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        self.count += count;
        let sample_selected = !self.have_valid_row || self.random.next_random() < ANALYSIS_SAMPLE_SIZE;
        let limit = string_block_limit(self.info.block_size);
        for string in unified_strings(input, count).flatten() {
            if string.len() >= limit {
                return Ok(false);
            }
            if !sample_selected {
                continue;
            }
            if string.is_empty() {
                self.empty_strings += 1;
            } else {
                self.have_valid_row = true;
                self.total_size += string.len();
                self.strings.push(string.to_vec());
            }
        }
        Ok(true)
    }

    /// `FSSTStorage::StringFinalAnalyze`: compresses the sample to estimate the size.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        if self.strings.is_empty() {
            return Ok(None);
        }
        let strings: Vec<&[u8]> = self.strings.iter().map(Vec::as_slice).collect();
        let encoder = FsstEncoder::create(&strings);
        let compressed = encoder.compress(&strings);
        self.encoder = Some(encoder);
        let compressed_size: usize = compressed.iter().map(Vec::len).sum();
        let max_length = compressed.iter().map(Vec::len).max().unwrap_or(0);
        let width = bit_width(max_length);
        let bitpacked = required_size(strings.len() + self.empty_strings, width);
        let estimated_base = (bitpacked + compressed_size) as f64 * (1.0 / ANALYSIS_SAMPLE_SIZE);
        let blocks = estimated_base / (self.info.block_size - FSST_DECODER_SIZE) as f64;
        let estimated = estimated_base + blocks * FSST_DECODER_SIZE as f64;
        Ok(Some((estimated * MINIMUM_COMPRESSION_RATIO) as u64))
    }

    /// `FSSTStorage::InitCompression`.
    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let encoder = self
            .encoder
            .ok_or_else(|| Error::internal("No encoder found during FSST compression"))?;
        let symbol_table = encoder.export();
        let mut compressor = FsstCompressor {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            info: self.info,
            encoder,
            symbol_table,
            buffer: Vec::new(),
            count: 0,
            dict_size: 0,
            index_buffer: Vec::new(),
            max_compressed_length: 0,
            current_width: 0,
            last_fitting_size: 0,
        };
        compressor.create_empty_segment();
        Ok(Box::new(compressor))
    }
}

/// `FSSTCompressionState`: compressed strings grow backwards from the end of the block;
/// their lengths and the symbol table are written when the segment is finished.
#[derive(Debug)]
struct FsstCompressor {
    ty: LogicalType,
    info: CompressionInfo,
    encoder: FsstEncoder,
    symbol_table: Vec<u8>,
    buffer: Vec<u8>,
    count: u64,
    stats: BaseStatistics,
    dict_size: usize,
    index_buffer: Vec<u64>,
    max_compressed_length: usize,
    current_width: u8,
    last_fitting_size: usize,
}

impl FsstCompressor {
    /// `CreateEmptySegment` + `Reset`.
    fn create_empty_segment(&mut self) {
        self.buffer = vec![0; self.info.block_size];
        self.count = 0;
        self.stats = BaseStatistics::create_empty(self.ty.clone());
        self.dict_size = 0;
        self.index_buffer.clear();
        self.current_width = 0;
        self.max_compressed_length = 0;
        self.last_fitting_size = 0;
    }

    /// `GetRequiredSize`.
    fn required_size(&self, length: usize) -> usize {
        let width = if length > self.max_compressed_length {
            bit_width(length)
        } else {
            self.current_width
        };
        FSST_HEADER_SIZE
            + self.dict_size
            + required_size(self.index_buffer.len() + 1, width)
            + length
            + self.symbol_table.len()
    }

    /// `HasEnoughSpace`.
    fn has_enough_space(&mut self, length: usize) -> bool {
        let required = self.required_size(length);
        if required <= self.info.block_size {
            self.last_fitting_size = required;
            return true;
        }
        false
    }

    /// Makes room for a string of `length` compressed bytes, flushing a full segment.
    fn reserve(&mut self, sink: &mut SegmentSink<'_>, length: usize) -> Result<()> {
        if !self.has_enough_space(length) {
            self.flush(sink, false)?;
            if !self.has_enough_space(length) {
                return Err(Error::internal(
                    "FSST string compression failed due to insufficient space in empty block",
                ));
            }
        }
        Ok(())
    }

    /// `UpdateState`.
    fn add_string(&mut self, sink: &mut SegmentSink<'_>, string: &[u8], compressed: &[u8]) -> Result<()> {
        self.reserve(sink, compressed.len())?;
        update_string_stats(&mut self.stats, string);
        self.dict_size += compressed.len();
        let position = self.info.block_size - self.dict_size;
        self.buffer[position..position + compressed.len()].copy_from_slice(compressed);
        self.index_buffer.push(compressed.len() as u64);
        self.max_compressed_length = self.max_compressed_length.max(compressed.len());
        self.current_width = bit_width(self.max_compressed_length);
        self.count += 1;
        Ok(())
    }

    /// `AddNull` / `AddEmptyString`.
    fn add_empty(&mut self, sink: &mut SegmentSink<'_>, null: bool) -> Result<()> {
        self.reserve(sink, 0)?;
        self.index_buffer.push(0);
        self.count += 1;
        if null {
            self.stats.set_has_null_fast();
        } else {
            update_string_stats(&mut self.stats, b"");
        }
        Ok(())
    }

    /// `Flush` (with `Finalize`).
    fn flush(&mut self, sink: &mut SegmentSink<'_>, last: bool) -> Result<()> {
        let block_size = self.info.block_size;
        let index_size = required_size(self.count as usize, self.current_width);
        let total = FSST_HEADER_SIZE + index_size + self.dict_size + self.symbol_table.len();
        if total != self.last_fitting_size {
            return Err(Error::internal(
                "FSST string compression failed due to incorrect size calculation",
            ));
        }
        let symbol_table_offset = FSST_HEADER_SIZE + index_size;
        pack_buffer_u64(
            &self.index_buffer,
            self.current_width,
            &mut self.buffer[FSST_HEADER_SIZE..symbol_table_offset],
        );
        self.buffer[symbol_table_offset..symbol_table_offset + self.symbol_table.len()]
            .copy_from_slice(&self.symbol_table);
        self.buffer[8..12].copy_from_slice(&u32::from(self.current_width).to_le_bytes());
        self.buffer[12..16].copy_from_slice(&(symbol_table_offset as u32).to_le_bytes());
        // The dictionary header is only rewritten when the dictionary moves.
        let (dict_size, dict_end, segment_size) = if total >= self.info.compaction_flush_limit() {
            (0, block_size, block_size)
        } else {
            let new_offset = symbol_table_offset + self.symbol_table.len();
            self.buffer
                .copy_within(block_size - self.dict_size..block_size, new_offset);
            (self.dict_size, total, total)
        };
        self.buffer[..4].copy_from_slice(&(dict_size as u32).to_le_bytes());
        self.buffer[4..8].copy_from_slice(&(dict_end as u32).to_le_bytes());
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        sink.flush(FinishedSegment {
            data: self.buffer[..segment_size].to_vec(),
            count: self.count,
            stats,
            compression: CompressionType::Fsst,
            overflow_blocks: Vec::new(),
        })?;
        if !last {
            self.create_empty_segment();
        }
        Ok(())
    }
}

impl Compressor for FsstCompressor {
    /// `FSSTStorage::Compress`: the non-empty strings of the vector are compressed at once.
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        let strings: Vec<Option<&[u8]>> = unified_strings(input, count).collect();
        let to_compress: Vec<&[u8]> = strings
            .iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .copied()
            .collect();
        let compressed = self.encoder.compress(&to_compress);
        let mut next = compressed.iter();
        for string in strings {
            match string {
                None => self.add_empty(sink, true)?,
                Some([]) => self.add_empty(sink, false)?,
                Some(bytes) => {
                    let encoded = next
                        .next()
                        .ok_or_else(|| Error::internal("FSST compression failed to compress all strings"))?;
                    self.add_string(sink, bytes, encoded)?;
                }
            }
        }
        Ok(())
    }

    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        self.flush(sink, true)
    }
}

/// `FSSTFun::GetFunction(type).init_analyze`: only before `DICT_FSST` (storage version 5).
pub(crate) fn init_analyze(ty: &LogicalType, info: &CompressionInfo) -> Option<Box<dyn Analyzer>> {
    if info.storage_version >= 5 {
        return None;
    }
    Some(Box::new(FsstAnalyzer {
        ty: ty.clone(),
        info: *info,
        count: 0,
        strings: Vec::new(),
        total_size: 0,
        random: RandomEngine::new(0),
        have_valid_row: false,
        empty_strings: 0,
        encoder: None,
    }))
}
