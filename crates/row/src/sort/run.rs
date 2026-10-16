//! A thread's sorted run: sort keys plus payload rows (DuckDB's `SortedRun`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustdb_buffer::MemoryTag;
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};

use super::SortLayouts;
use super::algorithm::{KeyBlocks, ska_key, sort_keys};
use super::encode::SortKeyBuffer;
use super::key::{SortKey, dispatch_sort_key};
use crate::collection::TupleDataCollection;
use crate::format::ChunkFormat;
use crate::row::RowPtr;
use crate::state::{AppendState, ChunkState, PinProperties};

/// The interrupt flag of a query, checked during long sorts.
pub(crate) fn check_interrupt(interrupted: Option<&AtomicBool>) -> Result<()> {
    if interrupted.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err(Error::new(rustdb_common::ErrorKind::Interrupt, "Interrupted!"));
    }
    Ok(())
}

/// Keys and payload of one run; sorted by [`SortedRun::finalize`].
pub struct SortedRun {
    pub(crate) layouts: Arc<SortLayouts>,
    pub(crate) key_data: TupleDataCollection,
    pub(crate) payload_data: Option<TupleDataCollection>,
    key_append: AppendState,
    payload_append: AppendState,
    pub(crate) finalized: bool,
    /// Whether this is the approximate sort of an index creation (no full-key tie break).
    is_index_sort: bool,
}

impl std::fmt::Debug for SortedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SortedRun")
            .field("count", &self.count())
            .field("finalized", &self.finalized)
            .finish_non_exhaustive()
    }
}

impl SortedRun {
    /// An empty run; `is_index_sort` for the approximate sort of an index creation.
    pub(crate) fn new(layouts: &Arc<SortLayouts>, is_index_sort: bool) -> Result<Self> {
        let mut key_data = TupleDataCollection::new(
            layouts.buffer_manager.clone(),
            Arc::clone(&layouts.key_layout),
            MemoryTag::OrderBy,
        )?;
        let key_append = key_data.initialize_append(PinProperties::KeepEverythingPinned);
        let (payload_data, payload_append) = match &layouts.payload_layout {
            Some(layout) => {
                let mut payload = TupleDataCollection::new(
                    layouts.buffer_manager.clone(),
                    Arc::clone(layout),
                    MemoryTag::OrderBy,
                )?;
                let append = payload.initialize_append(PinProperties::KeepEverythingPinned);
                (Some(payload), append)
            }
            None => (None, AppendState::default()),
        };
        Ok(Self {
            layouts: Arc::clone(layouts),
            key_data,
            payload_data,
            key_append,
            payload_append,
            finalized: false,
            is_index_sort,
        })
    }

    /// An empty, finalized run to materialize merged data into
    /// (`CreateRunForMaterialization`).
    pub(crate) fn for_materialization(layouts: &Arc<SortLayouts>) -> Result<Self> {
        // Materialized runs are already in order and never sorted again.
        let mut run = Self::new(layouts, false)?;
        run.key_append.pin_state.properties = PinProperties::UnpinAfterDone;
        run.payload_append.pin_state.properties = PinProperties::UnpinAfterDone;
        run.finalized = true;
        Ok(run)
    }

    /// Number of rows.
    pub fn count(&self) -> usize {
        self.key_data.count()
    }

    /// Bytes held by keys and payload.
    pub fn size_in_bytes(&self) -> usize {
        self.key_data.size_in_bytes()
            + self
                .payload_data
                .as_ref()
                .map_or(0, TupleDataCollection::size_in_bytes)
    }

    /// Appends rows: their normalized keys and (rows `sel` of) the payload columns.
    pub(crate) fn sink(
        &mut self,
        keys: &SortKeyBuffer,
        payload: Option<&ChunkFormat<'_>>,
        count: usize,
    ) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let key_type = self.layouts.key_type;
        dispatch_sort_key!(key_type, K => self.sink_typed::<K>(keys, payload, count))
    }

    fn sink_typed<K: SortKey>(
        &mut self,
        keys: &SortKeyBuffer,
        payload: Option<&ChunkFormat<'_>>,
        count: usize,
    ) -> Result<()> {
        let inline = K::INLINE_LENGTH;
        let state = &mut self.key_append;
        for (row, size) in state.chunk_state.heap_sizes[..count].iter_mut().enumerate() {
            let length = keys.key(row).len();
            if length > inline && K::TYPE.is_constant_size() {
                return Err(Error::internal("sort key longer than its fixed-size layout"));
            }
            *size = if length > inline { length } else { 0 };
        }
        self.key_data
            .build(&mut state.pin_state, &mut state.chunk_state, 0, count)?;
        let payload_rows = match (&mut self.payload_data, payload) {
            (Some(payload_data), Some(formats)) => {
                payload_data.append_formatted(&mut self.payload_append, formats, None, count)?;
                Some(&self.payload_append.chunk_state.row_locations)
            }
            (None, None) => None,
            _ => return Err(Error::internal("sort payload does not match its layout")),
        };
        let chunk_state = &self.key_append.chunk_state;
        for row in 0..count {
            let mut key = K::default();
            let mut heap = chunk_state.heap_locations[row].as_ptr();
            // SAFETY: `build` reserved `heap_sizes[row]` heap bytes for long keys, and the
            // row is a pinned key slot, aligned to the key width within its block.
            unsafe {
                key.construct(keys.key(row), &mut heap);
                if let Some(rows) = payload_rows {
                    key.set_payload(rows[row].as_ptr());
                }
                chunk_state.row_locations[row].as_ptr().cast::<K>().write(key);
            }
        }
        Ok(())
    }

    /// Sorts the keys; for external sorts the payload (and long keys) are rewritten in key
    /// order and everything is unpinned so it can be spilled (`SortedRun::Finalize`).
    pub(crate) fn finalize(&mut self, external: bool, interrupted: Option<&AtomicBool>) -> Result<()> {
        self.key_data.finalize_pin_state(&mut self.key_append.pin_state);
        if let Some(payload) = &self.payload_data {
            payload.finalize_pin_state(&mut self.payload_append.pin_state);
        }
        let key_type = self.layouts.key_type;
        dispatch_sort_key!(key_type, K => self.sort_typed::<K>())?;
        check_interrupt(interrupted)?;
        if external {
            if !key_type.is_constant_size() || key_type.has_payload() {
                dispatch_sort_key!(key_type, K => self.reorder::<K>(interrupted))?;
            } else {
                self.key_data.unpin();
            }
        }
        self.finalized = true;
        Ok(())
    }

    /// The pinned keys as a block-structured array.
    ///
    /// # Safety
    /// The key blocks must be pinned (in-memory runs) and not accessed elsewhere while the
    /// returned value is used for writing.
    pub(crate) unsafe fn key_blocks<K: SortKey>(&self) -> Result<KeyBlocks<K>> {
        let blocks = self.key_data.row_block_pointers()?;
        // SAFETY: rows of a sort key layout are `K`s packed `tuples_per_block` per block;
        // exclusivity is the caller's contract.
        Ok(unsafe { KeyBlocks::new(blocks, self.key_data.tuples_per_block(), self.count()) })
    }

    fn sort_typed<K: SortKey>(&mut self) -> Result<()> {
        let layout = self.key_data.layout();
        let extract = ska_key::<K>(
            layout.sort_width(),
            layout.sort_skippable_bytes(),
            self.is_index_sort,
        );
        // SAFETY: every key block is pinned (KeepEverythingPinned) and only this run uses it.
        let mut blocks = unsafe { self.key_blocks::<K>()? };
        let len = blocks.len();
        let copy_size = len * size_of::<K>();
        let buffer_manager = &self.layouts.buffer_manager;
        // Sorting a contiguous copy is several times faster than sorting across blocks; the
        // copy is not worth evicting other data for (other threads' runs are still being
        // built), so under memory pressure sort in place like DuckDB.
        let copy_fits = buffer_manager.used_memory() + copy_size as u64 <= buffer_manager.max_memory() / 2;
        if len > self.key_data.tuples_per_block()
            && copy_fits
            && let Ok(buffer) = buffer_manager.allocate_data(copy_size)
        {
            let keys = buffer.as_ptr().cast::<K>();
            // SAFETY: `buffer` holds `len` keys, is 16-byte aligned (enough for `K`) and
            // owned here; after `copy_to` every key is initialized.
            let keys = unsafe {
                blocks.copy_to(keys);
                std::slice::from_raw_parts_mut(keys, len)
            };
            sort_keys(keys, &extract);
            blocks.copy_from(keys);
            return Ok(());
        }
        sort_keys(&mut blocks, &extract);
        Ok(())
    }

    /// Rewrites payload rows (and long keys) in sorted order (`TemplatedReorder`), so chunk
    /// `c` of keys matches chunk `c` of payload and can be paged in independently.
    fn reorder<K: SortKey>(&mut self, interrupted: Option<&AtomicBool>) -> Result<()> {
        let total = self.count();
        // SAFETY: the keys are pinned; this run is the only user.
        let blocks = unsafe { self.key_blocks::<K>()? };
        let mut new_keys = (!K::TYPE.is_constant_size()).then(|| {
            let mut keys = self.key_data.empty_like();
            // Keys are needed before the payload: keep them in memory longer.
            keys.set_partition_index(0);
            keys
        });
        let mut new_key_append = new_keys
            .as_mut()
            .map(|keys| keys.initialize_append(PinProperties::UnpinAfterDone));
        let mut new_payload = self
            .payload_data
            .as_ref()
            .filter(|_| K::TYPE.has_payload())
            .map(TupleDataCollection::empty_like);
        let mut new_payload_append = new_payload
            .as_mut()
            .map(|payload| payload.initialize_append(PinProperties::UnpinAfterDone));
        let mut key_input = ChunkState::default();
        let mut payload_input = ChunkState::default();
        let mut start = 0;
        while start < total {
            check_interrupt(interrupted)?;
            let next = (total - start).min(STANDARD_VECTOR_SIZE);
            if let (Some(payload), Some(old_payload), Some(append)) =
                (&mut new_payload, &self.payload_data, &mut new_payload_append)
            {
                for i in 0..next {
                    payload_input.row_locations[i] = RowPtr::from_ptr(blocks.get(start + i).payload());
                }
                if !old_payload.layout().all_constant() {
                    old_payload.find_heap_pointers(&mut payload_input, next);
                }
                append.chunk_state.heap_sizes[..next].copy_from_slice(&payload_input.heap_sizes[..next]);
                payload.build(&mut append.pin_state, &mut append.chunk_state, 0, next)?;
                payload.copy_rows(&append.chunk_state, &payload_input, None, next);
                for i in 0..next {
                    let mut key = blocks.get(start + i);
                    key.set_payload(append.chunk_state.row_locations[i].as_ptr());
                    // SAFETY: pinned key slot of this run (see above).
                    unsafe { blocks.ptr_at(start + i).write(key) };
                }
            }
            if let (Some(keys), Some(append)) = (&mut new_keys, &mut new_key_append) {
                for i in 0..next {
                    let key = blocks.get(start + i);
                    key_input.row_locations[i] = RowPtr::from_ptr(blocks.ptr_at(start + i).cast());
                    key_input.heap_locations[i] = RowPtr::from_ptr(key.heap_data());
                    key_input.heap_sizes[i] = key.heap_size();
                }
                append.chunk_state.heap_sizes[..next].copy_from_slice(&key_input.heap_sizes[..next]);
                keys.build(&mut append.pin_state, &mut append.chunk_state, 0, next)?;
                keys.copy_rows(&append.chunk_state, &key_input, None, next);
            }
            start += next;
        }
        drop(blocks);
        self.key_data.unpin();
        if let (Some(keys), Some(mut append)) = (new_keys, new_key_append) {
            keys.finalize_pin_state(&mut append.pin_state);
            self.key_data = keys;
        }
        if let (Some(payload), Some(mut append)) = (new_payload, new_payload_append) {
            payload.finalize_pin_state(&mut append.pin_state);
            payload.unpin();
            self.payload_data = Some(payload);
        }
        Ok(())
    }

    /// Lets the blocks holding rows `begin..end` be destroyed (`SortedRun::DestroyData`).
    pub(crate) fn destroy_data(&self, begin: usize, end: usize) {
        let (first, last) = (begin / STANDARD_VECTOR_SIZE, end / STANDARD_VECTOR_SIZE);
        if first == last {
            return;
        }
        self.key_data.destroy_chunks(first, last);
        if let Some(payload) = &self.payload_data {
            payload.destroy_chunks(first, last);
        }
    }

    /// Appends rows from `keys` (copied from other runs, with payload pointers) to this
    /// materialization run (`TemplatedMaterializePartition`).
    pub(crate) fn append_keys<K: SortKey>(&mut self, keys: &mut [K]) -> Result<()> {
        let mut key_input = ChunkState::default();
        let mut payload_input = ChunkState::default();
        for batch in keys.chunks_mut(STANDARD_VECTOR_SIZE) {
            let count = batch.len();
            for (i, key) in batch.iter().enumerate() {
                key_input.row_locations[i] = RowPtr::from_ptr(std::ptr::from_ref(key).cast_mut().cast());
                key_input.heap_locations[i] = RowPtr::from_ptr(key.heap_data());
                key_input.heap_sizes[i] = key.heap_size();
                payload_input.row_locations[i] = RowPtr::from_ptr(key.payload());
            }
            if let Some(payload) = &mut self.payload_data {
                if !payload.layout().all_constant() {
                    payload.find_heap_pointers(&mut payload_input, count);
                }
                let append = &mut self.payload_append;
                append.chunk_state.heap_sizes[..count].copy_from_slice(&payload_input.heap_sizes[..count]);
                payload.build(&mut append.pin_state, &mut append.chunk_state, 0, count)?;
                payload.copy_rows(&append.chunk_state, &payload_input, None, count);
                for (key, row) in batch.iter_mut().zip(&append.chunk_state.row_locations) {
                    key.set_payload(row.as_ptr());
                }
                for (i, key) in batch.iter().enumerate() {
                    key_input.row_locations[i] = RowPtr::from_ptr(std::ptr::from_ref(key).cast_mut().cast());
                }
            }
            let append = &mut self.key_append;
            append.chunk_state.heap_sizes[..count].copy_from_slice(&key_input.heap_sizes[..count]);
            self.key_data
                .build(&mut append.pin_state, &mut append.chunk_state, 0, count)?;
            self.key_data
                .copy_rows(&append.chunk_state, &key_input, None, count);
        }
        Ok(())
    }

    /// Releases the pins of a materialization run.
    pub(crate) fn finish_materialization(&mut self) {
        self.key_data.finalize_pin_state(&mut self.key_append.pin_state);
        if let Some(payload) = &self.payload_data {
            payload.finalize_pin_state(&mut self.payload_append.pin_state);
        }
    }

    /// Moves `other`'s rows to the end of this run (`Combine` of materialized partitions).
    pub(crate) fn combine(&mut self, other: SortedRun) -> Result<()> {
        self.key_data.combine(other.key_data)?;
        if let (Some(payload), Some(other_payload)) = (&mut self.payload_data, other.payload_data) {
            payload.combine(other_payload)?;
        }
        Ok(())
    }
}
