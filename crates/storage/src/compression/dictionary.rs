//! Dictionary compression of strings (DuckDB's `dictionary_compression.cpp` and
//! `compression/dictionary/*`; written by storage versions before `DICT_FSST`).
//!
//! Segment layout: a header (`dict_size`, `dict_end`, `index_buffer_offset`,
//! `index_buffer_count`, `bitpacking_width`, all `u32`), the bit-packed selection buffer
//! (row -> dictionary index, index 0 = NULL), the index buffer (`u32` cumulative dictionary
//! sizes per index) and the dictionary, whose strings grow backwards from `dict_end`.

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::Vector;

use super::SegmentScan;
use super::bitpacking_primitives::{GROUP_SIZE, round_up_to_group, unpack_buffer_u32};
use super::primitive::{corrupt, load, write_string_ranges};
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
