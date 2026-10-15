//! A buffer-managed collection of rows (DuckDB's `TupleDataCollection`).

use std::ops::Range;
use std::sync::{Arc, PoisonError};

use rustdb_buffer::{BufferHandle, BufferManager, MemoryTag};
use rustdb_common::{Error, Result};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::format::ChunkFormat;
use crate::gather::{HeapKeepAlive, gather_column, has_heap_data};
use crate::layout::TupleDataLayout;
use crate::row::{RowPtr, copy};
use crate::scatter::{compute_heap_sizes, input_row, scatter};
use crate::segment::{
    HeapBases, Segment, SegmentContext, find_heap_pointers, read_heap_sizes, recompute_heap_pointers,
};
use crate::state::{AppendState, ChunkState, IdRange, ParallelScanState, PinProperties, PinState, ScanState};

/// Rows of a [`TupleDataLayout`] stored in buffer-managed blocks; see the crate docs.
///
/// Appends need `&mut self`; scans, gathers and pins work through `&self`, so several
/// threads can scan one collection in parallel.
pub struct TupleDataCollection {
    buffer_manager: BufferManager,
    layout: Arc<TupleDataLayout>,
    tag: MemoryTag,
    pub(crate) segments: Vec<Segment>,
    count: usize,
    data_size: usize,
    partition_index: Option<usize>,
}

impl std::fmt::Debug for TupleDataCollection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TupleDataCollection")
            .field("types", &self.layout.types())
            .field("count", &self.count)
            .field("chunks", &self.chunk_count())
            .field("data_size", &self.data_size)
            .finish_non_exhaustive()
    }
}

impl TupleDataCollection {
    /// An empty collection of rows laid out per `layout`, charged to `tag`.
    pub fn new(buffer_manager: BufferManager, layout: Arc<TupleDataLayout>, tag: MemoryTag) -> Result<Self> {
        if layout.row_width() == 0 || buffer_manager.block_size() / layout.row_width() == 0 {
            return Err(Error::not_implemented(format!(
                "Too many columns: tuple width exceeds block size of {}",
                buffer_manager.block_size()
            )));
        }
        Ok(Self {
            buffer_manager,
            layout,
            tag,
            segments: Vec::new(),
            count: 0,
            data_size: 0,
            partition_index: None,
        })
    }

    /// An empty collection with the same layout, tag and buffer manager (`CreateUnique`).
    #[must_use]
    pub fn empty_like(&self) -> Self {
        Self {
            buffer_manager: self.buffer_manager.clone(),
            layout: Arc::clone(&self.layout),
            tag: self.tag,
            segments: Vec::new(),
            count: 0,
            data_size: 0,
            partition_index: None,
        }
    }

    fn context(&self) -> SegmentContext<'_> {
        SegmentContext {
            buffer_manager: &self.buffer_manager,
            layout: &self.layout,
            tag: self.tag,
            partition_index: self.partition_index,
        }
    }

    pub fn layout(&self) -> &TupleDataLayout {
        &self.layout
    }

    pub fn layout_arc(&self) -> &Arc<TupleDataLayout> {
        &self.layout
    }

    pub fn types(&self) -> &[LogicalType] {
        self.layout.types()
    }

    pub fn buffer_manager(&self) -> &BufferManager {
        &self.buffer_manager
    }

    /// Rows per row block.
    pub fn tuples_per_block(&self) -> usize {
        self.buffer_manager.block_size() / self.layout.row_width()
    }

    /// Number of rows.
    pub fn count(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Number of chunks (of up to `STANDARD_VECTOR_SIZE` rows).
    pub fn chunk_count(&self) -> usize {
        self.segments.iter().map(|segment| segment.chunks.len()).sum()
    }

    /// Bytes of row and heap data plus metadata.
    pub fn size_in_bytes(&self) -> usize {
        let metadata: usize = self
            .segments
            .iter()
            .map(|segment| {
                segment.chunks.capacity() * size_of::<crate::segment::Chunk>()
                    + segment.parts.capacity() * size_of::<crate::segment::ChunkPart>()
            })
            .sum();
        self.data_size + metadata
    }

    /// Releases every pin stored by `KeepEverythingPinned`.
    pub fn unpin(&self) {
        for segment in &self.segments {
            segment.unpin();
        }
    }

    /// Marks this collection as partition `index` of a radix-partitioned set, so its blocks
    /// are evicted in order of partition size. Only before anything was appended.
    pub fn set_partition_index(&mut self, index: usize) {
        debug_assert_eq!(self.count, 0);
        self.partition_index = Some(index);
    }

    /// Addresses of all row blocks of a single-segment collection whose blocks are all
    /// pinned (`GetRowBlockPointers`).
    pub fn row_block_pointers(&self) -> Result<Vec<*mut u8>> {
        match self.segments.as_slice() {
            [] => Ok(Vec::new()),
            [segment] => segment.row_block_pointers(),
            _ => Err(Error::internal(
                "row block pointers of a multi-segment collection",
            )),
        }
    }

    /// Lets the blocks of chunks `begin..end` be destroyed once unpinned
    /// (`DestroyChunks`, single-segment collections).
    pub fn destroy_chunks(&self, begin: usize, end: usize) {
        let [segment] = self.segments.as_slice() else {
            return;
        };
        if begin >= end || begin >= segment.chunks.len() {
            return;
        }
        let first = &segment.chunks[begin];
        let row_end = segment.chunks.get(end).map_or(segment.row_blocks.len(), |chunk| {
            chunk.row_block_ids.start() as usize
        });
        segment.destroy_row_blocks(first.row_block_ids.start() as usize, row_end);
        if self.layout.all_constant() || first.heap_block_ids.is_empty() {
            return;
        }
        let heap_end = match segment.chunks.get(end) {
            None => segment.heap_blocks.len(),
            Some(chunk) if chunk.heap_block_ids.is_empty() => return,
            Some(chunk) => chunk.heap_block_ids.start() as usize,
        };
        segment.destroy_heap_blocks(first.heap_block_ids.start() as usize, heap_end);
    }

    // ---------------------------------------------------------------------------------
    // Appending
    // ---------------------------------------------------------------------------------

    /// Prepares appending every column.
    pub fn initialize_append(&mut self, properties: PinProperties) -> AppendState {
        self.initialize_append_columns((0..self.layout.column_count()).collect(), properties)
    }

    /// Prepares appending the given columns (the others must be fixed-size and are left
    /// uninitialised, e.g. hash table bookkeeping written separately).
    pub fn initialize_append_columns(
        &mut self,
        column_ids: Vec<usize>,
        properties: PinProperties,
    ) -> AppendState {
        if self.segments.is_empty() {
            self.segments.push(Segment::new());
        }
        AppendState {
            pin_state: PinState::new(properties),
            chunk_state: ChunkState::new(column_ids),
        }
    }

    /// Appends all rows of `chunk`.
    pub fn append(&mut self, state: &mut AppendState, chunk: &DataChunk) -> Result<()> {
        let formats = ChunkFormat::new(chunk)?;
        self.append_formatted(state, &formats, None, chunk.len())
    }

    /// Appends rows `sel[..count]` (or the first `count`) of already formatted input.
    /// After the call, `state.chunk_state.row_locations()` hold the new rows.
    pub fn append_formatted(
        &mut self,
        state: &mut AppendState,
        formats: &ChunkFormat<'_>,
        sel: Option<&[u32]>,
        count: usize,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        if !self.layout.all_constant() {
            compute_heap_sizes(
                &self.layout,
                formats,
                sel,
                count,
                &mut state.chunk_state.heap_sizes,
            )?;
        }
        self.build(&mut state.pin_state, &mut state.chunk_state, 0, count)?;
        self.scatter(&mut state.chunk_state, formats, sel, count);
        Ok(())
    }

    /// Appends `chunk` with a temporary append state (`Append(DataChunk)`).
    pub fn append_chunk(&mut self, chunk: &DataChunk) -> Result<()> {
        let mut state = self.initialize_append(PinProperties::UnpinAfterDone);
        self.append(&mut state, chunk)?;
        self.finalize_pin_state(&mut state.pin_state);
        Ok(())
    }

    /// Reserves `count` rows at `chunk_state` positions `offset..`, using the heap sizes
    /// already in `chunk_state` (`Build`).
    pub(crate) fn build(
        &mut self,
        pin_state: &mut PinState,
        chunk_state: &mut ChunkState,
        offset: usize,
        count: usize,
    ) -> Result<()> {
        if self.segments.is_empty() {
            self.segments.push(Segment::new());
        }
        let ctx = SegmentContext {
            buffer_manager: &self.buffer_manager,
            layout: &self.layout,
            tag: self.tag,
            partition_index: self.partition_index,
        };
        let Some(segment) = self.segments.last_mut() else {
            unreachable!("a segment was just ensured")
        };
        let size_before = segment.data_size;
        segment.build(ctx, pin_state, chunk_state, offset, count)?;
        self.data_size += segment.data_size - size_before;
        self.count += count;
        Ok(())
    }

    /// Writes rows `sel[..count]` of `formats` into the rows reserved by [`Self::build`].
    pub(crate) fn scatter(
        &self,
        chunk_state: &mut ChunkState,
        formats: &ChunkFormat<'_>,
        sel: Option<&[u32]>,
        count: usize,
    ) {
        let ChunkState {
            row_locations,
            heap_locations,
            heap_sizes,
            column_ids,
            ..
        } = chunk_state;
        // SAFETY: `build` reserved `count` rows at `row_locations` and `heap_sizes[i]` heap
        // bytes at each `heap_locations[i]`, sized by `compute_heap_sizes` for this input.
        unsafe {
            scatter(
                &self.layout,
                formats,
                column_ids,
                sel,
                count,
                row_locations,
                heap_locations,
                heap_sizes,
            );
        }
    }

    /// Copies rows `sel[..count]` described by `input` (rows of another collection with the
    /// same layout, with heap locations and sizes) into rows reserved by [`Self::build`]
    /// with the same heap sizes (`CopyRows`).
    pub(crate) fn copy_rows(
        &self,
        chunk_state: &ChunkState,
        input: &ChunkState,
        sel: Option<&[u32]>,
        count: usize,
    ) {
        let row_width = self.layout.row_width();
        // SAFETY: `build` reserved the target rows and heaps; the input rows are pinned.
        unsafe {
            for i in 0..count {
                let source = input.row_locations[input_row(sel, i)];
                copy(source.as_ptr(), chunk_state.row_locations[i].as_ptr(), row_width);
            }
            if self.layout.all_constant() {
                return;
            }
            let mut old_bases = Vec::with_capacity(count);
            let mut total = 0;
            for i in 0..count {
                let source = input_row(sel, i);
                let size = input.heap_sizes[source];
                total += size;
                if size != 0 {
                    copy(
                        input.heap_locations[source].as_ptr(),
                        chunk_state.heap_locations[i].as_ptr(),
                        size,
                    );
                }
                old_bases.push(input.heap_locations[source]);
            }
            if total == 0 {
                return;
            }
            recompute_heap_pointers(
                &self.layout,
                &chunk_state.row_locations[..count],
                HeapBases::PerRow(&old_bases, &chunk_state.heap_locations[..count]),
                0,
            );
        }
    }

    /// Fills in the heap sizes and heap locations of the `count` rows in `chunk_state`
    /// (`FindHeapPointers`).
    pub(crate) fn find_heap_pointers(&self, chunk_state: &mut ChunkState, count: usize) {
        // SAFETY: the rows in `chunk_state` are pinned rows of this layout.
        unsafe {
            read_heap_sizes(
                &self.layout,
                &chunk_state.row_locations[..count],
                &mut chunk_state.heap_sizes[..count],
            );
            find_heap_pointers(
                &self.layout,
                &chunk_state.row_locations[..count],
                &chunk_state.heap_sizes[..count],
                &mut chunk_state.heap_locations[..count],
            );
        }
    }

    /// Releases (or stores, per the pin properties) every pin of an append or scan
    /// (`FinalizePinState`).
    pub fn finalize_pin_state(&self, pin_state: &mut PinState) {
        if let Some(segment) = self.segments.last() {
            segment.release_or_store_handles(pin_state, IdRange::EMPTY, IdRange::EMPTY, true);
        }
    }

    /// Moves all rows of `other` into this collection (`Combine`). Finalize the pin states
    /// of appends to either collection first.
    pub fn combine(&mut self, mut other: TupleDataCollection) -> Result<()> {
        if other.count == 0 {
            return Ok(());
        }
        if self.layout.types() != other.layout.types() {
            return Err(Error::internal(
                "Attempting to combine TupleDataCollection with mismatching types",
            ));
        }
        self.count += other.count;
        self.data_size += other.data_size;
        self.segments.append(&mut other.segments);
        Ok(())
    }

    /// Removes all rows.
    pub fn reset(&mut self) {
        self.segments.clear();
        self.count = 0;
        self.data_size = 0;
    }

    // ---------------------------------------------------------------------------------
    // Scanning
    // ---------------------------------------------------------------------------------

    /// Prepares a scan of all columns.
    pub fn initialize_scan(&self, properties: PinProperties) -> ScanState {
        self.initialize_scan_columns((0..self.layout.column_count()).collect(), properties)
    }

    /// Prepares a scan of the given columns.
    pub fn initialize_scan_columns(&self, column_ids: Vec<usize>, properties: PinProperties) -> ScanState {
        ScanState {
            pin_state: PinState::new(properties),
            chunk_state: ChunkState::new(column_ids),
            segment_index: 0,
            chunk_index: 0,
            pinned_segment: None,
        }
    }

    /// The types of the scanned columns.
    pub fn scan_types(&self, state: &ScanState) -> Vec<LogicalType> {
        state
            .chunk_state
            .column_ids
            .iter()
            .map(|&col| self.layout.types()[col].clone())
            .collect()
    }

    fn next_scan_index(&self, segment_index: &mut usize, chunk_index: &mut usize) -> Option<(usize, usize)> {
        while let Some(segment) = self.segments.get(*segment_index) {
            if *chunk_index < segment.chunks.len() {
                let next = (*segment_index, *chunk_index);
                *chunk_index += 1;
                return Some(next);
            }
            *segment_index += 1;
            *chunk_index = 0;
        }
        None
    }

    /// Releases the pins of the previous segment when moving to another one.
    fn switch_segment(&self, state: &mut ScanState, segment_index: Option<usize>) {
        if let Some(previous) = state.pinned_segment
            && Some(previous) != segment_index
        {
            self.segments[previous].release_or_store_handles(
                &mut state.pin_state,
                IdRange::EMPTY,
                IdRange::EMPTY,
                true,
            );
        }
        state.pinned_segment = segment_index;
    }

    /// Reads the next chunk into `result`; returns `false` when the scan is done.
    pub fn scan(&self, state: &mut ScanState, result: &mut DataChunk) -> Result<bool> {
        let (mut segment_index, mut chunk_index) = (state.segment_index, state.chunk_index);
        let next = self.next_scan_index(&mut segment_index, &mut chunk_index);
        state.segment_index = segment_index;
        state.chunk_index = chunk_index;
        self.scan_next(state, next, result)
    }

    /// Prepares a scan shared by several threads.
    pub fn initialize_parallel_scan(
        &self,
        column_ids: Option<Vec<usize>>,
        properties: PinProperties,
    ) -> ParallelScanState {
        ParallelScanState {
            next: std::sync::Mutex::new((0, 0)),
            column_ids: column_ids.unwrap_or_else(|| (0..self.layout.column_count()).collect()),
            properties,
        }
    }

    /// A thread-local state for a parallel scan.
    pub fn initialize_local_scan(&self, global: &ParallelScanState) -> ScanState {
        self.initialize_scan_columns(global.column_ids.clone(), global.properties)
    }

    /// Reads the next unclaimed chunk of a parallel scan into `result`.
    pub fn parallel_scan(
        &self,
        global: &ParallelScanState,
        local: &mut ScanState,
        result: &mut DataChunk,
    ) -> Result<bool> {
        let next = {
            let mut position = global.next.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut segment_index, mut chunk_index) = *position;
            let next = self.next_scan_index(&mut segment_index, &mut chunk_index);
            *position = (segment_index, chunk_index);
            next
        };
        self.scan_next(local, next, result)
    }

    fn scan_next(
        &self,
        state: &mut ScanState,
        next: Option<(usize, usize)>,
        result: &mut DataChunk,
    ) -> Result<bool> {
        let Some((segment_index, chunk_index)) = next else {
            self.switch_segment(state, None);
            result.set_columns(Vec::new(), 0);
            return Ok(false);
        };
        self.switch_segment(state, Some(segment_index));
        let segment = &self.segments[segment_index];
        let count = segment.initialize_chunk_state(
            self.context(),
            &mut state.pin_state,
            &mut state.chunk_state,
            chunk_index,
            false,
        )?;
        state.chunk_state.chunk_index = Some((segment_index, chunk_index));
        let keep_alive = self.scan_keep_alive(&state.pin_state, &state.chunk_state.column_ids);
        let mut columns = Vec::with_capacity(state.chunk_state.column_ids.len());
        for &col in &state.chunk_state.column_ids {
            let mut vector = Vector::flat(self.layout.types()[col].clone(), count.max(1));
            // SAFETY: the chunk's blocks are pinned by `state.pin_state` and its row
            // locations were just initialised.
            unsafe {
                self.gather_column_unchecked(
                    &state.chunk_state.row_locations,
                    None,
                    count,
                    col,
                    &mut vector,
                    None,
                    keep_alive.as_ref(),
                )?;
            }
            columns.push(vector);
        }
        result.set_columns(columns, count);
        Ok(true)
    }

    fn scan_keep_alive(&self, pin_state: &PinState, column_ids: &[usize]) -> Option<HeapKeepAlive> {
        let needs_heap = column_ids
            .iter()
            .any(|&col| has_heap_data(&self.layout.types()[col]));
        if !needs_heap || self.layout.all_constant() {
            return None;
        }
        let pins: Vec<Arc<BufferHandle>> = pin_state.heap_pins();
        Some(Arc::new(pins))
    }

    /// Whether a serial scan has read everything.
    pub fn scan_complete(&self, state: &ScanState) -> bool {
        self.count == 0
            || (state.segment_index + 1 == self.segments.len()
                && self
                    .segments
                    .last()
                    .is_some_and(|segment| state.chunk_index == segment.chunks.len()))
    }

    /// Positions a serial scan at chunk `target_chunk`; returns the rows before it
    /// (`Seek`).
    pub fn seek(&self, state: &mut ScanState, target_chunk: usize) -> usize {
        state.pin_state.clear();
        state.pinned_segment = None;
        let mut current_chunk = 0;
        let mut rows = 0;
        for (segment_index, segment) in self.segments.iter().enumerate() {
            let chunks = segment.chunks.len();
            if current_chunk + chunks <= target_chunk {
                rows += segment.count;
                current_chunk += chunks;
                continue;
            }
            let within = target_chunk - current_chunk;
            rows += segment.chunks[..within]
                .iter()
                .map(|chunk| chunk.count as usize)
                .sum::<usize>();
            state.segment_index = segment_index;
            state.chunk_index = within;
            break;
        }
        rows
    }

    /// Pins chunk `chunk_index` (counted across segments) and fills `state.chunk_state`
    /// with its row (and with `init_heap`, heap) locations; returns its row count
    /// (`FetchChunk`).
    pub fn fetch_chunk(&self, state: &mut ScanState, chunk_index: usize, init_heap: bool) -> Result<usize> {
        let (segment_index, within) = self.locate_chunk(chunk_index)?;
        self.switch_segment(state, Some(segment_index));
        let count = self.segments[segment_index].initialize_chunk_state(
            self.context(),
            &mut state.pin_state,
            &mut state.chunk_state,
            within,
            init_heap,
        )?;
        state.chunk_state.chunk_index = Some((segment_index, within));
        Ok(count)
    }

    pub(crate) fn locate_chunk(&self, mut chunk_index: usize) -> Result<(usize, usize)> {
        for (segment_index, segment) in self.segments.iter().enumerate() {
            if chunk_index < segment.chunks.len() {
                return Ok((segment_index, chunk_index));
            }
            chunk_index -= segment.chunks.len();
        }
        Err(Error::internal(
            "Chunk index out of in TupleDataCollection::FetchChunk",
        ))
    }

    /// The pins held for `KeepEverythingPinned` heaps, to keep gathered strings alive.
    pub fn heap_keep_alive(&self) -> HeapKeepAlive {
        let pins: Vec<Arc<BufferHandle>> = self.segments.iter().flat_map(Segment::pinned_heaps).collect();
        Arc::new(pins)
    }

    /// Gathers `columns` of the rows `rows[scan_sel[i]]` into `result` at `target_sel[i]`
    /// (`Gather`). `result` holds one flat vector per gathered column; `keep_alive` must own
    /// the pins of the rows' heap blocks (e.g. [`Self::heap_keep_alive`]).
    ///
    /// # Safety
    /// Every selected row must be a row of this collection's layout in a block that stays
    /// pinned for the duration of the call (and the heap blocks as long as `keep_alive`).
    #[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's Gather")]
    pub unsafe fn gather(
        &self,
        rows: &[RowPtr],
        scan_sel: Option<&[u32]>,
        count: usize,
        columns: &[usize],
        result: &mut [Vector],
        target_sel: Option<&[u32]>,
        keep_alive: Option<&HeapKeepAlive>,
    ) -> Result<()> {
        for (&col, target) in columns.iter().zip(result) {
            // SAFETY: forwarded from the caller.
            unsafe {
                self.gather_column_unchecked(rows, scan_sel, count, col, target, target_sel, keep_alive)?;
            };
        }
        Ok(())
    }

    /// # Safety
    /// As for [`Self::gather`].
    #[allow(clippy::too_many_arguments, reason = "mirrors DuckDB's Gather")]
    pub(crate) unsafe fn gather_column_unchecked(
        &self,
        rows: &[RowPtr],
        scan_sel: Option<&[u32]>,
        count: usize,
        col: usize,
        target: &mut Vector,
        target_sel: Option<&[u32]>,
        keep_alive: Option<&HeapKeepAlive>,
    ) -> Result<()> {
        if target.logical_type() != &self.layout.types()[col] {
            return Err(Error::internal(format!(
                "gather into a vector of type {} from a column of type {}",
                target.logical_type(),
                self.layout.types()[col]
            )));
        }
        // SAFETY: forwarded from the caller.
        unsafe { gather_column(&self.layout, col, rows, scan_sel, count, 0, target, target_sel)? };
        if let Some(keep_alive) = keep_alive
            && has_heap_data(target.logical_type())
        {
            target.add_keep_alive(Arc::clone(keep_alive));
        }
        Ok(())
    }

    /// Reads every row into chunks (convenience for tests and small collections).
    pub fn to_chunks(&self) -> Result<Vec<DataChunk>> {
        let mut state = self.initialize_scan(PinProperties::UnpinAfterDone);
        let mut chunks = Vec::new();
        loop {
            let mut chunk = DataChunk::default();
            if !self.scan(&mut state, &mut chunk)? {
                break;
            }
            chunks.push(chunk);
        }
        Ok(chunks)
    }
}

/// Iterates over the chunks of a collection, exposing row (and heap) locations instead of
/// gathering (DuckDB's `TupleDataChunkIterator`).
#[derive(Debug)]
pub struct ChunkIterator<'a> {
    collection: &'a TupleDataCollection,
    init_heap: bool,
    chunks: Range<usize>,
    current: usize,
    state: ScanState,
    current_count: usize,
}

impl<'a> ChunkIterator<'a> {
    /// Iterates over all chunks.
    pub fn new(
        collection: &'a TupleDataCollection,
        properties: PinProperties,
        init_heap: bool,
    ) -> Result<Self> {
        Self::range(collection, properties, 0..collection.chunk_count(), init_heap)
    }

    /// Iterates over chunks `chunks` (indexes across segments).
    pub fn range(
        collection: &'a TupleDataCollection,
        properties: PinProperties,
        chunks: Range<usize>,
        init_heap: bool,
    ) -> Result<Self> {
        let mut iterator = Self {
            collection,
            init_heap,
            current: chunks.start,
            chunks,
            state: collection.initialize_scan_columns(Vec::new(), properties),
            current_count: 0,
        };
        iterator.load_current()?;
        Ok(iterator)
    }

    fn load_current(&mut self) -> Result<()> {
        self.current_count = if self.current < self.chunks.end {
            self.collection
                .fetch_chunk(&mut self.state, self.current, self.init_heap)?
        } else {
            0
        };
        Ok(())
    }

    /// Whether every chunk was visited.
    pub fn done(&self) -> bool {
        self.current >= self.chunks.end
    }

    /// Moves to the next chunk; returns `false` (and releases pins) when done.
    pub fn next_chunk(&mut self) -> Result<bool> {
        self.current += 1;
        if self.done() {
            self.collection.switch_segment(&mut self.state, None);
            self.current_count = 0;
            return Ok(false);
        }
        self.load_current()?;
        Ok(true)
    }

    /// Restarts at the first chunk of the range.
    pub fn reset(&mut self) -> Result<()> {
        self.current = self.chunks.start;
        self.load_current()
    }

    /// Rows in the current chunk.
    pub fn current_chunk_count(&self) -> usize {
        self.current_count
    }

    /// Locations of the current chunk.
    pub fn chunk_state(&self) -> &ChunkState {
        &self.state.chunk_state
    }

    pub fn row_locations(&self) -> &[RowPtr] {
        &self.state.chunk_state.row_locations[..self.current_count]
    }

    pub fn heap_locations(&self) -> &[RowPtr] {
        &self.state.chunk_state.heap_locations[..self.current_count]
    }

    pub fn heap_sizes(&self) -> &[usize] {
        &self.state.chunk_state.heap_sizes[..self.current_count]
    }

    /// The pins of the current chunk's heap blocks.
    pub fn heap_keep_alive(&self) -> HeapKeepAlive {
        Arc::new(self.state.pin_state.heap_pins())
    }
}
