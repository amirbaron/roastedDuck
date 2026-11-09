//! Segments of a collection and the allocation of their rows (DuckDB's
//! `TupleDataSegment` and `TupleDataAllocator`).
//!
//! A segment owns row blocks (fixed-width rows) and heap blocks (variable-size data),
//! all buffer-managed. Rows are grouped into chunks of up to `STANDARD_VECTOR_SIZE` rows;
//! a chunk consists of parts, each a run of consecutive rows in one row block (and their
//! heap data in one heap block).
//!
//! Rows hold absolute pointers into heap blocks. When a heap block is evicted and loaded
//! again at another address, the pointers of the affected rows are rewritten on the next
//! pin (`RecomputeHeapPointers`), so evicting row-format data is transparent.

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roastedduck_buffer::{BlockHandle, BufferHandle, BufferManager, DestroyBufferUpon, MemoryTag};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_types::PhysicalType;

use crate::layout::{HEAP_SIZE_WIDTH, TupleDataLayout};
use crate::radix::RadixPartitioning;
use crate::row::{RowPtr, is_valid, load, store};
use crate::sort::key::SortKeyType;
use crate::state::{ChunkState, IdRange, PinProperties, PinState};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Everything segment operations need to know about the owning collection.
#[derive(Clone, Copy)]
pub(crate) struct SegmentContext<'a> {
    pub(crate) buffer_manager: &'a BufferManager,
    pub(crate) layout: &'a TupleDataLayout,
    pub(crate) tag: MemoryTag,
    pub(crate) partition_index: Option<usize>,
}

/// A buffer-managed block holding rows or heap data.
pub(crate) struct DataBlock {
    /// `None` once destroyed.
    handle: Mutex<Option<Arc<BlockHandle>>>,
    capacity: usize,
    size: usize,
}

impl DataBlock {
    fn remaining(&self) -> usize {
        self.capacity - self.size
    }

    fn handle(&self) -> Option<Arc<BlockHandle>> {
        lock(&self.handle).clone()
    }

    fn set_destroy_upon_unpin(&self) {
        if let Some(handle) = &*lock(&self.handle) {
            handle.set_destroy_buffer_upon(DestroyBufferUpon::Unpin);
        }
    }

    fn destroy(&self) {
        if let Some(handle) = lock(&self.handle).take() {
            handle.set_destroy_buffer_upon(DestroyBufferUpon::Unpin);
        }
    }
}

const NO_HEAP: u32 = u32::MAX;

/// A run of consecutive rows in one row block (`TupleDataChunkPart`).
pub(crate) struct ChunkPart {
    pub(crate) row_block_index: u32,
    pub(crate) row_block_offset: u32,
    pub(crate) heap_block_index: u32,
    pub(crate) heap_block_offset: u32,
    /// Where the heap block was when the row pointers were last written.
    base_heap_ptr: AtomicPtr<u8>,
    pub(crate) total_heap_size: usize,
    pub(crate) count: u32,
}

impl ChunkPart {
    fn has_heap(&self) -> bool {
        self.total_heap_size != 0
    }
}

/// Up to `STANDARD_VECTOR_SIZE` rows made of consecutive parts (`TupleDataChunk`).
pub(crate) struct Chunk {
    pub(crate) part_ids: IdRange,
    pub(crate) row_block_ids: IdRange,
    pub(crate) heap_block_ids: IdRange,
    pub(crate) count: u32,
    /// Serialises heap pointer recomputation for this chunk's rows.
    lock: Mutex<()>,
}

impl Chunk {
    fn new() -> Self {
        Self {
            part_ids: IdRange::EMPTY,
            row_block_ids: IdRange::EMPTY,
            heap_block_ids: IdRange::EMPTY,
            count: 0,
            lock: Mutex::new(()),
        }
    }
}

/// A contiguous piece of a collection with its own blocks (`TupleDataSegment`).
pub(crate) struct Segment {
    pub(crate) row_blocks: Vec<DataBlock>,
    pub(crate) heap_blocks: Vec<DataBlock>,
    pub(crate) chunks: Vec<Chunk>,
    pub(crate) parts: Vec<ChunkPart>,
    pub(crate) count: usize,
    pub(crate) data_size: usize,
    /// Pins stored by `PinProperties::KeepEverythingPinned`.
    pinned_rows: Mutex<Vec<Option<Arc<BufferHandle>>>>,
    pinned_heaps: Mutex<Vec<Option<Arc<BufferHandle>>>>,
}

/// Which kind of block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Row,
    Heap,
}

impl Segment {
    pub(crate) fn new() -> Self {
        Self {
            row_blocks: Vec::new(),
            heap_blocks: Vec::new(),
            chunks: Vec::new(),
            parts: Vec::new(),
            count: 0,
            data_size: 0,
            pinned_rows: Mutex::new(Vec::new()),
            pinned_heaps: Mutex::new(Vec::new()),
        }
    }

    fn blocks(&self, kind: BlockKind) -> &[DataBlock] {
        match kind {
            BlockKind::Row => &self.row_blocks,
            BlockKind::Heap => &self.heap_blocks,
        }
    }

    /// Allocates a block and keeps its pin in `pin_state`.
    fn create_block(
        &mut self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        kind: BlockKind,
        size: usize,
    ) -> Result<()> {
        let pin = ctx.buffer_manager.allocate(ctx.tag, size, false)?;
        if let Some(partition) = ctx.partition_index {
            // Larger partitions (more radix bits) are evicted first.
            pin.block()
                .set_eviction_queue_index(RadixPartitioning::radix_bits(partition));
        }
        let block = DataBlock {
            handle: Mutex::new(Some(Arc::clone(pin.block()))),
            capacity: pin.len(),
            size: 0,
        };
        let (blocks, pinned, handles) = match kind {
            BlockKind::Row => (
                &mut self.row_blocks,
                &self.pinned_rows,
                &mut pin_state.row_handles,
            ),
            BlockKind::Heap => (
                &mut self.heap_blocks,
                &self.pinned_heaps,
                &mut pin_state.heap_handles,
            ),
        };
        let index = blocks.len() as u32;
        blocks.push(block);
        lock(pinned).push(None);
        handles.push((index, Arc::new(pin)));
        Ok(())
    }

    /// Pins block `index` (or finds its pin in `pin_state`) and returns its address.
    fn pin_block(
        &self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        kind: BlockKind,
        index: u32,
    ) -> Result<*mut u8> {
        let handles = match kind {
            BlockKind::Row => &mut pin_state.row_handles,
            BlockKind::Heap => &mut pin_state.heap_handles,
        };
        if let Some((_, pin)) = handles.iter().find(|(id, _)| *id == index) {
            return Ok(pin.as_ptr());
        }
        let block = self.blocks(kind)[index as usize]
            .handle()
            .ok_or_else(|| Error::internal("row data was destroyed before it was read"))?;
        let pin = ctx.buffer_manager.pin(&block)?;
        let ptr = pin.as_ptr();
        handles.push((index, Arc::new(pin)));
        Ok(ptr)
    }

    fn row_pointer(
        &self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        part: &ChunkPart,
    ) -> Result<*mut u8> {
        let base = self.pin_block(ctx, pin_state, BlockKind::Row, part.row_block_index)?;
        // SAFETY: the part lies within its row block.
        Ok(unsafe { base.add(part.row_block_offset as usize) })
    }

    /// Reserves room for `count` rows starting at row `offset` of `chunk_state`, setting
    /// their row and heap locations (`TupleDataAllocator::Build`).
    pub(crate) fn build(
        &mut self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        chunk_state: &mut ChunkState,
        offset: usize,
        count: usize,
    ) -> Result<()> {
        if let Some(last) = self.chunks.last() {
            let (rows, heaps) = (last.row_block_ids, last.heap_block_ids);
            self.release_or_store_handles(pin_state, rows, heaps, true);
        }
        if self.build_fast_path(ctx, pin_state, chunk_state, offset, count)? {
            return Ok(());
        }
        chunk_state.part_indices.clear();
        let row_width = ctx.layout.row_width();
        let mut appended = 0;
        while appended != count {
            if self
                .chunks
                .last()
                .is_none_or(|chunk| chunk.count as usize == STANDARD_VECTOR_SIZE)
            {
                self.chunks.push(Chunk::new());
            }
            let chunk_index = self.chunks.len() - 1;
            let room = STANDARD_VECTOR_SIZE - self.chunks[chunk_index].count as usize;
            let next = (count - appended).min(room);
            let part = self.build_chunk_part(ctx, pin_state, chunk_state, offset + appended, next)?;
            let part_count = part.count as usize;
            self.count += part_count;
            self.data_size += part_count * row_width + part.total_heap_size;
            if ctx.layout.has_destructor() {
                zero_aggregate_states(ctx.layout, self.row_pointer(ctx, pin_state, &part)?, part_count);
            }
            let part_index = self.parts.len();
            let chunk = &mut self.chunks[chunk_index];
            chunk.count += part.count;
            chunk.row_block_ids.insert(part.row_block_index);
            if part.has_heap() {
                chunk.heap_block_ids.insert(part.heap_block_index);
            }
            chunk.part_ids.insert(part_index as u32);
            self.parts.push(part);
            appended += part_count;
            chunk_state.part_indices.push((chunk_index, part_index));
        }
        let parts: Vec<usize> = chunk_state.part_indices.iter().map(|&(_, part)| part).collect();
        self.initialize_parts(
            ctx,
            pin_state,
            chunk_state,
            offset,
            false,
            true,
            false,
            &parts,
            None,
        )?;
        // Only the last part of the first chunk can be merged with its predecessor.
        if let Some(&(first_chunk, _)) = chunk_state.part_indices.first() {
            self.merge_last_chunk_part(first_chunk, row_width, ctx.layout.all_constant());
        }
        Ok(())
    }

    /// Extends the last part in place when possible (constant-size rows only).
    fn build_fast_path(
        &mut self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        chunk_state: &mut ChunkState,
        offset: usize,
        count: usize,
    ) -> Result<bool> {
        let layout = ctx.layout;
        if !layout.all_constant() || layout.has_destructor() {
            return Ok(false);
        }
        let Some(chunk) = self.chunks.last() else {
            return Ok(false);
        };
        if chunk.count as usize + count > STANDARD_VECTOR_SIZE {
            return Ok(false);
        }
        let part_index = chunk.part_ids.end() as usize - 1;
        let row_width = layout.row_width();
        let part = &self.parts[part_index];
        let block = &self.row_blocks[part.row_block_index as usize];
        if block.size + count * row_width > block.capacity {
            return Ok(false);
        }
        let base = self.row_pointer(ctx, pin_state, part)?;
        let start = part.count as usize * row_width;
        for (i, location) in chunk_state.row_locations[offset..offset + count]
            .iter_mut()
            .enumerate()
        {
            // SAFETY: the rows fit in the row block (checked above).
            *location = RowPtr::from_ptr(unsafe { base.add(start + i * row_width) });
        }
        let row_block_index = part.row_block_index as usize;
        if let Some(chunk) = self.chunks.last_mut() {
            chunk.count += count as u32;
        }
        self.parts[part_index].count += count as u32;
        self.row_blocks[row_block_index].size += count * row_width;
        self.count += count;
        self.data_size += count * row_width;
        Ok(true)
    }

    /// Carves out one part for up to `count` rows (`BuildChunkPart`).
    fn build_chunk_part(
        &mut self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        chunk_state: &ChunkState,
        offset: usize,
        count: usize,
    ) -> Result<ChunkPart> {
        let layout = ctx.layout;
        let row_width = layout.row_width();
        let block_size = ctx.buffer_manager.block_size();
        if self
            .row_blocks
            .last()
            .is_none_or(|block| block.remaining() < row_width)
        {
            self.create_block(ctx, pin_state, BlockKind::Row, block_size)?;
        }
        let row_block_index = self.row_blocks.len() - 1;
        let row_block = &self.row_blocks[row_block_index];
        let mut part = ChunkPart {
            row_block_index: row_block_index as u32,
            row_block_offset: row_block.size as u32,
            heap_block_index: NO_HEAP,
            heap_block_offset: NO_HEAP,
            base_heap_ptr: AtomicPtr::new(std::ptr::null_mut()),
            total_heap_size: 0,
            count: (row_block.remaining() / row_width).min(count) as u32,
        };
        if !layout.all_constant() {
            let sizes = &chunk_state.heap_sizes[offset..offset + part.count as usize];
            let total: usize = sizes.iter().sum();
            if total != 0 {
                let first = sizes[0];
                let heap_remaining = match self.heap_blocks.last() {
                    Some(block) if block.remaining() >= first => block.remaining(),
                    _ => block_size.max(first),
                };
                if total <= heap_remaining {
                    part.total_heap_size = total;
                } else {
                    let mut fitting = 0;
                    for (i, &size) in sizes.iter().enumerate() {
                        if fitting + size > heap_remaining {
                            part.count = i as u32;
                            break;
                        }
                        fitting += size;
                    }
                    part.total_heap_size = fitting;
                }
                if part.total_heap_size != 0 {
                    if self
                        .heap_blocks
                        .last()
                        .is_none_or(|block| block.remaining() < first)
                    {
                        self.create_block(ctx, pin_state, BlockKind::Heap, block_size.max(first))?;
                    }
                    let heap_block_index = self.heap_blocks.len() - 1;
                    let heap_block = &mut self.heap_blocks[heap_block_index];
                    part.heap_block_index = heap_block_index as u32;
                    part.heap_block_offset = heap_block.size as u32;
                    heap_block.size += part.total_heap_size;
                    let base = self.pin_block(ctx, pin_state, BlockKind::Heap, part.heap_block_index)?;
                    part.base_heap_ptr.store(base, Ordering::Relaxed);
                }
            }
        }
        debug_assert!(part.count > 0);
        self.row_blocks[row_block_index].size += part.count as usize * row_width;
        Ok(part)
    }

    /// Merges the last part of chunk `chunk_index` into its predecessor when they are
    /// adjacent in both row and heap blocks (`MergeLastChunkPart`).
    fn merge_last_chunk_part(&mut self, chunk_index: usize, row_width: usize, all_constant: bool) {
        let chunk = &self.chunks[chunk_index];
        if chunk.part_ids.len() < 2 {
            return;
        }
        let end = chunk.part_ids.end() as usize;
        let (previous, last) = (&self.parts[end - 2], &self.parts[end - 1]);
        let rows_align = last.row_block_index == previous.row_block_index
            && last.row_block_offset as usize
                == previous.row_block_offset as usize + previous.count as usize * row_width;
        if !rows_align {
            return;
        }
        let heaps_align = all_constant
            || (last.heap_block_index == previous.heap_block_index
                && last.heap_block_offset as usize
                    == (previous.heap_block_offset as usize).wrapping_add(previous.total_heap_size)
                && last.base_heap_ptr.load(Ordering::Relaxed)
                    == previous.base_heap_ptr.load(Ordering::Relaxed));
        if !heaps_align {
            return;
        }
        let (count, heap) = (last.count, last.total_heap_size);
        let previous = &mut self.parts[end - 2];
        previous.count += count;
        previous.total_heap_size += heap;
        if self.parts.len() == end {
            self.parts.pop();
        }
        self.chunks[chunk_index].part_ids.decrement_end();
    }

    /// Fills `chunk_state` with the row (and optionally heap) locations of chunk
    /// `chunk_index`, pinning its blocks (`InitializeChunkState`).
    pub(crate) fn initialize_chunk_state(
        &self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        chunk_state: &mut ChunkState,
        chunk_index: usize,
        init_heap: bool,
    ) -> Result<usize> {
        let chunk = &self.chunks[chunk_index];
        // Keep the heap pinned when this chunk has no heap: a later chunk may need the same
        // heap block again, and destroying it now would lose its data.
        self.release_or_store_handles(
            pin_state,
            chunk.row_block_ids,
            chunk.heap_block_ids,
            !chunk.heap_block_ids.is_empty(),
        );
        let parts: Vec<usize> = (chunk.part_ids.start()..chunk.part_ids.end())
            .map(|id| id as usize)
            .collect();
        self.initialize_parts(
            ctx,
            pin_state,
            chunk_state,
            0,
            true,
            init_heap,
            init_heap,
            &parts,
            Some(&chunk.lock),
        )?;
        Ok(chunk.count as usize)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the flags select DuckDB's variants of the same routine"
    )]
    fn initialize_parts(
        &self,
        ctx: SegmentContext<'_>,
        pin_state: &mut PinState,
        chunk_state: &mut ChunkState,
        mut offset: usize,
        recompute: bool,
        init_heap_pointers: bool,
        init_heap_sizes: bool,
        parts: &[usize],
        chunk_lock: Option<&Mutex<()>>,
    ) -> Result<()> {
        let layout = ctx.layout;
        let row_width = layout.row_width();
        for &part_index in parts {
            let part = &self.parts[part_index];
            let count = part.count as usize;
            let base = self.row_pointer(ctx, pin_state, part)?;
            for (i, location) in chunk_state.row_locations[offset..offset + count]
                .iter_mut()
                .enumerate()
            {
                // SAFETY: the part's rows lie within its pinned row block.
                *location = RowPtr::from_ptr(unsafe { base.add(i * row_width) });
            }
            if layout.all_constant() {
                offset += count;
                continue;
            }
            let rows = &chunk_state.row_locations[offset..offset + count];
            if !part.has_heap() {
                if init_heap_sizes {
                    // SAFETY: the rows are pinned rows of the layout.
                    unsafe {
                        read_heap_sizes(layout, rows, &mut chunk_state.heap_sizes[offset..offset + count]);
                    };
                }
                offset += count;
                continue;
            }
            if recompute && pin_state.properties != PinProperties::AlreadyPinned {
                let new_base = self.pin_block(ctx, pin_state, BlockKind::Heap, part.heap_block_index)?;
                if part.base_heap_ptr.load(Ordering::Acquire) != new_base {
                    let _guard = chunk_lock.map(lock);
                    let old_base = part.base_heap_ptr.load(Ordering::Acquire);
                    if old_base != new_base {
                        let heap_offset = part.heap_block_offset as usize;
                        // SAFETY: the rows and the heap block are pinned; the rows' pointers
                        // refer to the heap block's previous address.
                        unsafe {
                            recompute_heap_pointers(
                                layout,
                                rows,
                                HeapBases::Constant(
                                    old_base.wrapping_add(heap_offset),
                                    new_base.add(heap_offset),
                                ),
                                0,
                            );
                        }
                        part.base_heap_ptr.store(new_base, Ordering::Release);
                    }
                }
            }
            if init_heap_sizes {
                // SAFETY: the rows are pinned rows of the layout.
                unsafe { read_heap_sizes(layout, rows, &mut chunk_state.heap_sizes[offset..offset + count]) };
            }
            if init_heap_pointers {
                let mut heap = part
                    .base_heap_ptr
                    .load(Ordering::Acquire)
                    .wrapping_add(part.heap_block_offset as usize);
                for i in 0..count {
                    chunk_state.heap_locations[offset + i] = RowPtr::from_ptr(heap);
                    heap = heap.wrapping_add(chunk_state.heap_sizes[offset + i]);
                }
            }
            offset += count;
        }
        Ok(())
    }

    /// Drops or stores the pins in `pin_state` that are not among `rows` / `heaps`
    /// (`ReleaseOrStoreHandles`). Pass empty ranges to release everything.
    pub(crate) fn release_or_store_handles(
        &self,
        pin_state: &mut PinState,
        rows: IdRange,
        heaps: IdRange,
        release_heap: bool,
    ) {
        let properties = pin_state.properties;
        self.release_handles(&mut pin_state.row_handles, rows, BlockKind::Row, properties);
        if release_heap {
            self.release_handles(&mut pin_state.heap_handles, heaps, BlockKind::Heap, properties);
        }
    }

    fn release_handles(
        &self,
        handles: &mut Vec<(u32, Arc<BufferHandle>)>,
        keep: IdRange,
        kind: BlockKind,
        properties: PinProperties,
    ) {
        if handles.iter().all(|(id, _)| keep.contains(*id)) {
            return;
        }
        let mut kept = Vec::with_capacity(handles.len());
        for (id, pin) in handles.drain(..) {
            if keep.contains(id) {
                kept.push((id, pin));
                continue;
            }
            match properties {
                PinProperties::KeepEverythingPinned => {
                    let pinned = match kind {
                        BlockKind::Row => &self.pinned_rows,
                        BlockKind::Heap => &self.pinned_heaps,
                    };
                    lock(pinned)[id as usize] = Some(pin);
                }
                PinProperties::UnpinAfterDone | PinProperties::AlreadyPinned => drop(pin),
                PinProperties::DestroyAfterDone => {
                    self.blocks(kind)[id as usize].destroy();
                    drop(pin);
                }
            }
        }
        *handles = kept;
    }

    /// Releases every stored pin.
    pub(crate) fn unpin(&self) {
        lock(&self.pinned_rows).iter_mut().for_each(|pin| *pin = None);
        lock(&self.pinned_heaps).iter_mut().for_each(|pin| *pin = None);
    }

    /// Addresses of all row blocks; every block must be pinned (`KeepEverythingPinned`).
    pub(crate) fn row_block_pointers(&self) -> Result<Vec<*mut u8>> {
        lock(&self.pinned_rows)
            .iter()
            .map(|pin| {
                pin.as_ref().map(|pin| pin.as_ptr()).ok_or_else(|| {
                    Error::internal("row block pointers requested but not every block is pinned")
                })
            })
            .collect()
    }

    /// Clones of the stored heap pins.
    pub(crate) fn pinned_heaps(&self) -> Vec<Arc<BufferHandle>> {
        lock(&self.pinned_heaps).iter().flatten().cloned().collect()
    }

    /// Lets the row (and heap) blocks in the given ranges be destroyed when unpinned.
    pub(crate) fn destroy_row_blocks(&self, begin: usize, end: usize) {
        for block in &self.row_blocks[begin.min(end)..end.min(self.row_blocks.len())] {
            block.set_destroy_upon_unpin();
        }
    }

    pub(crate) fn destroy_heap_blocks(&self, begin: usize, end: usize) {
        for block in &self.heap_blocks[begin.min(end)..end.min(self.heap_blocks.len())] {
            block.set_destroy_upon_unpin();
        }
    }

    /// Marks every block destroy-on-unpin, so dropping the segment never spills.
    pub(crate) fn set_destroy_buffer_upon_unpin(&self) {
        self.destroy_row_blocks(0, self.row_blocks.len());
        self.destroy_heap_blocks(0, self.heap_blocks.len());
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // Blocks of a dropped segment need not be written to temporary files.
        self.set_destroy_buffer_upon_unpin();
    }
}

/// Zeroes the aggregate states of `count` rows at `base` (destructor safety).
fn zero_aggregate_states(layout: &TupleDataLayout, base: *mut u8, count: usize) {
    let row_width = layout.row_width();
    let start = layout.aggr_offset();
    for i in 0..count {
        // SAFETY: the rows were just allocated in a pinned block.
        unsafe { std::ptr::write_bytes(base.add(i * row_width + start), 0, layout.aggr_width()) };
    }
}

/// Reads the stored heap size of each row.
///
/// # Safety
/// `rows` must be pinned rows of `layout`.
pub(crate) unsafe fn read_heap_sizes(layout: &TupleDataLayout, rows: &[RowPtr], sizes: &mut [usize]) {
    let offset = layout.heap_size_offset();
    // SAFETY: guaranteed by the caller.
    unsafe {
        if let Some(key) = layout.sort_key_type() {
            debug_assert!(!key.is_constant_size());
            for (size, row) in sizes.iter_mut().zip(rows) {
                *size = load::<u64>(row.as_ptr().add(offset)) as usize;
            }
            return;
        }
        for (size, row) in sizes.iter_mut().zip(rows) {
            *size = load::<u32>(row.as_ptr().add(offset)) as usize;
        }
    }
    debug_assert_eq!(HEAP_SIZE_WIDTH, size_of::<u32>());
}

/// Old and new heap addresses for pointer recomputation.
#[derive(Clone, Copy)]
pub(crate) enum HeapBases<'a> {
    /// The same bases for every row.
    Constant(*mut u8, *mut u8),
    /// Per-row bases.
    PerRow(&'a [RowPtr], &'a [RowPtr]),
}

impl HeapBases<'_> {
    #[inline]
    fn get(self, i: usize) -> (*mut u8, *mut u8) {
        match self {
            Self::Constant(old, new) => (old, new),
            Self::PerRow(old, new) => (old[i].as_ptr(), new[i].as_ptr()),
        }
    }
}

/// Rewrites heap pointers of `rows` from the old to the new heap location
/// (`RecomputeHeapPointers`). `base` is the offset of the (struct) layout within the row.
///
/// # Safety
/// `rows` must be writable rows of `layout` whose pointers refer to the old heap
/// locations.
pub(crate) unsafe fn recompute_heap_pointers(
    layout: &TupleDataLayout,
    rows: &[RowPtr],
    bases: HeapBases<'_>,
    base: usize,
) {
    // SAFETY (whole function): guaranteed by the caller.
    unsafe {
        if let Some(key) = layout.sort_key_type() {
            if let Some(data_offset) = key.data_pointer_offset() {
                for (i, row) in rows.iter().enumerate() {
                    let (old, new) = bases.get(i);
                    let location = row.as_ptr().add(data_offset);
                    let pointer = load::<u64>(location) as usize;
                    if pointer != 0 {
                        store(
                            new.wrapping_add(pointer.wrapping_sub(old as usize)) as u64,
                            location,
                        );
                    }
                }
            }
            return;
        }
        let all_valid = layout.all_valid();
        for &col in layout.variable_columns() {
            let offset = base + layout.offsets()[col];
            match layout.types()[col].physical_type() {
                PhysicalType::Varchar => {
                    for (i, row) in rows.iter().enumerate() {
                        if !all_valid && !is_valid(row.as_ptr().add(base), col) {
                            continue;
                        }
                        let location = row.as_ptr().add(offset);
                        if load::<u32>(location) as usize > roastedduck_types::StringT::INLINE_LENGTH {
                            let (old, new) = bases.get(i);
                            let pointer = load::<u64>(location.add(8)) as usize;
                            store(
                                new.wrapping_add(pointer.wrapping_sub(old as usize)) as u64,
                                location.add(8),
                            );
                        }
                    }
                }
                PhysicalType::List | PhysicalType::Array => {
                    for (i, row) in rows.iter().enumerate() {
                        if !all_valid && !is_valid(row.as_ptr().add(base), col) {
                            continue;
                        }
                        let (old, new) = bases.get(i);
                        let location = row.as_ptr().add(offset);
                        let pointer = load::<u64>(location) as usize;
                        store(
                            new.wrapping_add(pointer.wrapping_sub(old as usize)) as u64,
                            location,
                        );
                    }
                }
                PhysicalType::Struct => {
                    if let Some(struct_layout) = layout.struct_layout(col)
                        && !struct_layout.all_constant()
                    {
                        recompute_heap_pointers(struct_layout, rows, bases, offset);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Finds where the heap data of each row in `rows` starts, for rows whose `sizes` are
/// non-zero (`FindHeapPointers`): the first heap pointer in the row.
///
/// # Safety
/// `rows` must be pinned rows of `layout`.
pub(crate) unsafe fn find_heap_pointers(
    layout: &TupleDataLayout,
    rows: &[RowPtr],
    sizes: &[usize],
    heaps: &mut [RowPtr],
) {
    let mut not_found: Vec<usize> = (0..rows.len()).filter(|&i| sizes[i] != 0).collect();
    // SAFETY: guaranteed by the caller.
    unsafe { find_heap_pointers_in(layout, rows, 0, &mut not_found, heaps) };
}

/// # Safety
/// As for [`find_heap_pointers`].
unsafe fn find_heap_pointers_in(
    layout: &TupleDataLayout,
    rows: &[RowPtr],
    base: usize,
    not_found: &mut Vec<usize>,
    heaps: &mut [RowPtr],
) {
    let all_valid = layout.all_valid();
    // SAFETY (whole function): guaranteed by the caller.
    unsafe {
        if let Some(key) = layout.sort_key_type() {
            if let Some(data_offset) = key.data_pointer_offset() {
                for &i in not_found.iter() {
                    heaps[i] =
                        RowPtr::from_ptr(load::<u64>(rows[i].as_ptr().add(data_offset)) as usize as *mut u8);
                }
            }
            not_found.clear();
            return;
        }
        for &col in layout.variable_columns() {
            if not_found.is_empty() {
                return;
            }
            let offset = base + layout.offsets()[col];
            match layout.types()[col].physical_type() {
                PhysicalType::Varchar => not_found.retain(|&i| {
                    let row = rows[i].as_ptr();
                    if !all_valid && !is_valid(row.add(base), col) {
                        return true;
                    }
                    let location = row.add(offset);
                    if load::<u32>(location) as usize > roastedduck_types::StringT::INLINE_LENGTH {
                        heaps[i] = RowPtr::from_ptr(load::<u64>(location.add(8)) as usize as *mut u8);
                        false
                    } else {
                        true
                    }
                }),
                PhysicalType::List | PhysicalType::Array => not_found.retain(|&i| {
                    let row = rows[i].as_ptr();
                    if !all_valid && !is_valid(row.add(base), col) {
                        return true;
                    }
                    heaps[i] = RowPtr::from_ptr(load::<u64>(row.add(offset)) as usize as *mut u8);
                    false
                }),
                PhysicalType::Struct => {
                    if let Some(struct_layout) = layout.struct_layout(col)
                        && !struct_layout.all_constant()
                    {
                        find_heap_pointers_in(struct_layout, rows, offset, not_found, heaps);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Offsets used by sort key layouts.
impl SortKeyType {
    /// Offset of the heap pointer in a variable-size key.
    pub(crate) fn data_pointer_offset(self) -> Option<usize> {
        match self {
            Self::NoPayloadVariable32 => Some(24),
            Self::PayloadVariable32 => Some(16),
            _ => None,
        }
    }
}
