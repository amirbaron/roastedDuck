//! Dictionary + FSST compression of strings (DuckDB's `dict_fsst.cpp` and
//! `compression/dict_fsst/*`).
//!
//! Segment layout (each part 8-byte aligned): the header (`dict_size`, `dict_count`, mode,
//! string length width, dictionary index width, symbol table size), the dictionary strings
//! back to back (FSST encoded in `DICT_FSST` and `FSST_ONLY` mode), the serialized symbol
//! table, the bit-packed length of every dictionary string and the bit-packed dictionary
//! index of every row. Dictionary entry 0 is NULL; `FSST_ONLY` segments store no indices:
//! row `i` is entry `i + 1`. The codec covers the column's validity
//! (`NO_VALIDITY_REQUIRED`).

use std::collections::HashMap;
use std::sync::Arc;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::{LogicalType, Vector};

use super::bitpacking_primitives::{
    GROUP_SIZE, minimum_bit_width_unsigned, pack_buffer_u64, required_size, round_up_to_group,
    unpack_buffer_u32,
};
use super::libfsst::{FSST_DECODER_SIZE, FsstDecoder, FsstEncoder};
use super::primitive::{
    align8, corrupt, load, set_invalid_where, unified_strings, update_string_stats, write_string_ranges,
};
use super::{Analyzer, CompressionInfo, CompressionType, Compressor, SegmentScan};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;

/// The size of `dict_fsst_compression_header_t` (`DICTIONARY_HEADER_SIZE`).
pub(crate) const DICT_FSST_HEADER_SIZE: usize = 16;

/// The encoding of a `DICT_FSST` segment (`DictFSSTMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum DictFsstMode {
    Dictionary = 0,
    DictFsst = 1,
    FsstOnly = 2,
}

/// The scan state of a `DICT_FSST` segment (DuckDB's `dict_fsst::CompressedStringScanState`).
/// The dictionary is decoded once into `dictionary` (`Initialize` with a dictionary);
/// `FSST_ONLY` segments decode the strings of each scan.
#[derive(Debug)]
pub(crate) struct DictFsstScanState {
    mode: DictFsstMode,
    /// The decoded dictionary strings, back to back, and the range of every entry.
    dictionary: Arc<Vec<u8>>,
    entries: Vec<std::ops::Range<usize>>,
    /// The raw (in `FSST_ONLY` mode encoded) entries, the decoder and a scratch buffer.
    encoded: Vec<std::ops::Range<usize>>,
    raw: Vec<u8>,
    decoder: Option<FsstDecoder>,
    indices_offset: usize,
    indices_width: u8,
    data: crate::table::column_segment::PersistentData,
    selection: Vec<u32>,
}

impl DictFsstScanState {
    /// `CompressedStringScanState::Initialize`.
    pub(crate) fn new(segment: &ColumnSegment) -> Result<Self> {
        let data = segment.pin_persistent()?;
        let bytes = data.bytes();
        let dict_size = load::<u32>(bytes, 0)? as usize;
        let dict_count = load::<u32>(bytes, 4)? as usize;
        let mode = match load::<u8>(bytes, 8)? {
            0 => DictFsstMode::Dictionary,
            1 => DictFsstMode::DictFsst,
            2 => DictFsstMode::FsstOnly,
            other => {
                return Err(corrupt(format!(
                    "This block was written with a mode that is not recognized by this version, highest available \
                     mode 3, found mode: {other}"
                )));
            }
        };
        let lengths_width = load::<u8>(bytes, 9)?;
        let indices_width = load::<u8>(bytes, 10)?;
        let symbol_table_size = load::<u32>(bytes, 12)? as usize;
        let lengths_space = required_size(dict_count, lengths_width);
        let indices_space = required_size(segment.count() as usize, indices_width);
        let dictionary_dest = align8(DICT_FSST_HEADER_SIZE);
        let symbol_table_dest = align8(dictionary_dest + dict_size);
        let lengths_dest = align8(symbol_table_dest + symbol_table_size);
        let indices_dest = align8(lengths_dest + lengths_space);
        if segment.block_offset() + indices_dest + indices_space > segment.block_size() {
            return Err(corrupt(
                "Failed to scan dictionary string - index was out of range. Database file appears to be corrupted.",
            ));
        }
        let decoder = if mode == DictFsstMode::Dictionary {
            None
        } else {
            let (decoder, _) = bytes
                .get(symbol_table_dest..)
                .and_then(FsstDecoder::import)
                .ok_or_else(|| corrupt("DICT_FSST segment without a valid symbol table"))?;
            Some(decoder)
        };
        let mut lengths = vec![0u32; round_up_to_group(dict_count)];
        unpack_buffer_u32(
            bytes.get(lengths_dest..).unwrap_or(&[]),
            dict_count,
            lengths_width,
            &mut lengths,
        );
        // The raw (possibly encoded) dictionary entries.
        let mut encoded = Vec::with_capacity(dict_count);
        let mut offset = dictionary_dest;
        for &length in &lengths[..dict_count] {
            encoded.push(offset..offset + length as usize);
            offset += length as usize;
        }
        if offset > bytes.len() {
            return Err(corrupt(
                "DICT_FSST dictionary out of range - corrupt database file",
            ));
        }
        let mut state = Self {
            mode,
            dictionary: Arc::new(Vec::new()),
            entries: Vec::new(),
            encoded,
            raw: Vec::new(),
            decoder,
            indices_offset: indices_dest,
            indices_width,
            data,
            selection: Vec::new(),
        };
        if mode != DictFsstMode::FsstOnly {
            state.decode_dictionary()?;
        }
        Ok(state)
    }

    /// Decodes every dictionary entry (index 0 is NULL and stays empty).
    fn decode_dictionary(&mut self) -> Result<()> {
        let bytes = self.data.bytes();
        let mut dictionary = Vec::new();
        let mut entries = Vec::with_capacity(self.encoded.len());
        for (index, range) in self.encoded.iter().enumerate() {
            let begin = dictionary.len();
            if index > 0 && !range.is_empty() {
                let raw = bytes
                    .get(range.clone())
                    .ok_or_else(|| corrupt("DICT_FSST string out of range - corrupt database file"))?;
                match &self.decoder {
                    Some(decoder) => decoder.decompress_into(raw, &mut dictionary),
                    None => dictionary.extend_from_slice(raw),
                }
            }
            entries.push(begin..dictionary.len());
        }
        self.dictionary = Arc::new(dictionary);
        self.entries = entries;
        Ok(())
    }

    /// `GetSelVec`: the dictionary index of rows `start..start + count` (in
    /// `self.selection[..count]`).
    fn unpack_selection(&mut self, start: usize, count: usize) {
        let start_offset = start % GROUP_SIZE;
        let decompress_count = round_up_to_group(count + start_offset);
        self.selection.resize(decompress_count, 0);
        let src_offset = self.indices_offset + (start - start_offset) * self.indices_width as usize / 8;
        let src = self.data.bytes().get(src_offset..).unwrap_or(&[]);
        unpack_buffer_u32(src, decompress_count, self.indices_width, &mut self.selection);
        self.selection.copy_within(start_offset..start_offset + count, 0);
    }
}

impl SegmentScan for DictFsstScanState {
    /// `ScanToFlatVector`.
    fn scan(
        &mut self,
        _segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        if self.mode == DictFsstMode::FsstOnly {
            let decoder = self
                .decoder
                .as_ref()
                .ok_or_else(|| corrupt("DICT_FSST segment without a valid symbol table"))?;
            let bytes = self.data.bytes();
            self.raw.clear();
            let mut ranges = Vec::with_capacity(count);
            for row in start..start + count {
                let range = self
                    .encoded
                    .get(row + 1)
                    .cloned()
                    .ok_or_else(|| corrupt("DICT_FSST row out of range - corrupt database file"))?;
                let begin = self.raw.len();
                let raw = bytes
                    .get(range)
                    .ok_or_else(|| corrupt("DICT_FSST string out of range - corrupt database file"))?;
                decoder.decompress_into(raw, &mut self.raw);
                ranges.push(begin..self.raw.len());
            }
            let decoded = Arc::new(std::mem::take(&mut self.raw));
            return write_string_ranges(result, result_offset, &decoded, ranges);
        }
        self.unpack_selection(start, count);
        let mut ranges = Vec::with_capacity(count);
        for &index in &self.selection[..count] {
            let range =
                self.entries.get(index as usize).cloned().ok_or_else(|| {
                    corrupt("DICT_FSST dictionary index out of range - corrupt database file")
                })?;
            ranges.push(range);
        }
        write_string_ranges(result, result_offset, &self.dictionary, ranges)?;
        let selection = &self.selection;
        set_invalid_where(result, result_offset, count, |row| selection[row] != 0);
        Ok(())
    }
}

/// `DictFSSTCompression::STRING_SIZE_LIMIT`.
const STRING_SIZE_LIMIT: usize = 16384;
/// Where the dictionary starts (after the header, 8-byte aligned).
const DICTIONARY_START: usize = DICT_FSST_HEADER_SIZE.next_multiple_of(8);
/// `DICTIONARY_ENCODE_THRESHOLD`: dictionaries smaller than this are not FSST encoded.
const DICTIONARY_ENCODE_THRESHOLD: usize = 4096;

/// `MinimumBitWidth` of an unsigned value.
fn bit_width(value: usize) -> u8 {
    minimum_bit_width_unsigned(value as u128, 64)
}

/// `GetStringSizeLimit`: the exclusive limit of string lengths that fit an empty segment
/// (plain, or FSST encoded in the worst case of every byte escaped).
fn string_size_limit(available: usize, fsst_encoded: bool) -> usize {
    let mut max_string_length = STRING_SIZE_LIMIT - 1;
    if fsst_encoded {
        max_string_length *= 2;
    }
    let lengths_space = required_size(2, bit_width(max_string_length));
    let indices_space = required_size(1, bit_width(1));
    let mut metadata = align8(DICT_FSST_HEADER_SIZE);
    if fsst_encoded {
        metadata += 7;
    }
    metadata += 7;
    if fsst_encoded {
        metadata += align8(FSST_DECODER_SIZE);
    }
    metadata += align8(lengths_space);
    metadata += indices_space;
    let mut max_string_size = available - metadata;
    if fsst_encoded {
        max_string_size /= 2;
    }
    STRING_SIZE_LIMIT.min(max_string_size + 1)
}

/// `DictFSSTAnalyzeState`.
#[derive(Debug)]
struct DictFsstAnalyzer {
    ty: LogicalType,
    info: CompressionInfo,
    contains_nulls: bool,
    string_size_limit: usize,
    fsst_string_size_limit: usize,
    disable_fsst: bool,
    total_string_length: usize,
    total_count: usize,
}

impl Analyzer for DictFsstAnalyzer {
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        for string in unified_strings(input, count) {
            let Some(bytes) = string else {
                self.contains_nulls = true;
                continue;
            };
            self.total_string_length += bytes.len();
            if bytes.len() >= self.string_size_limit {
                return Ok(false);
            }
            if bytes.len() >= self.fsst_string_size_limit {
                self.disable_fsst = true;
            }
        }
        self.total_count += count;
        Ok(true)
    }

    fn final_analyze(&mut self) -> Result<Option<u64>> {
        Ok(Some((self.total_string_length as f64 / 2.0) as u64))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        let map_capacity = self.total_count.min(self.info.block_size) / 2;
        let mut compressor = DictFsstCompressor {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            info: self.info,
            contains_nulls: self.contains_nulls,
            disable_fsst: self.disable_fsst,
            buffer: Vec::new(),
            dictionary_offset: 0,
            string_lengths_space: 0,
            string_lengths: Vec::new(),
            dict_count: 0,
            string_lengths_width: 0,
            real_string_lengths_width: 0,
            dictionary_indices_space: 0,
            dictionary_indices: Vec::new(),
            dictionary_indices_width: 0,
            map: StringMap::new(map_capacity),
            encoding_buffer: Vec::new(),
            to_encode_string_sum: 0,
            tuple_count: 0,
            encoder: None,
            symbol_table: Vec::new(),
            append_state: AppendState::Regular,
        };
        compressor.create_empty_segment();
        Ok(Box::new(compressor))
    }
}

/// `DictionaryAppendState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppendState {
    /// The symbol table threshold was not reached yet.
    Regular,
    /// The dictionary is FSST encoded; new entries are encoded lazily.
    Encoded,
    /// The threshold was reached and encoding did not pay off.
    NotEncoded,
    /// The dictionary is encoded and every row is its own entry (`FSST_ONLY`).
    EncodedAllUnique,
}

impl AppendState {
    fn is_encoded(self) -> bool {
        matches!(self, Self::Encoded | Self::EncodedAllUnique)
    }

    /// `ConvertToMode`.
    fn mode(self) -> DictFsstMode {
        match self {
            Self::Encoded => DictFsstMode::DictFsst,
            Self::Regular | Self::NotEncoded => DictFsstMode::Dictionary,
            Self::EncodedAllUnique => DictFsstMode::FsstOnly,
        }
    }
}

/// The string -> index map of the segment (`PrimitiveDictionary<string_t>`): a fixed
/// maximum number of entries, after which inserts are ignored until it is cleared.
#[derive(Debug)]
struct StringMap {
    entries: HashMap<Vec<u8>, u32>,
    maximum_size: usize,
    full: bool,
}

impl StringMap {
    fn new(maximum_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            maximum_size,
            full: false,
        }
    }

    fn lookup(&self, string: &[u8]) -> Option<u32> {
        self.entries.get(string).copied()
    }

    fn insert(&mut self, string: &[u8]) {
        if self.full || self.entries.contains_key(string) {
            return;
        }
        if self.entries.len() + 1 > self.maximum_size {
            self.full = true;
            return;
        }
        let index = self.entries.len() as u32;
        self.entries.insert(string.to_vec(), index);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.full = false;
    }
}

/// `DictFSSTCompressionState`.
#[derive(Debug)]
struct DictFsstCompressor {
    ty: LogicalType,
    info: CompressionInfo,
    contains_nulls: bool,
    disable_fsst: bool,
    buffer: Vec<u8>,
    stats: BaseStatistics,
    /// Bytes of dictionary written (`dictionary_offset`).
    dictionary_offset: usize,
    string_lengths_space: usize,
    string_lengths: Vec<u32>,
    dict_count: u32,
    string_lengths_width: u8,
    real_string_lengths_width: u8,
    dictionary_indices_space: usize,
    dictionary_indices: Vec<u32>,
    dictionary_indices_width: u8,
    map: StringMap,
    /// New entries waiting to be FSST encoded.
    encoding_buffer: Vec<Vec<u8>>,
    to_encode_string_sum: usize,
    tuple_count: usize,
    encoder: Option<FsstEncoder>,
    symbol_table: Vec<u8>,
    append_state: AppendState,
}

/// What to add in `CompressInternal`: a lookup of an existing entry or a new entry.
enum Addition<'a> {
    Lookup(u32),
    New(&'a [u8]),
}

impl DictFsstCompressor {
    /// `CreateEmptySegment`.
    fn create_empty_segment(&mut self) {
        self.buffer = vec![0; self.info.block_size];
        self.stats = BaseStatistics::create_empty(self.ty.clone());
        self.append_state = if self.disable_fsst {
            AppendState::NotEncoded
        } else {
            AppendState::Regular
        };
        self.string_lengths_width = 0;
        self.real_string_lengths_width = 0;
        self.dictionary_indices_width = 0;
        self.string_lengths_space = 0;
        self.dictionary_indices_space = 0;
        self.tuple_count = 0;
        self.string_lengths.clear();
        self.string_lengths.push(0);
        self.dict_count = 1;
        self.symbol_table.clear();
        self.dictionary_offset = 0;
    }

    /// The available space of the segment: `REGULAR` reserves room for a symbol table.
    fn available_space(&self, state: AppendState) -> usize {
        if state == AppendState::Regular {
            self.info.block_size - FSST_DECODER_SIZE
        } else {
            self.info.block_size
        }
    }

    /// `AddLookup`.
    fn add_lookup(&mut self, state: AppendState, lookup: u32, recalculate: bool) -> bool {
        let mut new_indices_space = self.dictionary_indices_space;
        if state != AppendState::EncodedAllUnique && recalculate {
            new_indices_space = required_size(self.tuple_count + 1, self.dictionary_indices_width);
        }
        let mut required = align8(DICT_FSST_HEADER_SIZE) + self.dictionary_offset;
        if state == AppendState::Encoded {
            required += self.to_encode_string_sum;
        }
        required = align8(required);
        if state.is_encoded() {
            required = align8(required + self.symbol_table.len());
        }
        required = align8(required + self.string_lengths_space) + new_indices_space;
        if required > self.available_space(state) {
            return false;
        }
        if recalculate {
            self.dictionary_indices_space = new_indices_space;
        }
        self.dictionary_indices.push(lookup);
        true
    }

    /// `AddToDictionary`.
    fn add_to_dictionary(&mut self, state: AppendState, string: &[u8], recalculate: bool) -> bool {
        let mut length = string.len();
        if state == AppendState::Encoded {
            // Encoding may double the size; entries are encoded lazily.
            length *= 2;
        }
        let requires_higher_length_width = length >= 1usize << self.string_lengths_width;
        let requires_higher_index_width = self.dict_count as usize >= 1usize << self.dictionary_indices_width;
        let recalculate_lengths_space = (self.dict_count as usize).is_multiple_of(GROUP_SIZE);
        let mut new_lengths_width = self.string_lengths_width;
        let mut new_lengths_space = self.string_lengths_space;
        if requires_higher_length_width {
            new_lengths_width = bit_width(length);
        }
        if requires_higher_length_width || recalculate_lengths_space {
            new_lengths_space = required_size(self.dict_count as usize + 1, new_lengths_width);
        }
        let mut new_indices_width = self.dictionary_indices_width;
        let mut new_indices_space = self.dictionary_indices_space;
        if state != AppendState::EncodedAllUnique {
            if requires_higher_index_width {
                new_indices_width = bit_width(self.dict_count as usize);
            }
            if requires_higher_index_width || recalculate {
                new_indices_space = required_size(self.tuple_count + 1, new_indices_width);
            }
        }
        let mut required = align8(DICT_FSST_HEADER_SIZE) + self.dictionary_offset + length;
        if state == AppendState::Encoded {
            if self.to_encode_string_sum == 0 {
                required += 7;
            }
            required += self.to_encode_string_sum;
        }
        required = align8(required);
        if state.is_encoded() {
            required = align8(required + self.symbol_table.len());
        }
        required = align8(required + new_lengths_space) + new_indices_space;
        if required > self.available_space(state) {
            return false;
        }
        self.dictionary_indices.push(self.dict_count);
        if state == AppendState::Encoded {
            self.encoding_buffer.push(string.to_vec());
            if self.to_encode_string_sum == 0 {
                self.to_encode_string_sum = 7;
            }
            self.to_encode_string_sum += length;
            self.map.insert(string);
        } else {
            self.string_lengths.push(length as u32);
            let start = DICTIONARY_START + self.dictionary_offset;
            self.buffer[start..start + length].copy_from_slice(string);
            self.dictionary_offset += length;
            self.map.insert(string);
        }
        self.dict_count += 1;
        if requires_higher_length_width {
            self.string_lengths_width = new_lengths_width;
        }
        if requires_higher_length_width || recalculate_lengths_space {
            self.string_lengths_space = new_lengths_space;
        }
        if requires_higher_index_width {
            self.dictionary_indices_width = new_indices_width;
        }
        if requires_higher_index_width || recalculate {
            self.dictionary_indices_space = new_indices_space;
        }
        true
    }

    /// Adds a lookup or a new entry in `state`.
    fn add(&mut self, state: AppendState, addition: &Addition<'_>, recalculate: bool) -> bool {
        match addition {
            Addition::Lookup(lookup) => self.add_lookup(state, *lookup, recalculate),
            Addition::New(string) => self.add_to_dictionary(state, string, recalculate),
        }
    }

    /// `FlushEncodingBuffer`: encodes the pending entries into the dictionary.
    fn flush_encoding_buffer(&mut self) -> Result<()> {
        if self.encoding_buffer.is_empty() {
            return Ok(());
        }
        let encoder = self
            .encoder
            .as_ref()
            .ok_or_else(|| Error::internal("DICT_FSST encoding without an encoder"))?;
        let mut taken = align8(DICT_FSST_HEADER_SIZE) + self.symbol_table.len();
        taken = align8(align8(taken) + self.string_lengths_space) + self.dictionary_indices_space;
        taken = align8(taken) + self.dictionary_offset;
        let strings: Vec<&[u8]> = self.encoding_buffer.iter().map(Vec::as_slice).collect();
        let compressed = encoder.compress_with_limit(&strings, self.info.block_size - taken);
        if compressed.len() != strings.len() {
            return Err(Error::new(
                ErrorKind::Fatal,
                "Somehow we did not have enough room in the segment to store the encoded strings",
            ));
        }
        self.string_lengths_width = self.real_string_lengths_width;
        let mut biggest = (1usize << self.string_lengths_width) - 1;
        for encoded in &compressed {
            let start = DICTIONARY_START + self.dictionary_offset;
            self.buffer[start..start + encoded.len()].copy_from_slice(encoded);
            biggest = biggest.max(encoded.len());
            self.string_lengths.push(encoded.len() as u32);
            self.dictionary_offset += encoded.len();
        }
        if biggest >= 1usize << self.string_lengths_width {
            self.string_lengths_width = bit_width(biggest);
        }
        self.real_string_lengths_width = self.string_lengths_width;
        self.string_lengths_space = required_size(self.dict_count as usize, self.string_lengths_width);
        self.to_encode_string_sum = 0;
        self.encoding_buffer.clear();
        Ok(())
    }

    /// `AllUnique`: every row so far added its own entry.
    fn all_unique(&self) -> bool {
        self.string_lengths.len() - 1 == self.tuple_count
    }

    /// `TryEncode`: with a full plain dictionary, tries to FSST encode it.
    fn try_encode(&mut self) -> AppendState {
        if self.dictionary_offset < DICTIONARY_ENCODE_THRESHOLD {
            return AppendState::NotEncoded;
        }
        let new_state = if !self.contains_nulls && self.all_unique() {
            AppendState::EncodedAllUnique
        } else {
            AppendState::Encoded
        };
        let start = DICTIONARY_START;
        let dictionary = self.buffer[start..start + self.dictionary_offset].to_vec();
        let mut strings = Vec::with_capacity(self.string_lengths.len());
        let mut offset = 0;
        for &length in &self.string_lengths[1..] {
            strings.push(&dictionary[offset..offset + length as usize]);
            offset += length as usize;
        }
        let encoder = FsstEncoder::create(&strings);
        let compressed = encoder.compress_with_limit(&strings, self.dictionary_offset);
        let new_size: usize = compressed.iter().map(Vec::len).sum();
        let max_length = compressed.iter().map(Vec::len).max().unwrap_or(0);
        let mut usable = compressed.len() == strings.len()
            && new_size + DICTIONARY_ENCODE_THRESHOLD <= self.dictionary_offset;
        let symbol_table = encoder.export();
        let new_lengths_width = bit_width(max_length);
        let new_lengths_space = required_size(self.dict_count as usize, new_lengths_width);
        if usable {
            let mut required = align8(align8(DICT_FSST_HEADER_SIZE) + new_size) + symbol_table.len();
            required = align8(align8(required) + new_lengths_space) + self.dictionary_indices_space;
            usable = required <= self.info.block_size;
        }
        if !usable {
            // The plain dictionary stays.
            return AppendState::NotEncoded;
        }
        // Rewrite the dictionary encoded.
        let mut position = start;
        for encoded in &compressed {
            self.buffer[position..position + encoded.len()].copy_from_slice(encoded);
            position += encoded.len();
        }
        if new_state == AppendState::EncodedAllUnique {
            self.dictionary_indices_width = 0;
            self.dictionary_indices_space = 0;
        }
        self.map.clear();
        for (index, encoded) in compressed.iter().enumerate() {
            if new_state == AppendState::Encoded {
                self.map.insert(strings[index]);
            } else {
                self.map.insert(encoded);
            }
            self.string_lengths[index + 1] = encoded.len() as u32;
        }
        self.dictionary_offset = new_size;
        self.string_lengths_width = new_lengths_width;
        self.string_lengths_space = new_lengths_space;
        self.real_string_lengths_width = new_lengths_width;
        self.symbol_table = symbol_table;
        self.encoder = Some(encoder);
        new_state
    }

    /// `Finalize`: writes the header, symbol table, string lengths and indices.
    fn finalize_segment(&mut self) -> FinishedSegment {
        let encoded = self.append_state.is_encoded();
        let symbol_table_size = if encoded { self.symbol_table.len() } else { 0 };
        let mut required = align8(align8(DICT_FSST_HEADER_SIZE) + self.dictionary_offset);
        if encoded {
            required = align8(required + symbol_table_size);
        }
        required = align8(required + self.string_lengths_space) + self.dictionary_indices_space;
        let symbol_table_dest = align8(DICTIONARY_START + self.dictionary_offset);
        let lengths_dest = align8(symbol_table_dest + symbol_table_size);
        let indices_dest = align8(lengths_dest + self.string_lengths_space);
        self.buffer[0..4].copy_from_slice(&(self.dictionary_offset as u32).to_le_bytes());
        self.buffer[4..8].copy_from_slice(&self.dict_count.to_le_bytes());
        self.buffer[8] = self.append_state.mode() as u8;
        self.buffer[9] = self.string_lengths_width;
        self.buffer[10] = self.dictionary_indices_width;
        self.buffer[12..16].copy_from_slice(&(symbol_table_size as u32).to_le_bytes());
        if encoded {
            self.buffer[symbol_table_dest..symbol_table_dest + symbol_table_size]
                .copy_from_slice(&self.symbol_table);
        }
        let lengths: Vec<u64> = self.string_lengths.iter().map(|&l| u64::from(l)).collect();
        let lengths_size = required_size(lengths.len(), self.string_lengths_width);
        pack_buffer_u64(
            &lengths,
            self.string_lengths_width,
            &mut self.buffer[lengths_dest..lengths_dest + lengths_size],
        );
        let indices: Vec<u64> = self.dictionary_indices.iter().map(|&i| u64::from(i)).collect();
        let indices_size = required_size(indices.len(), self.dictionary_indices_width);
        pack_buffer_u64(
            &indices,
            self.dictionary_indices_width,
            &mut self.buffer[indices_dest..indices_dest + indices_size],
        );
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        FinishedSegment {
            data: self.buffer[..required].to_vec(),
            count: self.tuple_count as u64,
            stats,
            compression: CompressionType::DictFsst,
            overflow_blocks: Vec::new(),
        }
    }

    /// `Flush`.
    fn flush(&mut self, sink: &mut SegmentSink<'_>, last: bool) -> Result<()> {
        if last {
            self.flush_encoding_buffer()?;
        }
        if self.tuple_count == 0 {
            return Ok(());
        }
        let segment = self.finalize_segment();
        sink.flush(segment)?;
        self.map.clear();
        self.string_lengths.clear();
        self.dictionary_indices.clear();
        self.encoder = None;
        self.symbol_table.clear();
        if !last {
            self.create_empty_segment();
        }
        Ok(())
    }

    /// `CompressInternal`: tries to add row `index` of `strings` (`encoded` holds the
    /// rows' encodings in `FSST_ONLY` mode).
    fn compress_internal(
        &mut self,
        strings: &[Option<&[u8]>],
        index: usize,
        encoded: &mut Option<(usize, Vec<Vec<u8>>)>,
    ) -> Result<bool> {
        let state = self.append_state;
        let recalculate =
            state != AppendState::EncodedAllUnique && self.tuple_count.is_multiple_of(GROUP_SIZE);
        let string = strings[index];
        let lookup = if state == AppendState::EncodedAllUnique || string.is_none() {
            Some(0)
        } else {
            string.and_then(|bytes| self.map.lookup(bytes)).map(|i| i + 1)
        };
        match state {
            AppendState::Regular | AppendState::NotEncoded => {
                let addition = match (lookup, string) {
                    (Some(lookup), _) => Addition::Lookup(lookup),
                    (None, Some(bytes)) => Addition::New(bytes),
                    (None, None) => Addition::Lookup(0),
                };
                Ok(self.add(state, &addition, recalculate))
            }
            AppendState::Encoded => {
                let addition = match (lookup, string) {
                    (Some(lookup), _) => Addition::Lookup(lookup),
                    (None, Some(bytes)) => Addition::New(bytes),
                    (None, None) => Addition::Lookup(0),
                };
                if self.add(state, &addition, recalculate) {
                    return Ok(true);
                }
                if self.encoding_buffer.is_empty() {
                    return Ok(false);
                }
                self.flush_encoding_buffer()?;
                Ok(self.add(state, &addition, recalculate))
            }
            AppendState::EncodedAllUnique => {
                if encoded.is_none() {
                    let encoder = self
                        .encoder
                        .as_ref()
                        .ok_or_else(|| Error::internal("DICT_FSST encoding without an encoder"))?;
                    let inputs: Vec<&[u8]> = strings[index..].iter().map(|s| s.unwrap_or_default()).collect();
                    *encoded = Some((index, encoder.compress(&inputs)));
                }
                let Some((offset, values)) = encoded.as_ref() else {
                    return Err(Error::internal("DICT_FSST encoded input missing"));
                };
                let value = values
                    .get(index - offset)
                    .cloned()
                    .ok_or_else(|| Error::internal("DICT_FSST encoded input out of range"))?;
                Ok(self.add_to_dictionary(state, &value, recalculate))
            }
        }
    }
}

impl Compressor for DictFsstCompressor {
    /// `DictFSSTCompressionState::Compress`.
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        let strings: Vec<Option<&[u8]>> = unified_strings(input, count).collect();
        let mut encoded = None;
        for index in 0..count {
            let mut fits = self.compress_internal(&strings, index, &mut encoded)?;
            if !fits && self.append_state == AppendState::Regular {
                self.append_state = self.try_encode();
                fits = self.compress_internal(&strings, index, &mut encoded)?;
            }
            if !fits {
                self.flush(sink, false)?;
                encoded = None;
                if !self.compress_internal(&strings, index, &mut encoded)? {
                    return Err(Error::new(
                        ErrorKind::Fatal,
                        "Compressing directly after Flush doesn't fit - expected to throw earlier!",
                    ));
                }
            }
            match strings[index] {
                Some(bytes) => update_string_stats(&mut self.stats, bytes),
                None => self.stats.set_has_null_fast(),
            }
            self.tuple_count += 1;
        }
        Ok(())
    }

    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        self.flush(sink, true)
    }
}

/// `DictFSSTCompressionFun::GetFunction(type).init_analyze`: from storage version 5.
pub(crate) fn init_analyze(ty: &LogicalType, info: &CompressionInfo) -> Option<Box<dyn Analyzer>> {
    if info.storage_version < 5 {
        return None;
    }
    Some(Box::new(DictFsstAnalyzer {
        ty: ty.clone(),
        info: *info,
        contains_nulls: false,
        string_size_limit: string_size_limit(info.block_size, false),
        fsst_string_size_limit: string_size_limit(info.block_size, true),
        disable_fsst: false,
        total_string_length: 0,
        total_count: 0,
    }))
}
