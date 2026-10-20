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

use crate::compression::{self, CompressionType, SegmentScan, constant, uncompressed};
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

/// A pin of a persistent segment's block, whose bytes never change once the segment is
/// loaded from (or written to) the database file. Compressed segments are only read
/// through this view.
#[derive(Clone, Debug)]
pub(crate) struct PersistentData {
    buffer: SegmentBuffer,
}

impl PersistentData {
    /// The bytes from the start of the segment to the end of its block.
    pub(crate) fn bytes(&self) -> &[u8] {
        let len = self.buffer.len();
        // SAFETY: persistent blocks are immutable (only transient segments are appended
        // to), and the pin held by `self.buffer` keeps the memory alive for `&self`.
        unsafe { self.buffer.bytes(0, len) }.unwrap_or(&[])
    }
}

impl AsRef<[u8]> for PersistentData {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
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
    /// The scan state of a compressed segment.
    Compressed(Box<dyn SegmentScan>),
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
    /// The block manager of a persistent segment, for its overflow string blocks.
    block_manager: Option<Arc<dyn BlockManager>>,
    /// The on-disk overflow string blocks of a persistent string segment
    /// (`UncompressedStringSegmentState::on_disk_blocks`).
    overflow_blocks: Vec<BlockId>,
}

impl ColumnSegment {
    /// A segment stored in the database file (`ColumnSegment::CreatePersistentSegment`):
    /// uncompressed segments refer to their block, constant ones need none.
    #[allow(clippy::too_many_arguments)]
    pub fn create_persistent(
        buffer_manager: &BufferManager,
        block_manager: &Arc<dyn BlockManager>,
        block_id: BlockId,
        offset: usize,
        ty: LogicalType,
        count: u64,
        compression: CompressionType,
        stats: BaseStatistics,
        overflow_blocks: Vec<BlockId>,
    ) -> Result<Self> {
        match compression {
            CompressionType::Constant | CompressionType::Empty => {
                let mut segment = Self::create_constant(buffer_manager, ty, count, stats);
                segment.compression = compression;
                Ok(segment)
            }
            CompressionType::Auto | CompressionType::PforDelta => Err(Error::not_implemented(format!(
                "reading {compression} compressed segments"
            ))),
            _ => {
                let layout = Layout::of(&ty)?;
                let block = buffer_manager.register_block(block_manager, block_id);
                block.release_on_unpin();
                Ok(Self {
                    ty,
                    layout,
                    compression,
                    segment_type: SegmentType::Persistent,
                    block: Some(block),
                    block_id,
                    offset,
                    segment_size: block_manager.block_size() - offset,
                    block_size: block_manager.block_size(),
                    count: AtomicU64::new(count),
                    stats: Mutex::new(stats),
                    string_state: Mutex::default(),
                    buffer_manager: buffer_manager.clone(),
                    block_manager: Some(Arc::clone(block_manager)),
                    overflow_blocks,
                })
            }
        }
    }

    /// The block manager of a persistent segment.
    pub(crate) fn persistent_block_manager(&self) -> Option<&Arc<dyn BlockManager>> {
        self.block_manager.as_ref()
    }

    /// The on-disk overflow string blocks of a persistent string segment.
    pub fn overflow_blocks(&self) -> &[BlockId] {
        &self.overflow_blocks
    }
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
            block_manager: None,
            overflow_blocks: Vec::new(),
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
            block_manager: None,
            overflow_blocks: Vec::new(),
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

    /// Pins the block of a persistent segment for reading.
    pub(crate) fn pin_persistent(&self) -> Result<PersistentData> {
        if self.segment_type != SegmentType::Persistent {
            return Err(Error::internal("reading a compressed transient segment"));
        }
        Ok(PersistentData { buffer: self.pin()? })
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
            _ => Ok(SegmentScanState::Compressed(compression::init_scan(self)?)),
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
            (_, SegmentScanState::Compressed(state)) => {
                state.scan(self, start as usize, count, result, result_offset)
            }
            (other, _) => Err(Error::internal(format!(
                "scan state does not match the {other} segment"
            ))),
        }
    }

    /// Reads row `row` of the segment into `result[result_index]` (`FetchRow`).
    pub fn fetch_row(&self, row: u64, result: &mut Vector, result_index: usize) -> Result<()> {
        if row > self.count() {
            return Err(Error::internal(
                "ColumnSegment::FetchRow - row_id out of range for segment",
            ));
        }
        // Rows fetched by id (late materialisation, index lookups) are read a few at a
        // time and again by later queries: their blocks stay cached like DuckDB's.
        if let Some(block) = &self.block {
            block.keep_cached();
        }
        match self.compression {
            CompressionType::Constant | CompressionType::Empty => {
                constant::scan(self, 1, result, result_index)
            }
            CompressionType::Uncompressed => {
                let buffer = self.pin()?;
                uncompressed::scan(self, &buffer, row as usize, 1, result, result_index)
            }
            _ => compression::fetch_row(self, row as usize, result, result_index),
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
