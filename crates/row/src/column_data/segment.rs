//! A segment of a column data collection: its chunks and the vectors they consist of
//! (DuckDB's `ColumnDataCollectionSegment`).
//!
//! Every vector of a chunk has room for `STANDARD_VECTOR_SIZE` values followed by a
//! validity mask (`GetDataSize` + `STANDARD_MASK_SIZE`). Child vectors of lists and arrays
//! that hold more values continue in further vectors linked by `next_data`; the bytes of
//! long strings go to heaps allocated in the same blocks, referenced by `swizzle_data`.

use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{DataChunk, LogicalType, PhysicalType, StringT, Vector};

use super::allocator::{ChunkManagementState, ColumnDataAllocator};
use crate::gather::HeapKeepAlive;

/// Bytes of a vector's validity mask (`ValidityMask::STANDARD_MASK_SIZE`).
pub(crate) const STANDARD_MASK_SIZE: usize = STANDARD_VECTOR_SIZE.div_ceil(64) * 8;

/// `AlignValue`: rounds up to a multiple of 8.
pub(crate) const fn align_value(size: usize) -> usize {
    size.div_ceil(8) * 8
}

/// The bytes of `STANDARD_VECTOR_SIZE` values of `type_size` bytes (`GetDataSize`).
pub(crate) const fn data_size(type_size: usize) -> usize {
    align_value(type_size * STANDARD_VECTOR_SIZE)
}

/// The size of one value of `ty` in a vector's data (`GetTypeIdSize`; 0 for STRUCT and
/// ARRAY, whose values live in their children).
pub(crate) fn type_size(ty: &LogicalType) -> usize {
    match ty.physical_type() {
        PhysicalType::Struct | PhysicalType::Array => 0,
        physical => physical.size(),
    }
}

/// Strings of a vector whose bytes are in a heap (`SwizzleMetaData`): `count` strings
/// starting at `offset` point into heap `child_index`, which was at `ptr` when they were
/// written.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SwizzleMetaData {
    pub(crate) child_index: usize,
    pub(crate) ptr: usize,
    pub(crate) offset: u16,
    pub(crate) count: u16,
}

/// Where a vector's data lives and what belongs to it (`VectorMetaData`).
#[derive(Clone, Debug)]
pub(crate) struct VectorMetaData {
    pub(crate) block_id: u32,
    pub(crate) offset: u32,
    /// The number of values.
    pub(crate) count: u16,
    /// The string heaps the values point into.
    pub(crate) swizzle_data: Vec<SwizzleMetaData>,
    /// The first child (lists, arrays, structs) or the first string heap, as an index into
    /// the segment's `child_indices`.
    pub(crate) child_index: Option<usize>,
    /// The vector holding the next values (children of lists and arrays).
    pub(crate) next_data: Option<usize>,
}

/// The vectors of one chunk and the blocks they use (`ChunkMetaData`).
#[derive(Clone, Debug, Default)]
pub(crate) struct ChunkMetaData {
    pub(crate) vector_data: Vec<usize>,
    pub(crate) block_ids: Vec<u32>,
    pub(crate) count: u16,
}

impl ChunkMetaData {
    fn add_block(&mut self, block_id: u32) {
        if !self.block_ids.contains(&block_id) {
            self.block_ids.push(block_id);
        }
    }
}

/// The chunks of a collection allocated from one allocator (`ColumnDataCollectionSegment`).
#[derive(Debug)]
pub(crate) struct ColumnDataCollectionSegment {
    pub(crate) allocator: Arc<ColumnDataAllocator>,
    pub(crate) types: Vec<LogicalType>,
    pub(crate) count: usize,
    pub(crate) chunk_data: Vec<ChunkMetaData>,
    pub(crate) vector_data: Vec<VectorMetaData>,
    pub(crate) child_indices: Vec<Option<usize>>,
}

/// Whether the mask of `count` values at `mask` has an invalid value
/// (`GetValidityPointer` returning a mask).
///
/// # Safety
/// `mask` must point to `STANDARD_MASK_SIZE` readable bytes.
unsafe fn has_invalid(mask: *const u8, count: usize) -> bool {
    let words = mask.cast::<u64>();
    for i in 0..count / 64 {
        // SAFETY: `i < count / 64 <= STANDARD_VECTOR_SIZE / 64` words of the mask.
        if unsafe { words.add(i).read_unaligned() } != u64::MAX {
            return true;
        }
    }
    if count % 64 != 0 {
        let mask_bits = (1u64 << (count % 64)) - 1;
        // SAFETY: as above, the word holding the last values.
        if unsafe { words.add(count / 64).read_unaligned() } & mask_bits != mask_bits {
            return true;
        }
    }
    false
}

impl ColumnDataCollectionSegment {
    pub(crate) fn new(allocator: Arc<ColumnDataAllocator>, types: Vec<LogicalType>) -> Self {
        Self {
            allocator,
            types,
            count: 0,
            chunk_data: Vec::new(),
            vector_data: Vec::new(),
            child_indices: Vec::new(),
        }
    }

    /// `AllocateVectorInternal`.
    fn allocate_vector_internal(
        &mut self,
        ty: &LogicalType,
        chunk_index: usize,
        state: Option<&mut ChunkManagementState>,
    ) -> Result<usize> {
        let (block_id, offset) = self
            .allocator
            .allocate_data(data_size(type_size(ty)) + STANDARD_MASK_SIZE, state)?;
        self.chunk_data[chunk_index].add_block(block_id);
        self.vector_data.push(VectorMetaData {
            block_id,
            offset,
            count: 0,
            swizzle_data: Vec::new(),
            child_index: None,
            next_data: None,
        });
        Ok(self.vector_data.len() - 1)
    }

    /// Allocates a vector of `ty` (and the children of a STRUCT) for chunk `chunk_index`,
    /// continuing `prev_index` if given (`AllocateVector`).
    pub(crate) fn allocate_vector(
        &mut self,
        ty: &LogicalType,
        chunk_index: usize,
        mut state: Option<&mut ChunkManagementState>,
        prev_index: Option<usize>,
    ) -> Result<usize> {
        let index = self.allocate_vector_internal(ty, chunk_index, state.as_deref_mut())?;
        if let Some(prev) = prev_index {
            self.vector_data[prev].next_data = Some(index);
        }
        if ty.physical_type() == PhysicalType::Struct {
            let child_types: Vec<LogicalType> = ty
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(_, child)| child.clone())
                .collect();
            let base_child_index = self.reserve_children(child_types.len());
            for (child_idx, child_type) in child_types.iter().enumerate() {
                let prev_child_index = prev_index.map(|prev| {
                    let child_index = self.vector_data[prev].child_index;
                    self.child_at(child_index, child_idx)
                });
                let prev_child_index = match prev_child_index {
                    Some(index) => Some(index?),
                    None => None,
                };
                let child_index =
                    self.allocate_vector(child_type, chunk_index, state.as_deref_mut(), prev_child_index)?;
                self.child_indices[base_child_index + child_idx] = Some(child_index);
            }
            self.vector_data[index].child_index = Some(base_child_index);
        }
        Ok(index)
    }

    /// Allocates a heap of `size` bytes for the strings of chunk `chunk_index`, after heap
    /// `prev_index` if given (`AllocateStringHeap`).
    pub(crate) fn allocate_string_heap(
        &mut self,
        size: usize,
        chunk_index: usize,
        state: &mut ChunkManagementState,
        prev_index: Option<usize>,
    ) -> Result<usize> {
        let (block_id, offset) = self.allocator.allocate_data(align_value(size), Some(state))?;
        self.chunk_data[chunk_index].add_block(block_id);
        self.vector_data.push(VectorMetaData {
            block_id,
            offset,
            count: 0,
            swizzle_data: Vec::new(),
            child_index: None,
            next_data: None,
        });
        let index = self.vector_data.len() - 1;
        if let Some(prev) = prev_index {
            self.vector_data[prev].next_data = Some(index);
        }
        Ok(index)
    }

    /// Starts a chunk with a vector per column (`AllocateNewChunk`).
    pub(crate) fn allocate_new_chunk(&mut self) -> Result<()> {
        self.chunk_data.push(ChunkMetaData::default());
        let chunk_index = self.chunk_data.len() - 1;
        let types = self.types.clone();
        let mut vector_data = Vec::with_capacity(types.len());
        for ty in &types {
            vector_data.push(self.allocate_vector(ty, chunk_index, None, None)?);
        }
        self.chunk_data[chunk_index].vector_data = vector_data;
        Ok(())
    }

    /// Pins the blocks of chunk `chunk_index` in `state` (`InitializeChunkState`).
    pub(crate) fn initialize_chunk_state(&self, chunk_index: usize, state: &mut ChunkManagementState) -> Result<()> {
        self.allocator
            .initialize_chunk_state(state, &self.chunk_data[chunk_index].block_ids)
    }

    /// Child `child_entry` of the children starting at `index` (`GetChildIndex`).
    pub(crate) fn child_at(&self, index: Option<usize>, child_entry: usize) -> Result<usize> {
        index
            .and_then(|index| self.child_indices.get(index + child_entry).copied().flatten())
            .ok_or_else(|| Error::internal("Column Data Collection: missing child vector"))
    }

    /// `AddChildIndex`.
    pub(crate) fn add_child_index(&mut self, index: usize) -> usize {
        self.child_indices.push(Some(index));
        self.child_indices.len() - 1
    }

    /// `ReserveChildren`.
    fn reserve_children(&mut self, child_count: usize) -> usize {
        let result = self.child_indices.len();
        self.child_indices
            .resize(self.child_indices.len() + child_count, None);
        result
    }

    /// Reads the vector at `vector_index` (and the vectors continuing it) as a flat vector
    /// of `ty` (`ReadVector`); returns it with its number of values.
    pub(crate) fn read_vector(
        &self,
        state: &ChunkManagementState,
        vector_index: usize,
        ty: &LogicalType,
    ) -> Result<(Vector, usize)> {
        let vdata = &self.vector_data[vector_index];
        if vdata.count == 0 {
            return Ok((Vector::flat(ty.clone(), 1), 0));
        }
        let (mut result, count) = self.read_vector_internal(state, vector_index, ty)?;
        match ty.physical_type() {
            PhysicalType::List => {
                let child_type = ty
                    .child_type()
                    .ok_or_else(|| Error::internal("LIST without a child type"))?;
                let (child, child_count) =
                    self.read_vector(state, self.child_at(vdata.child_index, 0)?, child_type)?;
                result.set_list_child(child, child_count);
            }
            PhysicalType::Array => {
                let child_type = ty
                    .child_type()
                    .ok_or_else(|| Error::internal("ARRAY without a child type"))?;
                let (child, _) = self.read_vector(state, self.child_at(vdata.child_index, 0)?, child_type)?;
                result.set_array_child(child);
            }
            PhysicalType::Struct => {
                let child_types = ty.struct_children().unwrap_or_default();
                let mut children = Vec::with_capacity(child_types.len());
                for (child_idx, (_, child_type)) in child_types.iter().enumerate() {
                    let (child, child_count) =
                        self.read_vector(state, self.child_at(vdata.child_index, child_idx)?, child_type)?;
                    if child_count != count {
                        return Err(Error::internal(
                            "Column Data Collection: mismatch in struct child sizes",
                        ));
                    }
                    children.push(child);
                }
                if let Some(slots) = result.struct_children_mut() {
                    for (slot, child) in slots.iter_mut().zip(children) {
                        *slot = child;
                    }
                }
            }
            PhysicalType::Varchar => self.read_strings(state, vector_index, &mut result)?,
            _ => {}
        }
        Ok((result, count))
    }

    /// Copies the values and validity of a vector and its continuations
    /// (`ReadVectorInternal` without the zero-copy read: rustdb's vectors own their data).
    fn read_vector_internal(
        &self,
        state: &ChunkManagementState,
        vector_index: usize,
        ty: &LogicalType,
    ) -> Result<(Vector, usize)> {
        let width = type_size(ty);
        let mut vector_count = 0;
        let mut next_index = Some(vector_index);
        while let Some(index) = next_index {
            vector_count += self.vector_data[index].count as usize;
            next_index = self.vector_data[index].next_data;
        }
        let mut result = if width > 0 && ty.physical_type() != PhysicalType::Varchar {
            Vector::flat_for_overwrite(ty.clone(), vector_count.max(1))
        } else {
            Vector::flat(ty.clone(), vector_count.max(1))
        };
        let mut current_offset = 0;
        let mut next_index = Some(vector_index);
        while let Some(index) = next_index {
            let vdata = &self.vector_data[index];
            let count = vdata.count as usize;
            let base_ptr = ColumnDataAllocator::data_pointer(state, vdata.block_id, vdata.offset)?;
            if width > 0 {
                let target = &mut result.data_bytes_mut()[current_offset * width..(current_offset + count) * width];
                // SAFETY: the vector's data holds `STANDARD_VECTOR_SIZE >= count` values of
                // `width` bytes in the pinned block.
                let source = unsafe { std::slice::from_raw_parts(base_ptr, count * width) };
                target.copy_from_slice(source);
            }
            // SAFETY: the mask follows the data in the vector's allocation.
            let mask = unsafe { base_ptr.add(data_size(width)) };
            // SAFETY: the mask is `STANDARD_MASK_SIZE` bytes.
            if unsafe { has_invalid(mask, count) } {
                let capacity = result.capacity().max(vector_count);
                let validity = result.validity_mut();
                for row in 0..count {
                    // SAFETY: `row < STANDARD_VECTOR_SIZE`, inside the mask.
                    let word = unsafe { mask.cast::<u64>().add(row / 64).read_unaligned() };
                    if word & (1u64 << (row % 64)) == 0 {
                        validity.set_invalid(current_offset + row, capacity);
                    }
                }
            }
            current_offset += count;
            next_index = vdata.next_data;
        }
        Ok((result, vector_count))
    }

    /// Points the long strings read into `result` at their heaps in the pinned blocks,
    /// which the vector keeps pinned (`UnswizzlePointers`, without writing the pointers
    /// back to the blocks).
    fn read_strings(&self, state: &ChunkManagementState, vector_index: usize, result: &mut Vector) -> Result<()> {
        let mut pins: Vec<Arc<rustdb_buffer::BufferHandle>> = Vec::new();
        let validity = result.validity().clone();
        let strings = result.values_mut::<StringT>();
        let mut v_offset = 0;
        let mut next_index = Some(vector_index);
        while let Some(index) = next_index {
            let vdata = &self.vector_data[index];
            for swizzle in &vdata.swizzle_data {
                let heap = &self.vector_data[swizzle.child_index];
                let pin = state
                    .find(heap.block_id)
                    .ok_or_else(|| Error::internal("ColumnDataAllocator: block of the chunk is not pinned"))?;
                // SAFETY: the heap lies inside the pinned block.
                let new_base = unsafe { pin.as_ptr().add(heap.offset as usize) } as usize;
                if !pins.iter().any(|pinned| Arc::ptr_eq(pinned, pin)) {
                    pins.push(Arc::clone(pin));
                }
                let start = v_offset + swizzle.offset as usize;
                for row in start..start + swizzle.count as usize {
                    let string = strings[row];
                    if !validity.is_valid(row) || string.is_inlined() {
                        continue;
                    }
                    // SAFETY: only the pointer value is read, not the memory behind it.
                    let old_ptr = unsafe { StringT::data_ptr(&raw const strings[row]) } as usize;
                    let new_ptr = (new_base + (old_ptr - swizzle.ptr)) as *const u8;
                    // SAFETY: the string's bytes are at this place of the heap, which the
                    // vector keeps pinned below.
                    strings[row] = unsafe { StringT::from_raw(std::slice::from_raw_parts(new_ptr, string.len())) };
                }
            }
            v_offset += vdata.count as usize;
            next_index = vdata.next_data;
        }
        if !pins.is_empty() {
            let keep_alive: HeapKeepAlive = Arc::new(pins);
            result.add_keep_alive(keep_alive);
        }
        Ok(())
    }

    /// Reads columns `column_ids` of chunk `chunk_index` (`ReadChunk`).
    pub(crate) fn read_chunk(
        &self,
        chunk_index: usize,
        state: &mut ChunkManagementState,
        column_ids: &[usize],
    ) -> Result<DataChunk> {
        self.initialize_chunk_state(chunk_index, state)?;
        let chunk_meta = &self.chunk_data[chunk_index];
        let mut columns = Vec::with_capacity(column_ids.len());
        for &column in column_ids {
            let (vector, _) = self.read_vector(state, chunk_meta.vector_data[column], &self.types[column])?;
            columns.push(vector);
        }
        Ok(DataChunk::new(columns, chunk_meta.count as usize))
    }

    /// `ChunkCount`.
    pub(crate) fn chunk_count(&self) -> usize {
        self.chunk_data.len()
    }
}
