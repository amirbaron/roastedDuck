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

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::Vector;

use super::SegmentScan;
use super::bitpacking_primitives::{GROUP_SIZE, required_size, round_up_to_group, unpack_buffer_u32};
use super::libfsst::FsstDecoder;
use super::primitive::{align8, corrupt, load, set_invalid_where, write_string_ranges};
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
