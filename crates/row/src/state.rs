//! Append and scan states (DuckDB's `TupleDataPinState`, `TupleDataChunkState`, ...).

use std::sync::Arc;

use rustdb_buffer::BufferHandle;
use rustdb_common::STANDARD_VECTOR_SIZE;

use crate::row::RowPtr;

/// What happens to pinned blocks once a chunk no longer needs them
/// (DuckDB's `TupleDataPinProperties`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PinProperties {
    /// Unpin blocks as soon as the next chunk does not need them.
    #[default]
    UnpinAfterDone,
    /// Keep every block pinned (row pointers stay valid, e.g. for hash tables).
    KeepEverythingPinned,
    /// Destroy blocks once scanned (single consuming scan).
    DestroyAfterDone,
    /// The caller has pinned everything already; do not pin or recompute pointers.
    AlreadyPinned,
}

/// Pins held while appending to or scanning a collection, keyed by block index within the
/// current segment.
#[derive(Debug, Default)]
pub struct PinState {
    pub(crate) row_handles: Vec<(u32, Arc<BufferHandle>)>,
    pub(crate) heap_handles: Vec<(u32, Arc<BufferHandle>)>,
    pub properties: PinProperties,
}

impl PinState {
    pub fn new(properties: PinProperties) -> Self {
        Self {
            properties,
            ..Self::default()
        }
    }

    /// Drops every pin without storing it anywhere.
    pub fn clear(&mut self) {
        self.row_handles.clear();
        self.heap_handles.clear();
    }

    /// The pinned heap blocks, for keeping gathered strings alive.
    pub(crate) fn heap_pins(&self) -> Vec<Arc<BufferHandle>> {
        self.heap_handles
            .iter()
            .map(|(_, handle)| Arc::clone(handle))
            .collect()
    }

    /// Moves every pin into `pins` (to keep them alive elsewhere).
    pub(crate) fn take_handles(&mut self, pins: &mut Vec<Arc<BufferHandle>>) {
        pins.extend(self.row_handles.drain(..).map(|(_, handle)| handle));
        pins.extend(self.heap_handles.drain(..).map(|(_, handle)| handle));
    }
}

/// Row and heap locations of the chunk being appended or scanned
/// (DuckDB's `TupleDataChunkState`).
#[derive(Debug)]
pub struct ChunkState {
    pub(crate) row_locations: Vec<RowPtr>,
    pub(crate) heap_locations: Vec<RowPtr>,
    pub(crate) heap_sizes: Vec<usize>,
    /// Columns written by appends / read by scans.
    pub(crate) column_ids: Vec<usize>,
    /// (chunk, part) indexes of the parts built by the last append.
    pub(crate) part_indices: Vec<(usize, usize)>,
    /// The chunk whose rows are described (scans).
    pub(crate) chunk_index: Option<(usize, usize)>,
}

impl Default for ChunkState {
    fn default() -> Self {
        Self {
            row_locations: vec![RowPtr::NULL; STANDARD_VECTOR_SIZE],
            heap_locations: vec![RowPtr::NULL; STANDARD_VECTOR_SIZE],
            heap_sizes: vec![0; STANDARD_VECTOR_SIZE],
            column_ids: Vec::new(),
            part_indices: Vec::new(),
            chunk_index: None,
        }
    }
}

impl ChunkState {
    pub fn new(column_ids: Vec<usize>) -> Self {
        Self {
            column_ids,
            ..Self::default()
        }
    }

    /// Locations of the rows of the last append or scanned chunk.
    pub fn row_locations(&self) -> &[RowPtr] {
        &self.row_locations
    }

    /// Mutable row locations, e.g. to copy rows found elsewhere ([`crate::TupleDataCollection::copy_rows`]).
    pub fn row_locations_mut(&mut self) -> &mut [RowPtr] {
        &mut self.row_locations
    }

    /// Heap locations of the rows (where their variable-size data starts).
    pub fn heap_locations(&self) -> &[RowPtr] {
        &self.heap_locations
    }

    pub fn heap_locations_mut(&mut self) -> &mut [RowPtr] {
        &mut self.heap_locations
    }

    /// Heap bytes of each row.
    pub fn heap_sizes(&self) -> &[usize] {
        &self.heap_sizes
    }

    pub fn heap_sizes_mut(&mut self) -> &mut [usize] {
        &mut self.heap_sizes
    }

    pub fn column_ids(&self) -> &[usize] {
        &self.column_ids
    }
}

/// State of an append (`TupleDataAppendState`).
#[derive(Debug, Default)]
pub struct AppendState {
    pub pin_state: PinState,
    pub chunk_state: ChunkState,
}

/// State of a scan (`TupleDataScanState`).
#[derive(Debug, Default)]
pub struct ScanState {
    pub pin_state: PinState,
    pub chunk_state: ChunkState,
    pub(crate) segment_index: usize,
    pub(crate) chunk_index: usize,
    /// The segment whose blocks `pin_state` holds.
    pub(crate) pinned_segment: Option<usize>,
}

/// Shared state of a parallel scan (`TupleDataParallelScanState`).
#[derive(Debug, Default)]
pub struct ParallelScanState {
    pub(crate) next: std::sync::Mutex<(usize, usize)>,
    pub(crate) column_ids: Vec<usize>,
    pub(crate) properties: PinProperties,
}

/// Contiguous range of block or part ids (DuckDB's `ContinuousIdSet`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IdRange {
    start: u32,
    end: u32,
}

impl Default for IdRange {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl IdRange {
    pub(crate) const EMPTY: Self = Self {
        start: u32::MAX,
        end: 0,
    };

    pub(crate) fn is_empty(self) -> bool {
        self.start >= self.end
    }

    pub(crate) fn insert(&mut self, id: u32) {
        if self.is_empty() {
            self.start = id;
            self.end = id + 1;
        } else {
            self.start = self.start.min(id);
            self.end = self.end.max(id + 1);
        }
    }

    pub(crate) fn contains(self, id: u32) -> bool {
        !self.is_empty() && id >= self.start && id < self.end
    }

    pub(crate) fn start(self) -> u32 {
        self.start
    }

    pub(crate) fn end(self) -> u32 {
        self.end
    }

    pub(crate) fn len(self) -> u32 {
        if self.is_empty() { 0 } else { self.end - self.start }
    }

    pub(crate) fn decrement_end(&mut self) {
        debug_assert!(self.len() > 1);
        self.end -= 1;
    }
}
