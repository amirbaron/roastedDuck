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
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, Vector};

use super::primitive::{corrupt, load, unified_strings, update_string_stats, write_string_ranges};
use super::{Analyzer, CompressionInfo, CompressionType, Compressor, SegmentScan};
use crate::checkpoint::column_checkpointer::{FinishedSegment, SegmentSink};
use crate::statistics::BaseStatistics;
use crate::table::ColumnSegment;
use crate::table::column_segment::{PersistentData, SegmentBuffer};

/// Strings per zstd vector (`ZSTD_VECTOR_SIZE`).
pub(crate) const ZSTD_VECTOR_SIZE: usize = 2048;

/// `GetVectorMetadataSize`: page ids (`i64`), page offsets (`u32`), uncompressed and
/// compressed sizes (`u64`), each array aligned to its element size.
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

/// `GetVectorCount`.
fn vector_count(count: usize) -> usize {
    count.div_ceil(ZSTD_VECTOR_SIZE)
}

/// `ZSTDAnalyzeState`: the string sizes and how the vectors split into segments.
#[derive(Debug)]
struct ZstdAnalyzer {
    ty: LogicalType,
    info: CompressionInfo,
    total_size: usize,
    count: usize,
    vectors_per_segment: usize,
    segment_count: usize,
    vectors_in_segment: usize,
    values_in_vector: usize,
}

impl Analyzer for ZstdAnalyzer {
    /// `ZSTDStorage::StringAnalyze`.
    fn analyze(&mut self, input: &Vector, count: usize) -> Result<bool> {
        self.total_size += unified_strings(input, count)
            .flatten()
            .map(<[u8]>::len)
            .sum::<usize>();
        self.values_in_vector += count;
        let writable = self.info.block_size - 8;
        while self.values_in_vector >= ZSTD_VECTOR_SIZE {
            if vector_metadata_size(self.vectors_in_segment + 1) > writable {
                self.vectors_per_segment = self.vectors_in_segment;
                self.segment_count += 1;
                self.vectors_in_segment = 0;
            } else {
                self.vectors_in_segment += 1;
            }
            self.values_in_vector -= ZSTD_VECTOR_SIZE;
        }
        self.count += count;
        Ok(true)
    }

    /// `ZSTDStorage::StringFinalAnalyze`: ZSTD is penalised below the minimum average string
    /// length.
    fn final_analyze(&mut self) -> Result<Option<u64>> {
        if self.count == 0 {
            return Ok(None);
        }
        if self.values_in_vector > 0 {
            self.vectors_in_segment += 1;
        }
        if self.vectors_in_segment > 0 {
            self.segment_count += 1;
        }
        let average_length = (self.total_size / self.count) as u64;
        let penalty = if average_length >= self.info.config.zstd_min_string_length {
            1.0
        } else {
            1000.0
        };
        let expected_compressed_size = self.total_size as f64 / 2.0;
        let estimated = expected_compressed_size as u64
            + (self.count * 4) as u64
            + vector_metadata_size(vector_count(self.count)) as u64;
        Ok(Some((estimated as f64 * penalty) as u64))
    }

    fn into_compressor(self: Box<Self>) -> Result<Box<dyn Compressor>> {
        Ok(Box::new(ZstdCompressor {
            stats: BaseStatistics::create_empty(self.ty.clone()),
            ty: self.ty,
            info: self.info,
            total_count: self.count,
            total_vector_count: vector_count(self.count),
            total_segment_count: self.segment_count,
            vectors_per_segment: self.vectors_per_segment,
            vectors_written: 0,
            segment_index: 0,
            segment: None,
            page: None,
            rows: Vec::new(),
            lengths: Vec::new(),
            has_null: Vec::new(),
        }))
    }
}

/// The segment being written: its page and vector metadata.
#[derive(Debug)]
struct ZstdSegment {
    buffer: Vec<u8>,
    /// Used bytes of the segment's page.
    offset: usize,
    total_vectors: usize,
    vectors: usize,
    count: u64,
    overflow_blocks: Vec<BlockId>,
}

/// An extra page being written and its block.
#[derive(Debug)]
struct ExtraPage {
    block_id: BlockId,
    buffer: Vec<u8>,
    offset: usize,
}

/// `ZSTDCompressionState`: strings are compressed per vector of 2048 into one zstd frame
/// (with the pure-Rust `ruzstd` encoder) that is laid out over the segment's page and extra
/// pages exactly like DuckDB lays out its streaming output.
#[derive(Debug)]
struct ZstdCompressor {
    ty: LogicalType,
    info: CompressionInfo,
    total_count: usize,
    total_vector_count: usize,
    total_segment_count: usize,
    vectors_per_segment: usize,
    vectors_written: usize,
    segment_index: usize,
    segment: Option<ZstdSegment>,
    /// The current extra page (`None`: writing on the segment's page).
    page: Option<ExtraPage>,
    stats: BaseStatistics,
    /// The bytes and lengths of the strings of the current vector, and which rows are NULL.
    rows: Vec<u8>,
    lengths: Vec<u32>,
    has_null: Vec<bool>,
}

impl ZstdCompressor {
    fn writable(&self) -> usize {
        self.info.block_size - 8
    }

    /// The current page's bytes and used size.
    fn current(&mut self) -> Result<(&mut Vec<u8>, &mut usize)> {
        if let Some(page) = &mut self.page {
            return Ok((&mut page.buffer, &mut page.offset));
        }
        let segment = self
            .segment
            .as_mut()
            .ok_or_else(|| Error::internal("ZSTD compression without a segment"))?;
        Ok((&mut segment.buffer, &mut segment.offset))
    }

    /// `NewSegment`.
    fn new_segment(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        if self.page.is_none() && self.segment.is_some() {
            return Err(Error::internal(
                "(ZSTDCompressionState::NewSegment) We are asking for a new segment, but somehow we're still writing \
                 vector data onto the initial (segment) page",
            ));
        }
        self.flush_segment(sink)?;
        let vectors = if self.segment_index + 1 >= self.total_segment_count {
            self.total_vector_count - self.segment_index * self.vectors_per_segment
        } else {
            self.vectors_per_segment
        };
        self.segment = Some(ZstdSegment {
            buffer: vec![0; self.info.block_size],
            offset: vector_metadata_size(vectors),
            total_vectors: vectors,
            vectors: 0,
            count: 0,
            overflow_blocks: Vec::new(),
        });
        self.page = None;
        Ok(())
    }

    /// `NewPage`: continues on a fresh extra page whose id ends the current one.
    fn new_page(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let new_id = sink.writer.new_block()?;
        let (buffer, offset) = self.current()?;
        buffer[*offset..*offset + 8].copy_from_slice(&new_id.to_le_bytes());
        *offset += 8;
        if let Some(segment) = &mut self.segment {
            segment.overflow_blocks.push(new_id);
        }
        if let Some(page) = self.page.take() {
            sink.writer.write_block(page.block_id, page.buffer)?;
        }
        self.page = Some(ExtraPage {
            block_id: new_id,
            buffer: vec![0; self.info.block_size],
            offset: 0,
        });
        Ok(())
    }

    /// Writes the buffered vector (`InitializeVector`, `CompressString`, `FlushVector`).
    fn write_vector(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let tuple_count = self.lengths.len();
        if self.segment.is_none() {
            // The first segment (created by DuckDB's constructor).
            self.new_segment(sink)?;
        }
        // `AddNull` marks the segment before `InitializeVector` may start a new one.
        if self.has_null.first() == Some(&true) {
            self.stats.set_has_null_fast();
        }
        // `InitializeVector`.
        {
            let (_, offset) = self.current()?;
            *offset = offset.next_multiple_of(4);
        }
        let segment_full = self
            .segment
            .as_ref()
            .is_none_or(|segment| segment.vectors + 1 > segment.total_vectors);
        if segment_full {
            self.new_segment(sink)?;
        }
        let writable = self.writable();
        let (_, offset) = self.current()?;
        if *offset + tuple_count * 4 >= writable {
            self.new_page(sink)?;
        }
        let starting_page = self.page.as_ref().map_or(INVALID_BLOCK, |page| page.block_id);
        let lengths = std::mem::take(&mut self.lengths);
        let rows = std::mem::take(&mut self.rows);
        let starting_offset = {
            let (buffer, offset) = self.current()?;
            *offset = offset.next_multiple_of(4);
            let start = *offset;
            for (index, length) in lengths.iter().enumerate() {
                buffer[start + index * 4..start + index * 4 + 4].copy_from_slice(&length.to_le_bytes());
            }
            *offset += tuple_count * 4;
            start
        };
        // The stats of the vector's strings go to the current segment.
        let mut position = 0;
        for (row, (length, null)) in lengths.iter().zip(&self.has_null).enumerate() {
            let end = position + *length as usize;
            if *null {
                // The first row's NULL was recorded before the vector started.
                if row > 0 {
                    self.stats.set_has_null_fast();
                }
            } else {
                update_string_stats(&mut self.stats, &rows[position..end])?;
            }
            position = end;
        }
        let compressed =
            ruzstd::encoding::compress_to_vec(rows.as_slice(), ruzstd::encoding::CompressionLevel::Fastest);
        let mut written = 0;
        loop {
            let (buffer, offset) = self.current()?;
            let take = (writable - *offset).min(compressed.len() - written);
            buffer[*offset..*offset + take].copy_from_slice(&compressed[written..written + take]);
            *offset += take;
            written += take;
            if written == compressed.len() {
                break;
            }
            self.new_page(sink)?;
        }
        // `FlushVector`: the vector metadata.
        let segment = self
            .segment
            .as_mut()
            .ok_or_else(|| Error::internal("ZSTD compression without a segment"))?;
        let [page_ids, page_offsets, uncompressed_sizes, compressed_sizes] =
            metadata_offsets(segment.total_vectors);
        let index = segment.vectors;
        segment.buffer[page_ids + 8 * index..page_ids + 8 * index + 8]
            .copy_from_slice(&starting_page.to_le_bytes());
        segment.buffer[page_offsets + 4 * index..page_offsets + 4 * index + 4]
            .copy_from_slice(&(starting_offset as u32).to_le_bytes());
        segment.buffer[uncompressed_sizes + 8 * index..uncompressed_sizes + 8 * index + 8]
            .copy_from_slice(&(rows.len() as u64).to_le_bytes());
        segment.buffer[compressed_sizes + 8 * index..compressed_sizes + 8 * index + 8]
            .copy_from_slice(&(compressed.len() as u64).to_le_bytes());
        segment.vectors += 1;
        segment.count += tuple_count as u64;
        self.vectors_written += 1;
        self.rows = rows;
        self.rows.clear();
        self.lengths = lengths;
        self.lengths.clear();
        self.has_null.clear();
        Ok(())
    }

    /// `FlushSegment`.
    fn flush_segment(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        let Some(segment) = self.segment.take() else {
            return Ok(());
        };
        if segment.vectors != segment.total_vectors {
            return Err(Error::internal(format!(
                "(ZSTDCompressionState::FlushSegment) We haven't written all vectors that we were expecting to \
                 write ({} instead of {})!",
                segment.vectors, segment.total_vectors
            )));
        }
        if let Some(page) = self.page.take()
            && page.offset != 0
        {
            sink.writer.write_block(page.block_id, page.buffer)?;
        }
        let stats = std::mem::replace(&mut self.stats, BaseStatistics::create_empty(self.ty.clone()));
        sink.flush(FinishedSegment {
            data: segment.buffer[..segment.offset].to_vec(),
            count: segment.count,
            stats,
            compression: CompressionType::Zstd,
            overflow_blocks: segment.overflow_blocks,
        })?;
        self.segment_index += 1;
        Ok(())
    }

    /// The number of values of the current vector (`InitializeVector`'s expected count).
    fn expected_vector_size(&self) -> usize {
        if self.vectors_written + 1 >= self.total_vector_count {
            self.total_count - ZSTD_VECTOR_SIZE * self.vectors_written
        } else {
            ZSTD_VECTOR_SIZE
        }
    }
}

impl Compressor for ZstdCompressor {
    /// `ZSTDStorage::Compress`: strings are buffered until their vector is complete.
    fn compress(&mut self, sink: &mut SegmentSink<'_>, input: &Vector, count: usize) -> Result<()> {
        for string in unified_strings(input, count) {
            let bytes = string.unwrap_or_default();
            self.rows.extend_from_slice(bytes);
            self.lengths.push(bytes.len() as u32);
            self.has_null.push(string.is_none());
            if self.lengths.len() == self.expected_vector_size() {
                self.write_vector(sink)?;
            }
        }
        Ok(())
    }

    fn finalize(&mut self, sink: &mut SegmentSink<'_>) -> Result<()> {
        if !self.lengths.is_empty() {
            self.write_vector(sink)?;
        }
        self.flush_segment(sink)
    }
}

/// `ZSTDFun::GetFunction(type).init_analyze`: from storage version 4.
pub(crate) fn init_analyze(ty: &LogicalType, info: &CompressionInfo) -> Option<Box<dyn Analyzer>> {
    if info.storage_version < 4 {
        return None;
    }
    Some(Box::new(ZstdAnalyzer {
        ty: ty.clone(),
        info: *info,
        total_size: 0,
        count: 0,
        vectors_per_segment: 0,
        segment_count: 0,
        vectors_in_segment: 0,
        values_in_vector: 0,
    }))
}
