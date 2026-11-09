//! Random access to the keys of a sorted run whose blocks may be spilled (DuckDB's
//! `ExternalBlockIteratorState`, `block_iterator.hpp`).
//!
//! A materialized run stores keys and payload in the same order, chunk by chunk, so key
//! `i` lives in chunk `i / STANDARD_VECTOR_SIZE` of both. The state pins one key chunk (and,
//! with [`ExternalBlockIteratorState::set_pin_payload`], the matching payload chunk) at a
//! time. With [`ExternalBlockIteratorState::set_keep_pinned`] the pins of the chunks it
//! leaves are kept until the next call, so keys read earlier (their heap and payload
//! pointers) stay valid; DuckDB calls that re-pinning.

use std::sync::Arc;

use roastedduck_buffer::BufferHandle;
use roastedduck_common::{Result, STANDARD_VECTOR_SIZE};

use super::key::SortKey;
use super::run::SortedRun;
use crate::gather::HeapKeepAlive;
use crate::row::RowPtr;
use crate::state::{PinProperties, PinState, ScanState};

/// See the module documentation. One state per reader: it is not shared between threads.
pub struct ExternalBlockIteratorState {
    tuple_count: usize,
    current_chunk: Option<usize>,
    key_scan: ScanState,
    payload_scan: ScanState,
    keep_pinned: bool,
    pin_payload: bool,
    pins: Vec<Arc<BufferHandle>>,
}

impl std::fmt::Debug for ExternalBlockIteratorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalBlockIteratorState")
            .field("tuple_count", &self.tuple_count)
            .field("current_chunk", &self.current_chunk)
            .field("keep_pinned", &self.keep_pinned)
            .field("pin_payload", &self.pin_payload)
            .finish_non_exhaustive()
    }
}

fn unpinning_scan() -> ScanState {
    ScanState {
        pin_state: PinState::new(PinProperties::UnpinAfterDone),
        ..ScanState::default()
    }
}

impl ExternalBlockIteratorState {
    /// A state over the keys (and payload) of `run`.
    pub fn new(run: &SortedRun) -> Self {
        Self {
            tuple_count: run.count(),
            current_chunk: None,
            key_scan: unpinning_scan(),
            payload_scan: unpinning_scan(),
            keep_pinned: false,
            pin_payload: false,
            pins: Vec::new(),
        }
    }

    /// Number of keys.
    pub fn count(&self) -> usize {
        self.tuple_count
    }

    /// Whether the pins of chunks left behind are kept; either way every pin held so far
    /// is released (`SetKeepPinned`).
    pub fn set_keep_pinned(&mut self, enable: bool) {
        self.keep_pinned = enable;
        // Always start with a clean slate when toggling.
        self.pins.clear();
        self.key_scan.pin_state.clear();
        self.payload_scan.pin_state.clear();
        self.key_scan.pinned_segment = None;
        self.payload_scan.pinned_segment = None;
        self.current_chunk = None;
    }

    /// Whether the payload chunks are pinned too, so the keys' payload pointers are valid
    /// (`SetPinPayload`).
    pub fn set_pin_payload(&mut self, enable: bool) {
        self.pin_payload = enable;
    }

    /// Re-pins for a new round of reads: releases everything held and keeps the pins of
    /// the keys (and payload) read from now on (`GlobalSortedTable::Repin`).
    pub fn repin(&mut self) {
        self.set_keep_pinned(true);
        self.set_pin_payload(true);
    }

    /// Key `index` of `run` (`GetValueAtIndex`). With the payload pinned its payload
    /// pointer points to the pinned payload row.
    #[inline]
    pub fn get<K: SortKey>(&mut self, run: &SortedRun, index: usize) -> Result<K> {
        debug_assert!(index < self.tuple_count);
        let chunk = index / STANDARD_VECTOR_SIZE;
        if self.current_chunk != Some(chunk) {
            self.initialize_chunk(run, chunk)?;
        }
        let tuple = index % STANDARD_VECTOR_SIZE;
        // SAFETY: the chunk's key rows are pinned and hold `K`s.
        let mut key = unsafe {
            self.key_scan.chunk_state.row_locations[tuple]
                .as_ptr()
                .cast::<K>()
                .read()
        };
        if K::TYPE.has_payload() && self.pin_payload {
            key.set_payload(self.payload_scan.chunk_state.row_locations[tuple].as_ptr());
        }
        Ok(key)
    }

    /// Pins key chunk `chunk` (and its payload), keeping or dropping the previous pins
    /// (`InitializeChunk`).
    fn initialize_chunk(&mut self, run: &SortedRun, chunk: usize) -> Result<()> {
        self.current_chunk = Some(chunk);
        if self.keep_pinned {
            self.key_scan.pin_state.take_handles(&mut self.pins);
        }
        run.key_data.fetch_chunk(&mut self.key_scan, chunk, false)?;
        if self.pin_payload
            && let Some(payload) = &run.payload_data
        {
            if self.keep_pinned {
                self.payload_scan.pin_state.take_handles(&mut self.pins);
            }
            payload.fetch_chunk(&mut self.payload_scan, chunk, false)?;
        }
        Ok(())
    }

    /// The payload row of key `index`, which must be in the current chunk with the payload
    /// pinned.
    pub fn payload_row(&self, index: usize) -> RowPtr {
        self.payload_scan.chunk_state.row_locations[index % STANDARD_VECTOR_SIZE]
    }

    /// Every pin held: what keeps the data of keys read since the last re-pin alive.
    pub fn keep_alive(&self) -> HeapKeepAlive {
        let mut pins = self.pins.clone();
        for state in [&self.key_scan.pin_state, &self.payload_scan.pin_state] {
            pins.extend(state.row_handles.iter().map(|(_, handle)| Arc::clone(handle)));
            pins.extend(state.heap_handles.iter().map(|(_, handle)| Arc::clone(handle)));
        }
        Arc::new(pins)
    }
}
