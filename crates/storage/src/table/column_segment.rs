//! A run of values of one column stored in one block (DuckDB's `ColumnSegment`).
//!
//! # Memory safety of block access
//! Segment bytes live in buffer-managed blocks and are read through raw pointers of a
//! pin ([`SegmentBuffer`]). A segment is appended to by one writer at a time (the owner
//! of the column's append state); concurrent readers only access rows below the
//! published `count`, whose bytes the writer never touches again (DuckDB's append-only
//! invariant). Writers write rows at and above `count` and publish them with a release
//! store; readers load `count` with acquire. The only exceptions — reverting appends and
//! compacting a segment at checkpoint — happen while no reader can observe the affected
//! rows.

use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustdb_buffer::{
    BlockHandle, BlockId, BlockManager, BufferHandle, BufferManager, INVALID_BLOCK, MemoryTag,
};
use rustdb_common::{Error, Result};
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, Vector};

use crate::compression::{CompressionType, constant, uncompressed};
use crate::statistics::BaseStatistics;

/// Whether a segment is still appendable memory or written to the database file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentType {
    Transient,
    Persistent,
}

/// A pinned view of a segment's bytes. Cloning shares the pin, so vectors holding strings
/// that point into the block can keep it loaded.
#[derive(Clone, Debug)]
pub struct SegmentBuffer {
    handle: Arc<BufferHandle>,
    offset: usize,
}

impl SegmentBuffer {
    pub(crate) fn new(handle: BufferHandle, offset: usize) -> Self {
        Self {
            handle: Arc::new(handle),
            offset,
        }
    }

    /// Bytes available from the segment start to the end of the block.
    pub fn len(&self) -> usize {
        self.handle.len().saturating_sub(self.offset)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check(&self, start: usize, len: usize) -> Result<()> {
        if start.checked_add(len).is_none_or(|end| end > self.len()) {
            return Err(Error::internal(format!(
                "segment access {start}+{len} out of bounds of a {} byte buffer",
                self.len()
            )));
        }
        Ok(())
    }

    /// Bytes `start..start + len` of the segment.
    ///
    /// # Safety
    /// No one may write these bytes while the returned slice lives (see the module
    /// documentation: only rows below a segment's published count may be read).
    pub(crate) unsafe fn bytes(&self, start: usize, len: usize) -> Result<&[u8]> {
        self.check(start, len)?;
        // SAFETY: in bounds of the pinned block (checked above); the pin keeps the memory
        // alive for the lifetime of `self`; absence of writers is the caller's contract.
        Ok(unsafe { std::slice::from_raw_parts(self.handle.as_ptr().add(self.offset + start), len) })
    }

    /// Copies `data` to `start..start + data.len()`.
    ///
    /// # Safety
    /// No one may read or write these bytes concurrently: they must lie at or after the
    /// published end of the segment, or the segment must not be shared yet.
    pub(crate) unsafe fn write(&self, start: usize, data: &[u8]) -> Result<()> {
        self.check(start, data.len())?;
        // SAFETY: in bounds (checked above); exclusivity is the caller's contract.
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                self.handle.as_ptr().add(self.offset + start),
                data.len(),
            );
        }
        Ok(())
    }

    /// Reads a little-endian `u32` at `start`.
    ///
    /// # Safety
    /// As for [`SegmentBuffer::bytes`].
    pub(crate) unsafe fn read_u32(&self, start: usize) -> Result<u32> {
        // SAFETY: forwarded contract.
        let bytes = unsafe { self.bytes(start, 4)? };
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Keeps the pinned block alive in `vector` (for strings pointing into it).
    pub(crate) fn keep_alive_in(&self, vector: &mut Vector) {
        let owner: Arc<dyn Any + Send + Sync> = self.handle.clone();
        vector.add_keep_alive(owner);
    }
}

/// An in-memory block holding uncompressed strings too large for the segment
/// (DuckDB's `StringBlock`).
#[derive(Debug)]
pub(crate) struct OverflowBlock {
    pub block: Arc<BlockHandle>,
    pub size: usize,
    pub used: usize,
}

/// Mutable state of a string segment: overflow storage for big strings.
#[derive(Debug, Default)]
pub(crate) struct StringSegmentState {
    /// In-memory overflow blocks, the one being filled last.
    pub memory_blocks: Vec<OverflowBlock>,
}

/// The per-scan state of a segment.
#[derive(Debug)]
pub enum SegmentScanState {
    /// A pin of the segment's block.
    Pinned(SegmentBuffer),
    /// The segment has no block (constant encodings).
    NoBlock,
}

/// Append state of a transient segment: its block, pinned for the append.
#[derive(Debug)]
pub struct SegmentAppendState {
    pub(crate) buffer: SegmentBuffer,
}

/// How values of a type are laid out in uncompressed segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// `width` bytes per row.
    Fixed(usize),
    /// One bit per row in 64-bit words (bit set = valid).
    Validity,
    /// A dictionary of string bytes with an offset per row.
    Strings,
}

impl Layout {
    pub(crate) fn of(ty: &LogicalType) -> Result<Self> {
        Ok(match ty.physical_type() {
            PhysicalType::Bit => Self::Validity,
            PhysicalType::Varchar => Self::Strings,
            // LIST columns store the cumulative end offset of each list as a u64.
            PhysicalType::List => Self::Fixed(8),
            PhysicalType::Struct | PhysicalType::Array | PhysicalType::Unknown | PhysicalType::Invalid => {
                return Err(Error::internal(format!("type {ty} has no segment layout")));
            }
            other => Self::Fixed(other.size()),
        })
    }
}

/// A segment of a column: a block (or nothing, for constant segments), the number of
/// rows it holds, and its statistics.
#[derive(Debug)]
pub struct ColumnSegment {
    ty: LogicalType,
    layout: Layout,
    compression: CompressionType,
    segment_type: SegmentType,
    block: Option<Arc<BlockHandle>>,
    block_id: BlockId,
    offset: usize,
    segment_size: usize,
    /// Usable size of a block of the segment's block manager.
    block_size: usize,
    /// Published row count; see the module documentation.
    count: AtomicU64,
    stats: Mutex<BaseStatistics>,
    string_state: Mutex<StringSegmentState>,
    buffer_manager: BufferManager,
}

impl ColumnSegment {
    /// An empty uncompressed segment of `segment_size` bytes in fresh memory
    /// (`ColumnSegment::CreateTransientSegment`).
    pub fn create_transient(
        buffer_manager: &BufferManager,
        block_manager: &dyn BlockManager,
        ty: LogicalType,
        segment_size: usize,
    ) -> Result<Self> {
        let layout = Layout::of(&ty)?;
        let block = if segment_size < block_manager.block_size() {
            buffer_manager.register_small_memory(MemoryTag::InMemoryTable, segment_size)?
        } else {
            let pin = buffer_manager.allocate_for(MemoryTag::InMemoryTable, block_manager, false)?;
            Arc::clone(pin.block())
        };
        let stats = BaseStatistics::create_empty(ty.clone());
        let segment = Self {
            ty,
            layout,
            compression: CompressionType::Uncompressed,
            segment_type: SegmentType::Transient,
            block: Some(block),
            block_id: INVALID_BLOCK,
            offset: 0,
            segment_size,
            block_size: block_manager.block_size(),
            count: AtomicU64::new(0),
            stats: Mutex::new(stats),
            string_state: Mutex::default(),
            buffer_manager: buffer_manager.clone(),
        };
        uncompressed::init_segment(&segment)?;
        Ok(segment)
    }

    /// A segment of `count` rows whose values are all described by `stats`
    /// (`COMPRESSION_CONSTANT`): every value equals the minimum, or all are NULL.
    pub fn create_constant(
        buffer_manager: &BufferManager,
        ty: LogicalType,
        count: u64,
        stats: BaseStatistics,
    ) -> Self {
        let layout = Layout::of(&ty).unwrap_or(Layout::Fixed(0));
        let compression = if ty.id() == LogicalTypeId::Validity && !stats.can_have_null() {
            CompressionType::Empty
        } else {
            CompressionType::Constant
        };
        Self {
            ty,
            layout,
            compression,
            segment_type: SegmentType::Persistent,
            block: None,
            block_id: INVALID_BLOCK,
            offset: 0,
            segment_size: 0,
            block_size: 0,
            count: AtomicU64::new(count),
            stats: Mutex::new(stats),
            string_state: Mutex::default(),
            buffer_manager: buffer_manager.clone(),
        }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub(crate) fn layout(&self) -> Layout {
        self.layout
    }

    pub fn compression(&self) -> CompressionType {
        self.compression
    }

    pub fn segment_type(&self) -> SegmentType {
        self.segment_type
    }

    /// Published row count.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    pub(crate) fn set_count(&self, count: u64) {
        self.count.store(count, Ordering::Release);
    }

    pub fn segment_size(&self) -> usize {
        self.segment_size
    }

    /// Usable size of a block of the segment's block manager.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn block_id(&self) -> BlockId {
        self.block_id
    }

    pub fn block_offset(&self) -> usize {
        self.offset
    }

    pub fn block(&self) -> Option<&Arc<BlockHandle>> {
        self.block.as_ref()
    }

    pub(crate) fn buffer_manager(&self) -> &BufferManager {
        &self.buffer_manager
    }

    /// The segment's zone map.
    pub fn statistics(&self) -> MutexGuard<'_, BaseStatistics> {
        self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn string_state(&self) -> MutexGuard<'_, StringSegmentState> {
        self.string_state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Pins the segment's block.
    pub(crate) fn pin(&self) -> Result<SegmentBuffer> {
        let block = self
            .block
            .as_ref()
            .ok_or_else(|| Error::internal("attempting to pin a segment without a block"))?;
        Ok(SegmentBuffer::new(self.buffer_manager.pin(block)?, self.offset))
    }

    /// Prepares a scan of this segment (`InitializeScan`).
    pub fn init_scan(&self) -> Result<SegmentScanState> {
        match self.compression {
            CompressionType::Constant | CompressionType::Empty => Ok(SegmentScanState::NoBlock),
            CompressionType::Uncompressed => Ok(SegmentScanState::Pinned(self.pin()?)),
            other => Err(Error::not_implemented(format!("scanning {other} segments"))),
        }
    }

    /// Scans rows `start..start + count` of the segment into `result` (flat) starting at
    /// row `result_offset` (`ScanPartial`).
    pub fn scan(
        &self,
        state: &mut SegmentScanState,
        start: u64,
        count: usize,
        result: &mut Vector,
        result_offset: usize,
    ) -> Result<()> {
        match (self.compression, state) {
            (CompressionType::Constant | CompressionType::Empty, _) => {
                constant::scan(self, count, result, result_offset)
            }
            (CompressionType::Uncompressed, SegmentScanState::Pinned(buffer)) => {
                uncompressed::scan(self, buffer, start as usize, count, result, result_offset)
            }
            (other, _) => Err(Error::not_implemented(format!("scanning {other} segments"))),
        }
    }

    /// Reads row `row` of the segment into `result[result_index]` (`FetchRow`).
    pub fn fetch_row(&self, row: u64, result: &mut Vector, result_index: usize) -> Result<()> {
        if row > self.count() {
            return Err(Error::internal(
                "ColumnSegment::FetchRow - row_id out of range for segment",
            ));
        }
        match self.compression {
            CompressionType::Constant | CompressionType::Empty => {
                constant::scan(self, 1, result, result_index)
            }
            CompressionType::Uncompressed => {
                let buffer = self.pin()?;
                uncompressed::scan(self, &buffer, row as usize, 1, result, result_index)
            }
            other => Err(Error::not_implemented(format!("fetching from {other} segments"))),
        }
    }

    /// Pins the segment for appending (`InitializeAppend`).
    pub fn init_append(&self) -> Result<SegmentAppendState> {
        if self.segment_type != SegmentType::Transient || self.compression != CompressionType::Uncompressed {
            return Err(Error::internal(
                "Attempting to init append to a segment without init_append method",
            ));
        }
        Ok(SegmentAppendState { buffer: self.pin()? })
    }

    /// Appends rows `offset..offset + count` of `vector`; returns how many fit
    /// (`Append`). The segment statistics are updated.
    pub fn append(
        &self,
        state: &mut SegmentAppendState,
        vector: &Vector,
        offset: usize,
        count: usize,
    ) -> Result<usize> {
        let mut stats = self.statistics();
        uncompressed::append(self, &state.buffer, &mut stats, vector, offset, count)
    }

    /// Truncates the segment to `new_count` rows (`RevertAppend`).
    pub fn revert_append(&self, new_count: u64) -> Result<()> {
        if self.segment_type != SegmentType::Transient {
            return Err(Error::internal("reverting an append to a persistent segment"));
        }
        uncompressed::revert_append(self, new_count)?;
        self.set_count(new_count);
        Ok(())
    }
}
