//! FSST compression of strings (DuckDB's `fsst.cpp`; written by storage versions before
//! `DICT_FSST`).
//!
//! Segment layout: a header (`dict_size`, `dict_end`, `bitpacking_width`,
//! `fsst_symbol_table_offset`, all `u32`), the bit-packed compressed length of every row,
//! the serialized symbol table, and the compressed strings growing backwards from
//! `dict_end`. Row `i`'s string ends `sum(lengths[0..=i])` bytes before `dict_end`.

use std::sync::Arc;

use rustdb_common::Result;
use rustdb_types::Vector;

use super::SegmentScan;
use super::bitpacking_primitives::{GROUP_SIZE, round_up_to_group, unpack_buffer_u32};
use super::libfsst::FsstDecoder;
use super::primitive::{corrupt, load, write_string_ranges};
use crate::table::ColumnSegment;
use crate::table::column_segment::PersistentData;

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
        let mut decoded = Vec::new();
        let mut ranges = Vec::with_capacity(count);
        for &length in &self.lengths[start - bitunpack_start..start - bitunpack_start + count] {
            offset = offset.wrapping_add(length);
            let begin = decoded.len();
            if length > 0 {
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
                decoder.decompress_into(compressed, &mut decoded);
            }
            ranges.push(begin..decoded.len());
        }
        self.last_known = Some((start + count - 1, offset));
        write_string_ranges(result, result_offset, &Arc::new(decoded), ranges)
    }
}
