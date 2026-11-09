//! The version information of one row group (DuckDB's `RowVersionManager`).

use std::sync::{Mutex, MutexGuard, PoisonError};

use roastedduck_common::{Result, STANDARD_VECTOR_SIZE};

use roastedduck_buffer::{BufferManager, MemoryReservation, MemoryTag};

use super::chunk_info::{ChunkVectorInfo, DeletedRows, ScanOptions, VersionCompression};
use crate::VECTOR_SIZE;
use crate::metadata::{MetaBlockPointer, MetadataManager, MetadataWriter};
use crate::transaction::TransactionData;

#[derive(Debug, Default)]
struct VersionState {
    /// Per vector; `None` means every row is visible to everyone.
    vectors: Vec<Option<ChunkVectorInfo>>,
    /// The newest commit id of a delete that has not been checkpointed yet.
    uncheckpointed_delete_commit: Option<u64>,
    /// Whether a compression pass may achieve anything.
    needs_compression_check: bool,
    /// The metadata blocks the deletes are stored in, as of the last checkpoint that
    /// wrote or read them.
    storage_pointers: Vec<MetaBlockPointer>,
    /// The charge for the per-row version ids, once the row group is part of a table.
    memory: Option<MemoryReservation>,
}

impl VersionState {
    /// Charges the memory of the per-row version ids (DuckDB allocates them from the row
    /// group's `FixedSizeAllocator` with `MemoryTag::BASE_TABLE`).
    fn update_memory(&mut self) {
        if self.memory.is_none() {
            return;
        }
        let size = self
            .vectors
            .iter()
            .flatten()
            .map(ChunkVectorInfo::memory_size)
            .sum();
        if let Some(memory) = &mut self.memory {
            memory.resize(size);
        }
    }

    fn fill(&mut self, vector_index: usize) {
        if self.vectors.len() <= vector_index {
            self.vectors.resize_with(vector_index + 1, || None);
        }
    }

    fn vector_info(&mut self, vector_index: usize) -> &mut ChunkVectorInfo {
        self.fill(vector_index);
        self.vectors[vector_index]
            .get_or_insert_with(|| ChunkVectorInfo::new(vector_index as u64 * VECTOR_SIZE, 0))
    }
}

/// Per-vector insert and delete versions of a row group.
#[derive(Debug, Default)]
pub struct RowVersionManager {
    state: Mutex<VersionState>,
}

/// Iterates over the vectors touched by rows `start..start + count` of a row group, as
/// `(vector index, start within vector, end within vector)`.
fn vector_ranges(start: u64, count: u64) -> impl Iterator<Item = (usize, usize, usize)> {
    let end = start + count;
    let first = start / VECTOR_SIZE;
    let last = if count == 0 {
        first
    } else {
        (end - 1) / VECTOR_SIZE
    };
    (first..=last).filter(move |_| count > 0).map(move |vector| {
        let vector_start = if vector == first {
            start - first * VECTOR_SIZE
        } else {
            0
        };
        let vector_end = if vector == last {
            end - last * VECTOR_SIZE
        } else {
            VECTOR_SIZE
        };
        (vector as usize, vector_start as usize, vector_end as usize)
    })
}

/// The locked state of a change: the charged memory follows the change when the guard is
/// dropped.
struct ChangeGuard<'a>(MutexGuard<'a, VersionState>);

impl std::ops::Deref for ChangeGuard<'_> {
    type Target = VersionState;

    fn deref(&self) -> &VersionState {
        &self.0
    }
}

impl std::ops::DerefMut for ChangeGuard<'_> {
    fn deref_mut(&mut self) -> &mut VersionState {
        &mut self.0
    }
}

impl Drop for ChangeGuard<'_> {
    fn drop(&mut self) {
        self.0.update_memory();
    }
}

impl RowVersionManager {
    fn lock(&self) -> MutexGuard<'_, VersionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks the state for a change of the version ids.
    fn lock_change(&self) -> ChangeGuard<'_> {
        ChangeGuard(self.lock())
    }

    /// Charges the memory of the per-row version ids to `BASE_TABLE` of `buffer_manager`
    /// from now on.
    pub fn track_memory(&self, buffer_manager: &BufferManager) {
        let mut state = self.lock();
        if state.memory.is_none() {
            state.memory = Some(buffer_manager.track_memory(MemoryTag::BaseTable));
            state.update_memory();
        }
    }

    /// Version info read from disk, stored in the metadata blocks `storage_pointers`
    /// (`RowVersionManager::Deserialize`).
    pub fn from_vectors(
        vectors: Vec<Option<ChunkVectorInfo>>,
        storage_pointers: Vec<MetaBlockPointer>,
    ) -> Self {
        Self {
            state: Mutex::new(VersionState {
                vectors,
                uncheckpointed_delete_commit: None,
                needs_compression_check: false,
                storage_pointers,
                memory: None,
            }),
        }
    }

    /// The metadata blocks the deletes are stored in (`GetStoragePointers`).
    pub fn storage_pointers(&self) -> Vec<MetaBlockPointer> {
        self.lock().storage_pointers.clone()
    }

    /// Rows among the first `count` whose delete is committed.
    pub fn committed_deleted_count(&self, count: u64) -> u64 {
        let state = self.lock();
        let mut deleted = 0;
        for (index, info) in state.vectors.iter().enumerate() {
            let start = index as u64 * VECTOR_SIZE;
            if start >= count {
                break;
            }
            if let Some(info) = info {
                let max_count = (count - start).min(VECTOR_SIZE) as usize;
                deleted += info.committed_deleted_count(max_count) as u64;
            }
        }
        deleted
    }

    /// Fills `sel` with the rows of vector `vector_index` the scan sees and returns how
    /// many there are; returns `max_count` (and leaves `sel` empty) if all are visible.
    pub fn sel_vector(
        &self,
        options: ScanOptions,
        vector_index: usize,
        sel: &mut Vec<u32>,
        max_count: usize,
    ) -> usize {
        let state = self.lock();
        if let Some(Some(info)) = state.vectors.get(vector_index) {
            return info.sel_vector(options, sel, max_count);
        }
        sel.clear();
        max_count
    }

    /// Whether row `row` of the row group is visible to the transaction.
    pub fn fetch(&self, transaction: TransactionData, row: u64) -> bool {
        let state = self.lock();
        let vector_index = (row / VECTOR_SIZE) as usize;
        match state.vectors.get(vector_index) {
            Some(Some(info)) => info.fetch(transaction, (row % VECTOR_SIZE) as usize),
            _ => true,
        }
    }

    /// Records rows `start..end` as inserted by the transaction (`AppendVersionInfo`).
    pub fn append_version_info(&self, transaction: TransactionData, start: u64, end: u64) {
        let mut state = self.lock_change();
        state.needs_compression_check = true;
        for (vector, vector_start, vector_end) in vector_ranges(start, end - start) {
            state.fill(vector);
            let vector_row = vector as u64 * VECTOR_SIZE;
            if vector_start == 0 && vector_end == STANDARD_VECTOR_SIZE {
                state.vectors[vector] = Some(ChunkVectorInfo::new(vector_row, transaction.transaction_id));
            } else {
                state.vectors[vector]
                    .get_or_insert_with(|| ChunkVectorInfo::new(vector_row, 0))
                    .append(vector_start, vector_end, transaction.transaction_id);
            }
        }
    }

    pub fn commit_append(&self, commit_id: u64, start: u64, count: u64) {
        let mut state = self.lock_change();
        for (vector, vector_start, vector_end) in vector_ranges(start, count) {
            if let Some(Some(info)) = state.vectors.get_mut(vector) {
                info.commit_append(commit_id, vector_start, vector_end);
            }
        }
    }

    /// Drops version info for vectors whose rows are visible to everybody
    /// (`CleanupAppend`).
    pub fn cleanup_append(&self, lowest_active_transaction: u64, start: u64, count: u64) {
        let mut state = self.lock_change();
        for (vector, _, vector_end) in vector_ranges(start, count) {
            if vector_end != STANDARD_VECTOR_SIZE {
                continue;
            }
            if let Some(slot) = state.vectors.get_mut(vector)
                && slot
                    .as_ref()
                    .is_some_and(|info| info.cleanup(lowest_active_transaction))
            {
                *slot = None;
            }
        }
    }

    /// Forgets the versions of rows at and after `new_count` (`RevertAppend`).
    pub fn revert_append(&self, new_count: u64) {
        let mut state = self.lock_change();
        let first_vector = new_count.div_ceil(VECTOR_SIZE) as usize;
        for slot in state.vectors.iter_mut().skip(first_vector) {
            *slot = None;
        }
    }

    /// Deletes `rows` (offsets within vector `vector_index`) in transaction
    /// `transaction_id`; `rows` is reduced to the rows actually deleted.
    pub fn delete_rows(
        &self,
        vector_index: usize,
        transaction_id: u64,
        rows: &mut Vec<u16>,
    ) -> Result<usize> {
        let mut state = self.lock_change();
        state.needs_compression_check = true;
        state.vector_info(vector_index).delete(transaction_id, rows)
    }

    pub fn commit_delete(&self, vector_index: usize, commit_id: u64, rows: &DeletedRows) {
        let mut state = self.lock_change();
        state.needs_compression_check = true;
        if state
            .uncheckpointed_delete_commit
            .is_none_or(|current| commit_id > current)
        {
            state.uncheckpointed_delete_commit = Some(commit_id);
        }
        state.vector_info(vector_index).commit_delete(commit_id, rows);
    }

    /// Collapses per-row ids where possible (`CompressVersionIds`).
    pub fn compress_version_ids(&self, lowest_active_start: u64) {
        let mut state = self.lock_change();
        if !state.needs_compression_check {
            return;
        }
        let mut pending = false;
        for info in state.vectors.iter_mut().flatten() {
            pending |= info.compress_version_ids(lowest_active_start) == VersionCompression::Pending;
        }
        state.needs_compression_check = pending;
    }

    /// Whether deletes were committed since the last checkpoint.
    pub fn has_unserialized_changes(&self) -> bool {
        self.lock().uncheckpointed_delete_commit.is_some()
    }

    /// Writes the deletes committed as of `checkpoint_id` to new metadata blocks, or
    /// keeps the stored ones when no delete was committed since they were written; returns
    /// the blocks (`RowVersionManager::Checkpoint`).
    pub fn checkpoint(
        &self,
        metadata: &MetadataManager,
        checkpoint_id: u64,
    ) -> Result<Vec<MetaBlockPointer>> {
        let mut state = self.lock();
        if state.uncheckpointed_delete_commit.is_none() {
            // The stored deletes are current: keep their blocks from being freed.
            metadata.clear_modified_blocks(&state.storage_pointers)?;
            return Ok(state.storage_pointers.clone());
        }
        let to_serialize: Vec<(usize, Option<Vec<u64>>)> = state
            .vectors
            .iter()
            .enumerate()
            .filter_map(|(index, info)| {
                let info = info.as_ref()?;
                if !info.has_deletes(checkpoint_id) {
                    return None;
                }
                info.checkpoint_deletes(checkpoint_id)
                    .map(|deletes| (index, deletes))
            })
            .collect();
        state.storage_pointers.clear();
        if !to_serialize.is_empty() {
            let mut writer = MetadataWriter::new(metadata);
            writer.track_written_pointers();
            writer.write_u64(to_serialize.len() as u64)?;
            for (vector_index, deleted) in to_serialize {
                writer.write_u64(vector_index as u64)?;
                write_chunk_deletes(&mut writer, vector_index, deleted.as_deref())?;
            }
            writer.flush()?;
            state.storage_pointers = writer.take_written_pointers();
        }
        if state
            .uncheckpointed_delete_commit
            .is_some_and(|commit| commit <= checkpoint_id)
        {
            state.uncheckpointed_delete_commit = None;
        }
        Ok(state.storage_pointers.clone())
    }
}

/// `ChunkVectorInfo::Write`: the deletes of one vector; `deleted` is `None` when the
/// whole vector is deleted, otherwise a mask with a set bit per deleted row.
fn write_chunk_deletes(
    writer: &mut MetadataWriter<'_>,
    vector_index: usize,
    deleted: Option<&[u64]>,
) -> Result<()> {
    const CONSTANT_INFO: u8 = 0;
    const VECTOR_INFO: u8 = 1;
    let start = (vector_index * STANDARD_VECTOR_SIZE) as u64;
    let Some(mask) = deleted else {
        writer.write_u8(CONSTANT_INFO)?;
        return writer.write_u64(start);
    };
    writer.write_u8(VECTOR_INFO)?;
    writer.write_u64(start)?;
    // `ValidityMask::Write` with valid == deleted.
    let count = STANDARD_VECTOR_SIZE;
    let valid_values = mask.iter().map(|word| word.count_ones() as usize).sum::<usize>();
    let invalid_values = count - valid_values;
    let bitmask_bytes = count / 8;
    let valid_size = 2 * valid_values + 4;
    let invalid_size = 2 * invalid_values + 4;
    if valid_size < bitmask_bytes || invalid_size < bitmask_bytes {
        let serialize_valid = valid_size < invalid_size;
        writer.write_u8(if serialize_valid { 1 } else { 2 })?;
        writer.write_u32(valid_values.min(invalid_values) as u32)?;
        for row in 0..count {
            let is_valid = mask[row / 64] >> (row % 64) & 1 == 1;
            if is_valid == serialize_valid {
                writer.write_data(&(row as u16).to_le_bytes())?;
            }
        }
    } else {
        writer.write_u8(0)?;
        for word in mask {
            writer.write_u64(*word)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::TRANSACTION_ID_START;

    #[test]
    fn appends_span_vectors() {
        let manager = RowVersionManager::default();
        let transaction = TransactionData::new(TRANSACTION_ID_START, 2);
        manager.append_version_info(transaction, 0, 3000);
        let mut sel = Vec::new();
        let other = ScanOptions::new(TransactionData::new(TRANSACTION_ID_START + 1, 2));
        assert_eq!(manager.sel_vector(other, 0, &mut sel, 2048), 0);
        assert_eq!(manager.sel_vector(other, 1, &mut sel, 952), 0);
        manager.commit_append(3, 0, 3000);
        let later = ScanOptions::new(TransactionData::new(TRANSACTION_ID_START + 1, 4));
        assert_eq!(manager.sel_vector(later, 1, &mut sel, 952), 952);
        manager.cleanup_append(4, 0, 3000);
        // the full first vector is cleaned up, the partial second one is kept
        assert_eq!(manager.sel_vector(later, 0, &mut sel, 2048), 2048);
        assert_eq!(sel, Vec::<u32>::new());
        manager.revert_append(2048);
        assert_eq!(manager.sel_vector(other, 1, &mut sel, 10), 10);
    }
}
