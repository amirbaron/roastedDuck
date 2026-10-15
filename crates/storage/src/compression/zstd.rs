//! ZSTD compression of strings (DuckDB's `zstd.cpp`).
//!
//! Strings are compressed in vectors of [`ZSTD_VECTOR_SIZE`], each vector one zstd frame
//! preceded by the `u32` lengths of its strings. Segment layout: per vector its page id
//! (`-1` when on the segment's own page), page offset, uncompressed and compressed size,
//! followed by vector data. A vector's data may continue on extra pages; every page that
//! is continued ends with the id of the next page. The extra pages are the segment state
//! (`overflow_blocks`).

use std::io::Read;
use std::sync::Arc;

use rustdb_buffer::{BlockId, INVALID_BLOCK};
use rustdb_common::Result;
use rustdb_types::Vector;

use super::SegmentScan;
use super::primitive::{corrupt, load, write_string_ranges};
use crate::table::ColumnSegment;
use crate::table::column_segment::{PersistentData, SegmentBuffer};

/// Strings per zstd vector (`ZSTD_VECTOR_SIZE`).
pub(crate) const ZSTD_VECTOR_SIZE: usize = 2048;

/// `GetVectorMetadataSize`: page ids (`i64`), page offsets (`u32`), uncompressed and
/// compressed sizes (`u64`), each array aligned to its element size.
#[allow(dead_code, reason = "used by the checkpoint writer")]
pub(crate) fn vector_metadata_size(vector_count: usize) -> usize {
    let mut size = 8 * vector_count;
    size = size.next_multiple_of(4) + 4 * vector_count;
    size = size.next_multiple_of(8) + 8 * vector_count;
    size.next_multiple_of(8) + 8 * vector_count
}

/// The offsets of the four vector metadata arrays of a segment with `vector_count` vectors.
fn metadata_offsets(vector_count: usize) -> [usize; 4] {
    let page_ids = 0;
    let page_offsets = (page_ids + 8 * vector_count).next_multiple_of(4);
    let uncompressed = (page_offsets + 4 * vector_count).next_multiple_of(8);
    let compressed = (uncompressed + 8 * vector_count).next_multiple_of(8);
    [page_ids, page_offsets, uncompressed, compressed]
}

/// The bytes of an extra page, or of the segment's own page (`segment`).
fn page_bytes<'a>(page: Option<&'a SegmentBuffer>, segment: &'a [u8]) -> &'a [u8] {
    match page {
        // SAFETY: on-disk pages are immutable; the pin lives as long as the borrow.
        Some(buffer) => unsafe { buffer.bytes(0, buffer.len()) }.unwrap_or(&[]),
        None => segment,
    }
}

/// A decoded vector: its strings back to back and the range of each.
#[derive(Debug)]
struct DecodedVector {
    index: usize,
    strings: Arc<Vec<u8>>,
    ranges: Vec<std::ops::Range<usize>>,
}

/// The scan state of a ZSTD segment (DuckDB's `ZSTDScanState`); the current vector is
/// decompressed as a whole.
#[derive(Debug)]
pub(crate) struct ZstdScanState {
    data: PersistentData,
    count: usize,
    offsets: [usize; 4],
    current: Option<DecodedVector>,
}

impl ZstdScanState {
    pub(crate) fn new(segment: &ColumnSegment) -> Result<Self> {
        let count = segment.count() as usize;
        Ok(Self {
            data: segment.pin_persistent()?,
            count,
            offsets: metadata_offsets(count.div_ceil(ZSTD_VECTOR_SIZE)),
            current: None,
        })
    }

    /// Pins page `block_id` of the segment's block manager.
    fn pin_page(segment: &ColumnSegment, block_id: BlockId) -> Result<SegmentBuffer> {
        let block_manager = segment
            .persistent_block_manager()
            .ok_or_else(|| corrupt("ZSTD page of a transient segment"))?;
        let handle = segment.buffer_manager().register_block(block_manager, block_id);
        Ok(SegmentBuffer::new(segment.buffer_manager().pin(&handle)?, 0))
    }

    /// `LoadVector` + `DecompressString` for the whole vector.
    fn load_vector(&self, segment: &ColumnSegment, index: usize) -> Result<DecodedVector> {
        let bytes = self.data.bytes();
        let [page_ids, page_offsets, uncompressed_sizes, compressed_sizes] = self.offsets;
        let page_id: i64 = load(bytes, page_ids + 8 * index)?;
        let page_offset = load::<u32>(bytes, page_offsets + 4 * index)? as usize;
        let uncompressed_size = load::<u64>(bytes, uncompressed_sizes + 8 * index)? as usize;
        let compressed_size = load::<u64>(bytes, compressed_sizes + 8 * index)? as usize;
        let vector_size = (self.count - index * ZSTD_VECTOR_SIZE).min(ZSTD_VECTOR_SIZE);
        let block_size = segment.block_size();
        let lengths_size = 4 * vector_size;

        // The first page: the segment's own or an extra page.
        let (page, page_start) = if page_id == INVALID_BLOCK {
            (None, segment.block_offset())
        } else {
            (Some(Self::pin_page(segment, page_id)?), 0)
        };
        let lengths_offset = (page_start + page_offset).next_multiple_of(4);
        // `bytes` starts at the segment's offset in its block.
        let local = |offset: usize| {
            if page.is_none() {
                offset - page_start
            } else {
                offset
            }
        };
        let mut lengths = Vec::with_capacity(vector_size);
        {
            let current = page_bytes(page.as_ref(), bytes);
            for row in 0..vector_size {
                lengths.push(load::<u32>(current, local(lengths_offset) + 4 * row)? as usize);
            }
        }
        let data_offset = lengths_offset + lengths_size;
        let first_size = if page_offset + lengths_size + compressed_size > block_size - 8 {
            compressed_size.min((block_size - 8).saturating_sub(data_offset))
        } else {
            compressed_size.min(block_size.saturating_sub(data_offset))
        };
        let mut compressed = Vec::with_capacity(compressed_size);
        let mut next_id_offset = local(data_offset) + first_size;
        {
            let current = page_bytes(page.as_ref(), bytes);
            compressed.extend_from_slice(
                current
                    .get(local(data_offset)..next_id_offset)
                    .ok_or_else(|| corrupt("ZSTD vector out of range - corrupt database file"))?,
            );
        }
        let mut current_page = page;
        while compressed.len() < compressed_size {
            // `LoadNextPageForVector`: the next page id ends the consumed part of the page.
            let next_id: i64 = load(page_bytes(current_page.as_ref(), bytes), next_id_offset)?;
            let next = Self::pin_page(segment, next_id)?;
            let take = (block_size - 8).min(compressed_size - compressed.len());
            {
                let next_bytes = page_bytes(Some(&next), bytes);
                compressed.extend_from_slice(
                    next_bytes
                        .get(..take)
                        .ok_or_else(|| corrupt("ZSTD page out of range - corrupt database file"))?,
                );
            }
            next_id_offset = take;
            current_page = Some(next);
        }
        let mut strings = Vec::with_capacity(uncompressed_size);
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(compressed.as_slice())
            .map_err(|error| corrupt(format!("ZSTD Decompression failed: {error}")))?;
        decoder
            .read_to_end(&mut strings)
            .map_err(|error| corrupt(format!("ZSTD Decompression failed: {error}")))?;
        let mut ranges = Vec::with_capacity(vector_size);
        let mut offset = 0;
        for length in lengths {
            ranges.push(offset..offset + length);
            offset += length;
        }
        if offset > strings.len() {
            return Err(corrupt(
                "ZSTD vector shorter than its strings - corrupt database file",
            ));
        }
        Ok(DecodedVector {
            index,
            strings: Arc::new(strings),
            ranges,
        })
    }
}

impl SegmentScan for ZstdScanState {
    /// `ZSTDScanState::ScanPartial`.
    fn scan(
        &mut self,
        segment: &ColumnSegment,
        start: usize,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        let mut scanned = 0;
        while scanned < count {
            let row = start + scanned;
            let vector = row / ZSTD_VECTOR_SIZE;
            let offset = row % ZSTD_VECTOR_SIZE;
            if self
                .current
                .as_ref()
                .is_none_or(|current| current.index != vector)
            {
                self.current = Some(self.load_vector(segment, vector)?);
            }
            let Some(current) = &self.current else {
                return Err(corrupt("ZSTD vector missing"));
            };
            let to_scan = (count - scanned).min(current.ranges.len().saturating_sub(offset));
            if to_scan == 0 {
                return Err(corrupt("ZSTD scan out of range - corrupt database file"));
            }
            write_string_ranges(
                result,
                result_offset + scanned,
                &current.strings,
                current.ranges[offset..offset + to_scan].iter().cloned(),
            )?;
            scanned += to_scan;
        }
        Ok(())
    }
}
