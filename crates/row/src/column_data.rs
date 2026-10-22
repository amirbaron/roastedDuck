//! Chunks stored column by column in buffer-managed blocks (DuckDB's
//! `ColumnDataCollection` with its `BUFFER_MANAGER_ALLOCATOR`, as `Sort::MaterializeColumnData`
//! produces it for the window operator).
//!
//! Every vector of a chunk is one allocation in a block of the buffer manager: the values,
//! then the validity words. Strings that do not fit inline are copied into a heap
//! allocation of the chunk; their `string_t`s hold offsets into it ("swizzled"), so a block
//! that was evicted can be reloaded anywhere. Reading a chunk copies its vectors out (rustdb
//! vectors own their buffers; DuckDB references the block memory), fixing the string
//! pointers up and keeping the heap blocks pinned for as long as the strings live.
//!
//! Columns of nested types (LIST, STRUCT, ARRAY) are not supported:
//! [`ColumnDataCollection::supports`] tells.

use std::sync::Arc;

use rustdb_buffer::{BlockHandle, BufferHandle, BufferManager, DestroyBufferUpon, MemoryTag};
use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, PhysicalType, StringT, ValidityMask, Vector};

use crate::format::{FormatKind, SlotMap, VectorFormat};
use crate::gather::HeapKeepAlive;
use crate::row::{load, store};

/// A buffer-managed block and how much of it is used (`BlockMetaData`).
struct Block {
    handle: Arc<BlockHandle>,
    capacity: usize,
    size: usize,
}

/// Where a vector lives (`VectorMetaData`): its values and validity, and the heap of its
/// long strings.
#[derive(Clone, Copy, Debug)]
struct VectorData {
    block: u32,
    offset: u32,
    heap: Option<(u32, u32)>,
}

/// A chunk (`ChunkMetaData`): its vectors and the blocks they use.
#[derive(Debug)]
struct ChunkData {
    count: usize,
    vectors: Vec<VectorData>,
    block_ids: Vec<u32>,
}

/// See the module documentation.
pub struct ColumnDataCollection {
    buffer_manager: BufferManager,
    tag: MemoryTag,
    types: Vec<LogicalType>,
    blocks: Vec<Block>,
    chunks: Vec<ChunkData>,
    count: usize,
}

impl std::fmt::Debug for ColumnDataCollection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnDataCollection")
            .field("types", &self.types)
            .field("count", &self.count)
            .field("chunks", &self.chunks.len())
            .field("blocks", &self.blocks.len())
            .finish_non_exhaustive()
    }
}

/// The blocks an append keeps pinned (`ColumnDataAppendState`).
#[derive(Debug, Default)]
pub struct ColumnDataAppendState {
    pins: Vec<(u32, BufferHandle)>,
}

/// The blocks a scan keeps pinned (`ChunkManagementState`).
#[derive(Debug, Default)]
pub struct ColumnDataScanState {
    pins: Vec<(u32, Arc<BufferHandle>)>,
}

/// Bytes of the values of `count` rows of `width` bytes, rounded up to whole words
/// (`GetDataSize`).
fn values_size(width: usize, count: usize) -> usize {
    (width * count).next_multiple_of(8)
}

/// Bytes of the validity words of `count` rows.
fn validity_size(count: usize) -> usize {
    count.div_ceil(64) * 8
}

/// The width of a vector's values, or `None` for unsupported (nested) types.
fn value_width(ty: &LogicalType) -> Option<usize> {
    match ty.physical_type() {
        PhysicalType::Varchar => Some(size_of::<StringT>()),
        PhysicalType::List
        | PhysicalType::Array
        | PhysicalType::Struct
        | PhysicalType::Bit
        | PhysicalType::Unknown
        | PhysicalType::Invalid => None,
        other => Some(other.size()),
    }
}

impl ColumnDataCollection {
    /// Whether columns of `types` can be stored.
    pub fn supports(types: &[LogicalType]) -> bool {
        types.iter().all(|ty| value_width(ty).is_some())
    }

    /// An empty collection of chunks of `types`, charged to `tag`.
    pub fn new(buffer_manager: BufferManager, types: Vec<LogicalType>, tag: MemoryTag) -> Result<Self> {
        if !Self::supports(&types) {
            return Err(Error::internal("column data collection of a nested type"));
        }
        Ok(Self {
            buffer_manager,
            tag,
            types,
            blocks: Vec::new(),
            chunks: Vec::new(),
            count: 0,
        })
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    /// Number of rows.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Number of chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// The number of rows of every chunk, in order.
    pub fn chunk_row_counts(&self) -> impl Iterator<Item = usize> + '_ {
        self.chunks.iter().map(|chunk| chunk.count)
    }

    /// Reserves `size` bytes in the last block or a new one, pinned by `state`
    /// (`ColumnDataAllocator::AllocateBuffer`); returns the block, the offset and the
    /// address of the reservation.
    fn allocate(&mut self, state: &mut ColumnDataAppendState, size: usize) -> Result<(u32, u32, *mut u8)> {
        let size = size.max(1).next_multiple_of(8);
        let fits = self
            .blocks
            .last()
            .is_some_and(|block| block.capacity - block.size >= size);
        if !fits {
            let pin =
                self.buffer_manager
                    .allocate(self.tag, size.max(self.buffer_manager.block_size()), false)?;
            self.blocks.push(Block {
                handle: Arc::clone(pin.block()),
                capacity: pin.len(),
                size: 0,
            });
            state.pins.push((self.blocks.len() as u32 - 1, pin));
        }
        let block_id = self.blocks.len() as u32 - 1;
        let block = &mut self.blocks[block_id as usize];
        let offset = block.size as u32;
        block.size += size;
        Ok((block_id, offset, self.pinned_address(state, block_id, offset)?))
    }

    /// Appends the rows of `chunk` as a new chunk (`Append`).
    pub fn append(&mut self, state: &mut ColumnDataAppendState, chunk: &DataChunk) -> Result<()> {
        let count = chunk.len();
        if count == 0 {
            return Ok(());
        }
        if count > STANDARD_VECTOR_SIZE || chunk.column_count() != self.types.len() {
            return Err(Error::internal("chunk does not fit the column data collection"));
        }
        // Keep only the last block pinned from earlier chunks.
        let last = self.blocks.len() as u32;
        state.pins.retain(|(id, _)| id + 1 == last);
        let mut vectors = Vec::with_capacity(self.types.len());
        let mut block_ids = Vec::new();
        for (column, ty) in chunk.columns().iter().zip(self.types.clone()) {
            let vector = if column.kind() == rustdb_types::VectorKind::Sequence {
                column.flattened(count)
            } else {
                column.clone()
            };
            let format = VectorFormat::new(&vector, count)?;
            let data = self.append_vector(state, &format, &ty, count)?;
            for id in std::iter::once(data.block).chain(data.heap.map(|(block, _)| block)) {
                if !block_ids.contains(&id) {
                    block_ids.push(id);
                }
            }
            vectors.push(data);
        }
        self.chunks.push(ChunkData {
            count,
            vectors,
            block_ids,
        });
        self.count += count;
        Ok(())
    }

    /// Copies one vector into the collection (`ColumnDataCopy`).
    fn append_vector(
        &mut self,
        state: &mut ColumnDataAppendState,
        format: &VectorFormat<'_>,
        ty: &LogicalType,
        count: usize,
    ) -> Result<VectorData> {
        let width = value_width(ty).ok_or_else(|| Error::internal("column data of a nested type"))?;
        let values = values_size(width, count);
        let (block, offset, base) = self.allocate(state, values + validity_size(count))?;
        // SAFETY (whole block): `base` holds `values` bytes of values followed by the
        // validity words, in a pinned block reserved for this vector.
        unsafe {
            let words = base.add(values);
            for word in 0..count.div_ceil(64) {
                store(u64::MAX, words.add(word * 8));
            }
            if !format.all_valid() {
                for row in 0..count {
                    if !format.slot_is_valid(format.slot(row)) {
                        let word = words.add(row / 64 * 8);
                        store(load::<u64>(word) & !(1 << (row % 64)), word);
                    }
                }
            }
            match format.kind {
                FormatKind::Fixed(width) => {
                    if matches!(format.sel, SlotMap::Identity) {
                        std::ptr::copy_nonoverlapping(format.slot_ptr(0, width), base, width * count);
                    } else {
                        for row in 0..count {
                            let slot = format.slot(row);
                            std::ptr::copy_nonoverlapping(
                                format.slot_ptr(slot, width),
                                base.add(row * width),
                                width,
                            );
                        }
                    }
                    Ok(VectorData {
                        block,
                        offset,
                        heap: None,
                    })
                }
                FormatKind::Varchar => {
                    let heap_size: usize = (0..count)
                        .map(|row| {
                            let slot = format.slot(row);
                            if format.slot_is_valid(slot) {
                                let string = format.string(slot);
                                if !string.is_inlined() {
                                    return string.len();
                                }
                            }
                            0
                        })
                        .sum();
                    let heap = if heap_size == 0 {
                        None
                    } else {
                        Some(self.allocate(state, heap_size)?)
                    };
                    // The vector's block may have been unpinned while allocating the heap:
                    // look its address up again.
                    let base = self.pinned_address(state, block, offset)?;
                    let mut cursor = 0usize;
                    for row in 0..count {
                        let target = base.add(row * 16);
                        let slot = format.slot(row);
                        if !format.slot_is_valid(slot) {
                            std::ptr::write_bytes(target, 0, 16);
                            continue;
                        }
                        let string = format.string(slot);
                        if string.is_inlined() {
                            store(string, target);
                            continue;
                        }
                        let bytes = format.string_bytes(slot);
                        let (_, _, heap_base) = heap.ok_or_else(|| Error::internal("string heap missing"))?;
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), heap_base.add(cursor), bytes.len());
                        // Length and prefix, then the offset of the bytes in the heap.
                        std::ptr::copy_nonoverlapping((&raw const string).cast::<u8>(), target, 8);
                        store(cursor as u64, target.add(8));
                        cursor += bytes.len();
                    }
                    Ok(VectorData {
                        block,
                        offset,
                        heap: heap.map(|(block, offset, _)| (block, offset)),
                    })
                }
                _ => Err(Error::internal("column data of a nested type")),
            }
        }
    }

    /// The address of `offset` in block `block`, which `state` keeps pinned.
    fn pinned_address(
        &mut self,
        state: &mut ColumnDataAppendState,
        block: u32,
        offset: u32,
    ) -> Result<*mut u8> {
        let base = if let Some((_, pin)) = state.pins.iter().find(|(id, _)| *id == block) {
            pin.as_ptr()
        } else {
            let pin = self.buffer_manager.pin(&self.blocks[block as usize].handle)?;
            let ptr = pin.as_ptr();
            state.pins.push((block, pin));
            ptr
        };
        // SAFETY: the offset lies within the pinned block.
        Ok(unsafe { base.add(offset as usize) })
    }

    /// Releases the pins of an append.
    pub fn finalize_append(&self, state: &mut ColumnDataAppendState) {
        state.pins.clear();
    }

    /// Moves the chunks of `other` behind this collection's (`Combine`).
    pub fn combine(&mut self, mut other: ColumnDataCollection) -> Result<()> {
        if other.types != self.types {
            return Err(Error::internal(
                "Attempting to combine ColumnDataCollections with mismatching types",
            ));
        }
        let shift = self.blocks.len() as u32;
        for mut chunk in std::mem::take(&mut other.chunks) {
            for vector in &mut chunk.vectors {
                vector.block += shift;
                if let Some((block, _)) = &mut vector.heap {
                    *block += shift;
                }
            }
            for id in &mut chunk.block_ids {
                *id += shift;
            }
            self.chunks.push(chunk);
        }
        self.blocks.append(&mut other.blocks);
        self.count += other.count;
        other.count = 0;
        Ok(())
    }

    /// Pins the blocks of chunk `chunk_index`, releasing those of earlier chunks it does
    /// not use (`InitializeChunkState`).
    fn initialize_chunk_state(&self, state: &mut ColumnDataScanState, chunk_index: usize) -> Result<()> {
        let chunk = &self.chunks[chunk_index];
        state.pins.retain(|(id, _)| chunk.block_ids.contains(id));
        for &id in &chunk.block_ids {
            if !state.pins.iter().any(|(pinned, _)| *pinned == id) {
                let pin = self.buffer_manager.pin(&self.blocks[id as usize].handle)?;
                state.pins.push((id, Arc::new(pin)));
            }
        }
        Ok(())
    }

    fn pinned(state: &ColumnDataScanState, block: u32) -> Result<&Arc<BufferHandle>> {
        state
            .pins
            .iter()
            .find(|(id, _)| *id == block)
            .map(|(_, pin)| pin)
            .ok_or_else(|| Error::internal("column data block not pinned"))
    }

    /// Reads columns `column_ids` of chunk `chunk_index` into `result` (`ReadChunk`).
    pub fn fetch_chunk(
        &self,
        state: &mut ColumnDataScanState,
        chunk_index: usize,
        column_ids: &[usize],
        result: &mut DataChunk,
    ) -> Result<()> {
        let chunk = self
            .chunks
            .get(chunk_index)
            .ok_or_else(|| Error::internal("column data chunk out of range"))?;
        self.initialize_chunk_state(state, chunk_index)?;
        let count = chunk.count;
        let mut columns = Vec::with_capacity(column_ids.len());
        for &column in column_ids {
            let ty = &self.types[column];
            let data = chunk.vectors[column];
            columns.push(Self::read_vector(state, &data, ty, count)?);
        }
        result.set_columns(columns, count);
        Ok(())
    }

    /// Copies a stored vector out (`ReadVector`), fixing string pointers up
    /// (`UnswizzlePointers`).
    fn read_vector(
        state: &ColumnDataScanState,
        data: &VectorData,
        ty: &LogicalType,
        count: usize,
    ) -> Result<Vector> {
        let width = value_width(ty).ok_or_else(|| Error::internal("column data of a nested type"))?;
        let values = values_size(width, count);
        let pin = Self::pinned(state, data.block)?;
        // SAFETY (whole block): the vector's values and validity words lie in the pinned
        // block at `data.offset`; the target buffer holds `count` values.
        unsafe {
            let base = pin.as_ptr().add(data.offset as usize);
            let mut vector = Vector::flat_for_overwrite(ty.clone(), count.max(1));
            std::ptr::copy_nonoverlapping(base, vector.data_bytes_mut().as_mut_ptr(), width * count);
            let words: Vec<u64> = (0..count.div_ceil(64))
                .map(|word| load::<u64>(base.add(values + word * 8)))
                .collect();
            let full = |word: usize| {
                let rows = (count - word * 64).min(64);
                if rows == 64 { u64::MAX } else { (1u64 << rows) - 1 }
            };
            let all_valid = words
                .iter()
                .enumerate()
                .all(|(index, &word)| word & full(index) == full(index));
            if !all_valid {
                vector.set_validity(ValidityMask::from_words(words));
            }
            if let Some((heap_block, heap_offset)) = data.heap {
                let heap_pin = Self::pinned(state, heap_block)?;
                let heap_base = heap_pin.as_ptr().add(heap_offset as usize);
                let strings = vector.data_bytes_mut().as_mut_ptr();
                for row in 0..count {
                    let target = strings.add(row * 16);
                    if load::<u32>(target) as usize <= StringT::INLINE_LENGTH {
                        continue;
                    }
                    let offset = load::<u64>(target.add(8)) as usize;
                    store(heap_base.add(offset) as usize as u64, target.add(8));
                }
                let keep_alive: HeapKeepAlive = Arc::new(vec![Arc::clone(heap_pin)]);
                vector.add_keep_alive(keep_alive);
            }
            Ok(vector)
        }
    }
}

impl Drop for ColumnDataCollection {
    fn drop(&mut self) {
        // Blocks of a dropped collection need not be written to temporary files.
        for block in &self.blocks {
            block.handle.set_destroy_buffer_upon(DestroyBufferUpon::Unpin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_buffer::BufferManagerConfig;
    use rustdb_types::Value;

    fn buffer_manager() -> BufferManager {
        BufferManager::new(BufferManagerConfig {
            memory_limit: Some(1 << 30),
            ..BufferManagerConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn round_trips_fixed_and_string_columns() {
        let types = vec![LogicalType::BIGINT, LogicalType::VARCHAR, LogicalType::BOOLEAN];
        let mut collection =
            ColumnDataCollection::new(buffer_manager(), types.clone(), MemoryTag::ColumnData).unwrap();
        let mut state = ColumnDataAppendState::default();
        let mut expected = Vec::new();
        for chunk_index in 0..5 {
            let count = if chunk_index == 4 {
                100
            } else {
                STANDARD_VECTOR_SIZE
            };
            let mut columns: Vec<Vector> = types.iter().map(|ty| Vector::flat(ty.clone(), count)).collect();
            for row in 0..count {
                let n = (chunk_index * STANDARD_VECTOR_SIZE + row) as i64;
                let values = [
                    if n % 7 == 0 {
                        Value::null(LogicalType::BIGINT)
                    } else {
                        Value::bigint(n)
                    },
                    if n % 5 == 0 {
                        Value::null(LogicalType::VARCHAR)
                    } else if n % 2 == 0 {
                        Value::varchar(format!("s{n}"))
                    } else {
                        Value::varchar(format!("a long string number {n} that is not inlined"))
                    },
                    Value::boolean(n % 3 == 0),
                ];
                for (column, value) in columns.iter_mut().zip(&values) {
                    column.set_value(row, value).unwrap();
                }
                expected.push(values.to_vec());
            }
            collection
                .append(&mut state, &DataChunk::new(columns, count))
                .unwrap();
        }
        collection.finalize_append(&mut state);
        assert_eq!(collection.count(), expected.len());
        let mut scan = ColumnDataScanState::default();
        let mut row_index = 0;
        for chunk_index in 0..collection.chunk_count() {
            let mut chunk = DataChunk::default();
            collection
                .fetch_chunk(&mut scan, chunk_index, &[0, 1, 2], &mut chunk)
                .unwrap();
            for row in 0..chunk.len() {
                for (column, value) in expected[row_index].iter().enumerate() {
                    assert_eq!(&chunk.column(column).get_value(row), value);
                }
                row_index += 1;
            }
        }
        assert_eq!(row_index, expected.len());
    }
}
