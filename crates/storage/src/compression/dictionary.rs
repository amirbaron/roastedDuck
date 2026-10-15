//! Dictionary compression of strings (DuckDB's `dictionary_compression.cpp` and
//! `compression/dictionary/*`; written by storage versions before `DICT_FSST`).
//!
//! Segment layout: a header (`dict_size`, `dict_end`, `index_buffer_offset`,
//! `index_buffer_count`, `bitpacking_width`, all `u32`), the bit-packed selection buffer
//! (row -> dictionary index, index 0 = NULL), the index buffer (`u32` cumulative dictionary
//! sizes per index) and the dictionary, whose strings grow backwards from `dict_end`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Vector};

use super::bitpacking_primitives::{
    GROUP_SIZE, minimum_bit_width_unsigned, pack_buffer_u64, required_size, round_up_to_group,
    unpack_buffer_u32,
};
use super::primitive::{corrupt, load, unified_strings, update_string_stats, write_string_ranges};
use super::uncompressed::string_block_limit;
use super::{Analyzer, CompressionInfo, CompressionType, Compressor, SegmentScan};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

/// The size of `dictionary_compression_header_t` (`DICTIONARY_HEADER_SIZE`).
pub(crate) const DICTIONARY_HEADER_SIZE: usize = 20;

/// The scan state of a dictionary segment (DuckDB's `CompressedStringScanState`).
#[derive(Debug)]
pub(crate) struct DictionaryScanState {
    data: Arc<PersistentData>,
    /// `dict.end`.
    dict_end: usize,
    /// The cumulative dictionary size of every index.
    index_buffer: Vec<u32>,
    width: u8,
    /// Unpacked selection values (`sel_vec`).
    selection: Vec<u32>,
}

impl DictionaryScanState {
    /// `CompressedStringScanState::Initialize`.
    pub(crate) fn new(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let bytes = data.bytes();
        let dict_end = load::<u32>(bytes, 4)? as usize;
        let index_buffer_offset = load::<u32>(bytes, 8)? as usize;
        let index_buffer_count = load::<u32>(bytes, 12)? as usize;
        let width = load::<u32>(bytes, 16)? as u8;
        let block_size = segment.block_size();
        if segment.block_offset() + index_buffer_offset + 4 * index_buffer_count > block_size {
            return Err(corrupt(
                "Failed to scan dictionary string - index was out of range. Database file appears to be corrupted.",
            ));
        }
        if segment.block_offset() + dict_end > block_size {
            return Err(corrupt(
                "Failed to scan dictionary string - dictionary end out of range. Database file appears to be \
                 corrupted.",
            ));
        }
        let index_buffer = (0..index_buffer_count)
            .map(|index| load::<u32>(bytes, index_buffer_offset + index * 4))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            data: Arc::new(data),
            dict_end,
            index_buffer,
            width,
            selection: Vec::new(),
        })
    }

    /// The byte range of dictionary string `index` (`GetStringLength` +
    /// `FetchStringFromDict`).
    fn string_range(&self, index: usize) -> Result<std::ops::Range<usize>> {
        let out_of_range = || {
            corrupt(
                "Failed to scan dictionary string - offset out of range. Database file appears to be corrupted.",
            )
        };
        let dict_offset = *self.index_buffer.get(index).ok_or_else(out_of_range)? as usize;
        let length = if index == 0 {
            0
        } else {
            usize::from(dict_offset.wrapping_sub(self.index_buffer[index - 1] as usize) as u16)
        };
        if dict_offset > self.dict_end || length > dict_offset {
            return Err(out_of_range());
        }
        if dict_offset == 0 {
            return Ok(0..0);
        }
        let start = self.dict_end - dict_offset;
        Ok(start..start + length)
    }
}

impl SegmentScan for DictionaryScanState {
    /// `ScanToFlatVector`.
    fn scan(
        &mut self,
        _segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let start_offset = start % GROUP_SIZE;
        let decompress_count = round_up_to_group(count + start_offset);
        self.selection.resize(decompress_count, 0);
        let src_offset = DICTIONARY_HEADER_SIZE + (start - start_offset) * self.width as usize / 8;
        let src = self.data.bytes().get(src_offset..).unwrap_or(&[]);
        unpack_buffer_u32(src, decompress_count, self.width, &mut self.selection);
        let ranges = (0..count)
            .map(|row| self.string_range(self.selection[row + start_offset] as usize))
            .collect::<Result<Vec<_>>>()?;
        write_string_ranges(result, result_offset, &self.data, ranges)
    }
}

/// `DictionaryCompression::MINIMUM_COMPRESSION_RATIO`.
const MINIMUM_COMPRESSION_RATIO: f32 = 1.2;

/// `DictionaryCompression::RequiredSpace`.
fn required_space(count: usize, index_count: usize, dict_size: usize, width: u8) -> usize {
    DICTIONARY_HEADER_SIZE + dict_size + required_size(count, width) + index_count * 4
}

/// `MinimumBitWidth(idx_t)`.
fn bit_width(value: usize) -> u8 {
    minimum_bit_width_unsigned(value as u128, 64)
}

/// The space accounting shared by the analysis and the compression
/// (`DictionaryCompressionState::UpdateState`): every row is checked to fit the segment
/// (as a NULL, a known string or a new one); a full segment is flushed first.
trait DictionaryState {
    fn block_size(&self) -> usize;
    fn lookup(&mut self, string: &[u8]) -> bool;
    fn calculate_space_requirements(&mut self, new_string: bool, size: usize) -> bool;
    fn flush(&mut self, sink: Option<&mut SegmentSink<'_>>) -> Result<()>;
    fn add_null(&mut self);
    fn add_new_string(&mut self, string: &[u8]);
    fn add_last_lookup(&mut self, string: &[u8]);

    /// `UpdateState`; `false` when a string is too big for dictionary compression.
    fn update_state(
        &mut self,
        mut sink: Option<&mut SegmentSink<'_>>,
        input: &Vector,
        count: usize,
    ) -> Result<bool> {
        let limit = string_block_limit(self.block_size());
        for string in unified_strings(input, count) {
            let (size, mut new_string) = match string {
                Some(bytes) => {
                    if bytes.len() >= limit {
                        return Ok(false);
                    }
                    (bytes.len(), !self.lookup(bytes))
                }
                None => (0, false),
            };
            if !self.calculate_space_requirements(new_string, size) {
                self.flush(sink.as_deref_mut())?;
                new_string = true;
                if !self.calculate_space_requirements(new_string, size) {
                    return Err(Error::internal(
                        "Dictionary compression could not write to new segment",
                    ));
                }
            }
            match string {
                None => self.add_null(),
                Some(bytes) if new_string => self.add_new_string(bytes),
                Some(bytes) => self.add_last_lookup(bytes),
            }
        }
        Ok(true)
    }
}

/// `DictionaryAnalyzeState`.
#[derive(Debug)]
struct DictionaryAnalyzer {
    ty: LogicalType,
    info: CompressionInfo,
    segment_count: usize,
    tuple_count: usize,
    unique_count: usize,
    max_unique_count: usize,
    dict_size: usize,
    set: HashSet<Vec<u8>>,
    current_width: u8,
    next_width: u8,
}

impl DictionaryState for DictionaryAnalyzer {
    fn block_size(&self) -> usize {
        self.info.block_size
    }

    fn lookup(&mut self, string: &[u8]) -> bool {
        self.set.contains(string)
    }

    fn calculate_space_requirements(&mut self, new_string: bool, size: usize) -> bool {
        if !new_string {
            return required_space(
                self.tuple_count + 1,
                self.unique_count,
                self.dict_size,
                self.current_width,
            ) <= self.info.block_size;
        }
        self.next_width = bit_width(self.unique_count + 2);
        required_space(
            self.tuple_count + 1,
            self.unique_count + 1,
            self.dict_size + size,
            self.next_width,
        ) <= self.info.block_size
    }

    fn flush(&mut self, _sink: Option<&mut SegmentSink<'_>>) -> Result<()> {
        self.segment_count += 1;
        self.tuple_count = 0;
        self.max_unique_count = self.max_unique_count.max(self.unique_count);
        self.unique_count = 0;
        self.dict_size = 0;
        self.set.clear();
        Ok(())
    }

    fn add_null(&mut self) {
        self.tuple_count += 1;
    }

    fn add_new_string(&mut self, string: &[u8]) {
        self.tuple_count += 1;
        self.unique_count += 1;
        self.dict_size += string.len();
        self.set.insert(string.to_vec());
        self.current_width = self.next_width;
    }

    fn add_last_lookup(&mut self, _string: &[u8]) {
        self.tuple_count += 1;
    }
}

impl Analyzer for DictionaryAnalyzer {
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        self.update_state(None, input, count)
    }

    /// `StringFinalAnalyze`.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        if self.tuple_count != 0 {
            self.max_unique_count = self.max_unique_count.max(self.unique_count);
        }
        let width = bit_width(self.unique_count + 1);
        let required = required_space(self.tuple_count, self.unique_count, self.dict_size, width);
        let total = self.segment_count * self.info.block_size + required;
        Ok(Some((MINIMUM_COMPRESSION_RATIO * total as f32) as u64))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let mut compressor = DictionaryCompressor {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            info: self.info,
            buffer: Vec::new(),
            count: 0,
            dict_size: 0,
            map: HashMap::new(),
            index_buffer: Vec::new(),
            selection_buffer: Vec::new(),
            current_width: 0,
            next_width: 0,
            latest_lookup: 0,
            finished: Vec::new(),
        };
        compressor.create_empty_segment();
        Ok(Box::new(compressor))
    }
}

/// `DictionaryCompressionCompressState`: new strings are written backwards from the end of
/// the block, the selection and index buffers are written when the segment is finished.
#[derive(Debug)]
struct DictionaryCompressor {
    ty: LogicalType,
    info: CompressionInfo,
    buffer: Vec<u8>,
    count: u64,
    stats: BaseStatistics,
    dict_size: usize,
    /// String -> its index in the index buffer minus one.
    map: HashMap<Vec<u8>, u32>,
    index_buffer: Vec<u32>,
    selection_buffer: Vec<u64>,
    current_width: u8,
    next_width: u8,
    latest_lookup: u32,
    /// Segments finished while appending, to be handed to the sink.
    finished: Vec<FinishedSegment>,
}

impl DictionaryCompressor {
    /// `CreateEmptySegment`.
    fn create_empty_segment(&mut self) {
        self.buffer = vec![0; self.info.block_size];
        self.count = 0;
        self.stats = BaseStatistics::create_empty(self.ty.clone());
        self.dict_size = 0;
        self.map.clear();
        self.index_buffer.clear();
        self.index_buffer.push(0);
        self.selection_buffer.clear();
        self.current_width = 0;
        self.next_width = 0;
    }

    /// `Finalize`: writes the selection and index buffers and the header; compacts the
    /// dictionary next to them when the segment is not full enough.
    fn finalize_segment(&mut self) -> FinishedSegment {
        let block_size = self.info.block_size;
        let selection_size = required_size(self.count as usize, self.current_width);
        let index_size = self.index_buffer.len() * 4;
        let total = DICTIONARY_HEADER_SIZE + selection_size + index_size + self.dict_size;
        let index_offset = DICTIONARY_HEADER_SIZE + selection_size;
        pack_buffer_u64(
            &self.selection_buffer,
            self.current_width,
            &mut self.buffer[DICTIONARY_HEADER_SIZE..index_offset],
        );
        for (index, value) in self.index_buffer.iter().enumerate() {
            self.buffer[index_offset + index * 4..index_offset + index * 4 + 4]
                .copy_from_slice(&value.to_le_bytes());
        }
        self.buffer[8..12].copy_from_slice(&(index_offset as u32).to_le_bytes());
        self.buffer[12..16].copy_from_slice(&(self.index_buffer.len() as u32).to_le_bytes());
        self.buffer[16..20].copy_from_slice(&u32::from(self.current_width).to_le_bytes());
        let mut dict_end = block_size;
        let segment_size = if total >= self.info.compaction_flush_limit() {
            block_size
        } else {
            let new_offset = index_offset + index_size;
            self.buffer
                .copy_within(block_size - self.dict_size..block_size, new_offset);
            dict_end -= block_size - total;
            total
        };
        self.buffer[..4].copy_from_slice(&(self.dict_size as u32).to_le_bytes());
        self.buffer[4..8].copy_from_slice(&(dict_end as u32).to_le_bytes());
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        FinishedSegment {
            data: self.buffer[..segment_size].to_vec(),
            count: self.count,
            stats,
            compression: CompressionType::Dictionary,
            overflow_blocks: Vec::new(),
        }
    }
}

impl DictionaryState for DictionaryCompressor {
    fn block_size(&self) -> usize {
        self.info.block_size
    }

    fn lookup(&mut self, string: &[u8]) -> bool {
        match self.map.get(string) {
            Some(&index) => {
                self.latest_lookup = index + 1;
                true
            }
            None => false,
        }
    }

    fn calculate_space_requirements(&mut self, new_string: bool, size: usize) -> bool {
        let block_size = self.info.block_size;
        let count = self.count as usize;
        if !new_string {
            return required_space(
                count + 1,
                self.index_buffer.len(),
                self.dict_size,
                self.current_width,
            ) <= block_size;
        }
        self.next_width = bit_width(self.index_buffer.len());
        required_space(
            count + 1,
            self.index_buffer.len() + 1,
            self.dict_size + size,
            self.next_width,
        ) <= block_size
    }

    /// `Flush(false)`.
    fn flush(&mut self, _sink: Option<&mut SegmentSink<'_>>) -> Result<()> {
        let segment = self.finalize_segment();
        self.finished.push(segment);
        self.create_empty_segment();
        Ok(())
    }

    fn add_null(&mut self) {
        self.stats.set_has_null_fast();
        self.selection_buffer.push(0);
        self.count += 1;
    }

    fn add_new_string(&mut self, string: &[u8]) {
        update_string_stats(&mut self.stats, string);
        self.dict_size += string.len();
        let position = self.info.block_size - self.dict_size;
        self.buffer[position..position + string.len()].copy_from_slice(string);
        self.index_buffer.push(self.dict_size as u32);
        self.selection_buffer.push((self.index_buffer.len() - 1) as u64);
        self.map.insert(string.to_vec(), self.map.len() as u32);
        self.current_width = self.next_width;
        self.count += 1;
    }

    fn add_last_lookup(&mut self, string: &[u8]) {
        update_string_stats(&mut self.stats, string);
        self.selection_buffer.push(u64::from(self.latest_lookup));
        self.count += 1;
    }
}

impl Compressor for DictionaryCompressor {
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        self.update_state(None, input, count)?;
        for segment in std::mem::take(&mut self.finished) {
            sink.flush(segment)?;
        }
        Ok(())
    }

    /// `Flush(true)`.
    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let segment = self.finalize_segment();
        sink.flush(segment)
    }
}

/// `DictionaryCompressionFun::GetFunction(type).init_analyze`: only before `DICT_FSST`
/// (storage version 5).
pub(crate) fn init_analyze(ty: &LogicalType, info: &CompressionInfo) -> Option<Box<dyn Analyzer>> {
    if info.storage_version >= 5 {
        return None;
    }
    Some(Box::new(DictionaryAnalyzer {
        ty: ty.clone(),
        info: *info,
        segment_count: 0,
        tuple_count: 0,
        unique_count: 0,
        max_unique_count: 0,
        dict_size: 0,
        set: HashSet::new(),
        current_width: 0,
        next_width: 0,
    }))
}
