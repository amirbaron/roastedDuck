//! The collection (DuckDB's `ColumnDataCollection`, `column_data_collection.cpp`):
//! appending copies each chunk's vectors into the collection's blocks with a copy function
//! per column type (`GetCopyFunction`); reading copies them back into flat vectors.

use std::borrow::Cow;
use std::sync::Arc;

use roastedduck_buffer::{BufferManager, MemoryTag};
use roastedduck_common::{Error, Result, STANDARD_VECTOR_SIZE};
use roastedduck_types::{
    DataChunk, ListEntry, LogicalType, PhysicalType, Sel, SelectionVector, StringT, ValidityMask, Vector,
    VectorKind,
};

use super::allocator::{ChunkManagementState, ColumnDataAllocator};
use super::segment::{ColumnDataCollectionSegment, STANDARD_MASK_SIZE, SwizzleMetaData, data_size};
use crate::row::{load, store};

/// How the values of a column are copied in (`ColumnDataCopyFunction`).
#[derive(Clone, Debug)]
enum CopyFunction {
    /// Values of a fixed width (`ColumnDataCopy<T>`).
    Fixed(usize),
    /// Strings, whose long bytes go to heaps in the blocks (`ColumnDataCopy<string_t>`).
    String,
    /// `ColumnDataCopy<list_entry_t>` with the child's function.
    List(Box<CopyFunction>),
    /// `ColumnDataCopyStruct` with the fields' functions.
    Struct(Vec<CopyFunction>),
    /// `ColumnDataCopyArray` with the element's function.
    Array(Box<CopyFunction>),
}

impl CopyFunction {
    /// `GetCopyFunction`.
    fn for_type(ty: &LogicalType) -> Result<Self> {
        Ok(match ty.physical_type() {
            physical @ (PhysicalType::Bool
            | PhysicalType::Int8
            | PhysicalType::UInt8
            | PhysicalType::Int16
            | PhysicalType::UInt16
            | PhysicalType::Int32
            | PhysicalType::UInt32
            | PhysicalType::Int64
            | PhysicalType::UInt64
            | PhysicalType::Int128
            | PhysicalType::UInt128
            | PhysicalType::Float
            | PhysicalType::Double
            | PhysicalType::Interval) => Self::Fixed(physical.size()),
            PhysicalType::Varchar => Self::String,
            PhysicalType::Struct => Self::Struct(
                ty.struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(_, child)| Self::for_type(child))
                    .collect::<Result<_>>()?,
            ),
            PhysicalType::List => Self::List(Box::new(Self::for_type(
                ty.child_type()
                    .ok_or_else(|| Error::internal("LIST without a child type"))?,
            )?)),
            PhysicalType::Array => Self::Array(Box::new(Self::for_type(
                ty.child_type()
                    .ok_or_else(|| Error::internal("ARRAY without a child type"))?,
            )?)),
            physical => {
                return Err(Error::internal(format!(
                    "Unsupported type {} for ColumnDataCollection::GetCopyFunction",
                    physical.name()
                )));
            }
        })
    }
}

/// A vector seen in DuckDB's unified format: its data, selection and validity
/// (`UnifiedVectorFormat`). Sequences, and nested vectors that are not flat, are
/// flattened first.
struct Source<'a> {
    vector: Cow<'a, Vector>,
    validity: Cow<'a, ValidityMask>,
}

impl<'a> Source<'a> {
    fn new(vector: &'a Vector, count: usize) -> Self {
        let nested = matches!(
            vector.logical_type().physical_type(),
            PhysicalType::Struct | PhysicalType::List | PhysicalType::Array
        );
        let vector = if vector.kind() == VectorKind::Sequence || (nested && vector.kind() != VectorKind::Flat)
        {
            Cow::Owned(vector.flattened(count))
        } else {
            Cow::Borrowed(vector)
        };
        // The validity is a cheap clone (shared words) of the vector's own.
        let validity = Cow::Owned(vector.validity().clone());
        Self { vector, validity }
    }

    fn sel(&self) -> Sel<'_> {
        self.vector.sel()
    }

    fn data(&self) -> *const u8 {
        self.vector.data_bytes().as_ptr()
    }

    fn is_valid(&self, slot: usize) -> bool {
        self.validity.is_valid(slot)
    }

    fn strings(&self) -> &[StringT] {
        self.vector.values::<StringT>()
    }
}

/// What a copy works on (`ColumnDataMetaData`): the segment, the append's pins and the
/// chunk being appended to.
struct CopyContext<'a> {
    segment: &'a mut ColumnDataCollectionSegment,
    state: &'a mut ChunkManagementState,
    chunk_index: usize,
}

/// Sets the first `STANDARD_VECTOR_SIZE` validity bits at `mask` (`SetAllValid`).
///
/// # Safety
/// `mask` must point to `STANDARD_MASK_SIZE` writable bytes.
unsafe fn set_all_valid(mask: *mut u8) {
    // SAFETY: guaranteed by the caller.
    unsafe { std::ptr::write_bytes(mask, 0xFF, STANDARD_MASK_SIZE) };
}

/// Clears validity bit `row` at `mask` (`SetInvalid`).
///
/// # Safety
/// `mask` must point to `STANDARD_MASK_SIZE` writable bytes and `row` be below
/// `STANDARD_VECTOR_SIZE`.
unsafe fn set_invalid(mask: *mut u8, row: usize) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let word = mask.add(row / 64 * 8);
        store(load::<u64>(word) & !(1u64 << (row % 64)), word);
    }
}

/// Copies `count` values of `source` from row `offset` into the vector `vector_index`
/// and the vectors continuing it (`TemplatedColumnDataCopy`). `assign` writes the value
/// of source slot `slot` to position `target` of the vector data at `base`.
fn templated_copy(
    ctx: &mut CopyContext<'_>,
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    count: usize,
    value_size: usize,
    assign: impl FnMut(*mut u8, usize, usize),
) -> Result<()> {
    templated_copy_with(ctx, vector_index, source, offset, count, value_size, assign, None)
}

/// [`templated_copy`] with `bulk`, which writes `n` consecutive source slots from `slot`
/// to positions from `target` at `base` in one go, for sources without a selection: the
/// values are copied whole (a NULL's slot too, which nothing reads) and the NULLs marked
/// after.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters of DuckDB's copy plus the bulk copy"
)]
fn templated_copy_with(
    ctx: &mut CopyContext<'_>,
    vector_index: usize,
    source: &Source<'_>,
    mut offset: usize,
    count: usize,
    value_size: usize,
    mut assign: impl FnMut(*mut u8, usize, usize),
    bulk: Option<&dyn Fn(*mut u8, usize, usize, usize)>,
) -> Result<()> {
    let bulk = bulk.filter(|_| matches!(source.sel(), Sel::Identity));
    let mut current_index = vector_index;
    let mut remaining = count;
    let all_valid = source.validity.is_all_valid();
    let sel = source.sel();
    while remaining > 0 {
        let current = &ctx.segment.vector_data[current_index];
        let current_count = current.count as usize;
        let append_count = (STANDARD_VECTOR_SIZE - current_count).min(remaining);
        let base_ptr = ColumnDataAllocator::data_pointer(ctx.state, current.block_id, current.offset)?;
        // SAFETY: the mask follows the data in the vector's allocation.
        let mask = unsafe { base_ptr.add(data_size(value_size)) };
        if current_count == 0 {
            // SAFETY: the mask is `STANDARD_MASK_SIZE` bytes in the pinned block.
            unsafe { set_all_valid(mask) };
        }
        if let Some(bulk) = bulk {
            bulk(base_ptr, current_count, offset, append_count);
            if !all_valid {
                for i in 0..append_count {
                    if !source.is_valid(offset + i) {
                        // SAFETY: `current_count + i < STANDARD_VECTOR_SIZE`.
                        unsafe { set_invalid(mask, current_count + i) };
                    }
                }
            }
        } else if all_valid {
            for i in 0..append_count {
                assign(base_ptr, current_count + i, sel.slot(offset + i));
            }
        } else {
            for i in 0..append_count {
                let slot = sel.slot(offset + i);
                if source.is_valid(slot) {
                    assign(base_ptr, current_count + i, slot);
                } else {
                    // SAFETY: `current_count + i < STANDARD_VECTOR_SIZE`.
                    unsafe { set_invalid(mask, current_count + i) };
                }
            }
        }
        ctx.segment.vector_data[current_index].count += append_count as u16;
        offset += append_count;
        remaining -= append_count;
        if remaining > 0 {
            // Continue in the next vector, allocating it if needed.
            if ctx.segment.vector_data[current_index].next_data.is_none() {
                ctx.segment.allocate_vector(
                    source.vector.logical_type(),
                    ctx.chunk_index,
                    Some(ctx.state),
                    Some(current_index),
                )?;
            }
            current_index = ctx.segment.vector_data[current_index]
                .next_data
                .ok_or_else(|| Error::internal("ColumnDataCollection: missing continuation vector"))?;
        }
    }
    Ok(())
}

/// `ColumnDataCopy<T>` for values of `width` bytes.
fn copy_fixed(
    ctx: &mut CopyContext<'_>,
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    count: usize,
    width: usize,
) -> Result<()> {
    match width {
        1 => copy_fixed_width::<1>(ctx, vector_index, source, offset, count),
        2 => copy_fixed_width::<2>(ctx, vector_index, source, offset, count),
        4 => copy_fixed_width::<4>(ctx, vector_index, source, offset, count),
        8 => copy_fixed_width::<8>(ctx, vector_index, source, offset, count),
        16 => copy_fixed_width::<16>(ctx, vector_index, source, offset, count),
        _ => Err(Error::internal(format!(
            "ColumnDataCollection: no copy for values of {width} bytes"
        ))),
    }
}

/// [`copy_fixed`] for values of `W` bytes.
fn copy_fixed_width<const W: usize>(
    ctx: &mut CopyContext<'_>,
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    count: usize,
) -> Result<()> {
    let data = source.data();
    let bulk = |base: *mut u8, target: usize, slot: usize, n: usize| {
        // SAFETY: slots `slot..slot + n` are in the source vector's data and positions
        // `target..target + n` below `STANDARD_VECTOR_SIZE` of the target vector's data.
        unsafe { std::ptr::copy_nonoverlapping(data.add(slot * W), base.add(target * W), n * W) };
    };
    templated_copy_with(
        ctx,
        vector_index,
        source,
        offset,
        count,
        W,
        |base, target, slot| {
            // SAFETY: `slot` is a slot of the source vector's data and `target` a position
            // below `STANDARD_VECTOR_SIZE` of the target vector's data, both `W` bytes.
            unsafe { std::ptr::copy_nonoverlapping(data.add(slot * W), base.add(target * W), W) };
        },
        Some(&bulk),
    )
}

/// `ColumnDataCopyCompressedStrings`: a constant string, or a dictionary from storage
/// much smaller than the rows, is copied into the heap once. Returns `false` (doing
/// nothing) when that does not apply.
#[expect(clippy::too_many_arguments, reason = "mirrors DuckDB's helper")]
fn copy_compressed_strings(
    ctx: &mut CopyContext<'_>,
    vector_index: usize,
    current_index: usize,
    child_index: &mut Option<usize>,
    source: &Source<'_>,
    offset: usize,
    vector_remaining: usize,
    append_count: &mut usize,
    heap_size: &mut usize,
    base_heap_ptr: &mut *mut u8,
) -> Result<bool> {
    let vector = source.vector.as_ref();
    let dictionary = match vector.kind() {
        VectorKind::Constant => {
            let constant = vector.values::<StringT>()[0];
            if vector.is_null(0) || constant.is_inlined() {
                return Ok(false);
            }
            *heap_size = constant.len();
            None
        }
        VectorKind::Dictionary => {
            let Some((dictionary, _, info)) = vector.storage_dictionary_parts() else {
                return Ok(false);
            };
            let dictionary_size = info.size();
            if dictionary_size >= vector_remaining / 2 {
                return Ok(false);
            }
            let strings = dictionary.values::<StringT>();
            let validity = dictionary.validity();
            for (i, string) in strings.iter().enumerate().take(dictionary_size) {
                if validity.is_valid(i) && !string.is_inlined() {
                    *heap_size += string.len();
                }
            }
            if *heap_size == 0 {
                return Ok(false);
            }
            Some((dictionary, dictionary_size))
        }
        _ => return Ok(false),
    };

    *child_index =
        Some(
            ctx.segment
                .allocate_string_heap(*heap_size, ctx.chunk_index, ctx.state, *child_index)?,
        );
    let heap_index = child_index.ok_or_else(|| Error::internal("string heap not allocated"))?;
    if ctx.segment.vector_data[vector_index].child_index.is_none() {
        ctx.segment.vector_data[vector_index].child_index = Some(ctx.segment.add_child_index(heap_index));
    }
    let heap = &ctx.segment.vector_data[heap_index];
    *base_heap_ptr = ColumnDataAllocator::data_pointer(ctx.state, heap.block_id, heap.offset)?;

    let current = &ctx.segment.vector_data[current_index];
    let current_count = current.count as usize;
    let base_ptr = ColumnDataAllocator::data_pointer(ctx.state, current.block_id, current.offset)?;
    // SAFETY: the mask follows the string data in the vector's allocation.
    let mask = unsafe { base_ptr.add(data_size(size_of::<StringT>())) };
    if current_count == 0 {
        // SAFETY: the mask is `STANDARD_MASK_SIZE` bytes in the pinned block.
        unsafe { set_all_valid(mask) };
    }
    let targets = base_ptr;
    match dictionary {
        None => {
            let constant = vector.values::<StringT>()[0];
            let bytes = vector.string_value(&constant);
            // SAFETY: the heap has room for the constant's bytes; the string points into it.
            let string = unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), *base_heap_ptr, bytes.len());
                StringT::from_raw(std::slice::from_raw_parts(*base_heap_ptr, bytes.len()))
            };
            for i in 0..vector_remaining {
                // SAFETY: `current_count + i < STANDARD_VECTOR_SIZE` values of the vector.
                unsafe { store(string, targets.add((current_count + i) * size_of::<StringT>())) };
            }
        }
        Some((dictionary, dictionary_size)) => {
            let strings = dictionary.values::<StringT>();
            let validity = dictionary.validity();
            let mut string_offsets = vec![0usize; dictionary_size];
            let mut current_string_offset = 0;
            for i in 0..dictionary_size {
                let string = &strings[i];
                if validity.is_valid(i) && !string.is_inlined() {
                    string_offsets[i] = current_string_offset;
                    let bytes = dictionary.string_value(string);
                    // SAFETY: the heap holds the bytes of every long dictionary string.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            bytes.as_ptr(),
                            base_heap_ptr.add(current_string_offset),
                            bytes.len(),
                        );
                    };
                    current_string_offset += bytes.len();
                }
            }
            let sel = source.sel();
            for i in 0..vector_remaining {
                let source_idx = sel.slot(offset + i);
                let target_idx = current_count + i;
                if !source.is_valid(source_idx) {
                    // SAFETY: `target_idx < STANDARD_VECTOR_SIZE`.
                    unsafe { set_invalid(mask, target_idx) };
                    continue;
                }
                let entry = strings[source_idx];
                let target = if entry.is_inlined() {
                    entry
                } else {
                    // SAFETY: the dictionary entry's bytes were copied to this place in the heap.
                    unsafe {
                        StringT::from_raw(std::slice::from_raw_parts(
                            base_heap_ptr.add(string_offsets[source_idx]),
                            entry.len(),
                        ))
                    }
                };
                // SAFETY: `target_idx < STANDARD_VECTOR_SIZE` values of the vector.
                unsafe { store(target, targets.add(target_idx * size_of::<StringT>())) };
            }
        }
    }
    *append_count = vector_remaining;
    Ok(true)
}

/// `ColumnDataCopy<string_t>` with the buffer-manager allocator.
fn copy_strings(
    ctx: &mut CopyContext<'_>,
    vector_index: usize,
    source: &Source<'_>,
    mut offset: usize,
    copy_count: usize,
) -> Result<()> {
    // Continue after the last heap of the vector.
    let mut child_index = None;
    if let Some(first) = ctx.segment.vector_data[vector_index].child_index {
        let mut index = ctx.segment.child_at(Some(first), 0)?;
        while let Some(next) = ctx.segment.vector_data[index].next_data {
            index = next;
        }
        child_index = Some(index);
    }
    let block_size = ctx.segment.allocator.block_size();
    let sel = source.sel();
    let mut current_index = vector_index;
    let mut remaining = copy_count;
    while remaining > 0 {
        let vector_remaining =
            (STANDARD_VECTOR_SIZE - ctx.segment.vector_data[current_index].count as usize).min(remaining);
        let mut append_count = 0;
        let mut heap_size = 0;
        let mut base_heap_ptr: *mut u8 = std::ptr::null_mut();
        if !copy_compressed_strings(
            ctx,
            vector_index,
            current_index,
            &mut child_index,
            source,
            offset,
            vector_remaining,
            &mut append_count,
            &mut heap_size,
            &mut base_heap_ptr,
        )? {
            // As many strings as fit in a block, at least one.
            let strings = source.strings();
            while append_count < vector_remaining {
                let slot = sel.slot(offset + append_count);
                if source.is_valid(slot) {
                    let entry = &strings[slot];
                    if !entry.is_inlined() {
                        if heap_size + entry.len() > block_size {
                            break;
                        }
                        heap_size += entry.len();
                    }
                }
                append_count += 1;
            }
            if vector_remaining != 0 && append_count == 0 {
                // A string longer than a block gets a block of its own.
                heap_size += strings[sel.slot(offset)].len();
                append_count += 1;
            }
            if heap_size != 0 {
                child_index = Some(ctx.segment.allocate_string_heap(
                    heap_size,
                    ctx.chunk_index,
                    ctx.state,
                    child_index,
                )?);
                let heap_index = child_index.ok_or_else(|| Error::internal("string heap not allocated"))?;
                if ctx.segment.vector_data[vector_index].child_index.is_none() {
                    ctx.segment.vector_data[vector_index].child_index =
                        Some(ctx.segment.add_child_index(heap_index));
                }
                let heap = &ctx.segment.vector_data[heap_index];
                base_heap_ptr = ColumnDataAllocator::data_pointer(ctx.state, heap.block_id, heap.offset)?;
            }
            let current = &ctx.segment.vector_data[current_index];
            let current_count = current.count as usize;
            let base_ptr = ColumnDataAllocator::data_pointer(ctx.state, current.block_id, current.offset)?;
            // SAFETY: the mask follows the string data in the vector's allocation.
            let mask = unsafe { base_ptr.add(data_size(size_of::<StringT>())) };
            if current_count == 0 {
                // SAFETY: the mask is `STANDARD_MASK_SIZE` bytes in the pinned block.
                unsafe { set_all_valid(mask) };
            }
            let targets = base_ptr;
            let mut heap_ptr = base_heap_ptr;
            for i in 0..append_count {
                let slot = sel.slot(offset + i);
                let target_idx = current_count + i;
                if !source.is_valid(slot) {
                    // SAFETY: `target_idx < STANDARD_VECTOR_SIZE`.
                    unsafe { set_invalid(mask, target_idx) };
                    continue;
                }
                let entry = strings[slot];
                let target = if entry.is_inlined() {
                    entry
                } else {
                    let bytes = source.vector.string_value(&entry);
                    // SAFETY: the heap was sized for the long strings of these rows.
                    unsafe {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), heap_ptr, bytes.len());
                        let string = StringT::from_raw(std::slice::from_raw_parts(heap_ptr, bytes.len()));
                        heap_ptr = heap_ptr.add(bytes.len());
                        string
                    }
                };
                // SAFETY: `target_idx < STANDARD_VECTOR_SIZE` values of the vector.
                unsafe { store(target, targets.add(target_idx * size_of::<StringT>())) };
            }
        }
        let current = &mut ctx.segment.vector_data[current_index];
        if heap_size != 0 {
            current.swizzle_data.push(SwizzleMetaData {
                child_index: child_index.ok_or_else(|| Error::internal("string heap not allocated"))?,
                ptr: base_heap_ptr as usize,
                offset: current.count,
                count: append_count as u16,
            });
        }
        current.count += append_count as u16;
        offset += append_count;
        remaining -= append_count;
        if remaining != 0 && vector_remaining - append_count == 0 {
            if ctx.segment.vector_data[current_index].next_data.is_none() {
                ctx.segment.allocate_vector(
                    source.vector.logical_type(),
                    ctx.chunk_index,
                    Some(ctx.state),
                    Some(current_index),
                )?;
            }
            current_index = ctx.segment.vector_data[current_index]
                .next_data
                .ok_or_else(|| Error::internal("ColumnDataCollection: missing continuation vector"))?;
        }
    }
    Ok(())
}

/// Where the children of a run of list rows are (`ConsecutiveChildListInfo`).
struct ConsecutiveChildListInfo {
    offset: usize,
    length: usize,
    is_constant: bool,
    needs_slicing: bool,
}

/// `ListVector::GetConsecutiveChildListInfo`.
fn consecutive_child_list_info(source: &Source<'_>, offset: usize, count: usize) -> ConsecutiveChildListInfo {
    let entries = source.vector.values::<ListEntry>();
    let sel = source.sel();
    let mut info = ConsecutiveChildListInfo {
        offset: 0,
        length: 0,
        is_constant: true,
        needs_slicing: false,
    };
    let mut first_length = 0;
    for i in offset..offset + count {
        let slot = sel.slot(i);
        if !source.is_valid(slot) {
            continue;
        }
        info.offset = entries[slot].offset as usize;
        first_length = entries[slot].length as usize;
        break;
    }
    if source.vector.kind() == VectorKind::Constant {
        info.length = first_length;
        return info;
    }
    let mut is_consecutive = true;
    for i in offset..offset + count {
        let slot = sel.slot(i);
        if !source.is_valid(slot) {
            continue;
        }
        let entry = entries[slot];
        if entry.offset as usize != info.offset || entry.length as usize != first_length {
            info.is_constant = false;
        }
        if entry.offset as usize != info.offset + info.length {
            is_consecutive = false;
        }
        info.length += entry.length as usize;
    }
    if info.is_constant {
        info.length = first_length;
    }
    if !info.is_constant && !is_consecutive {
        info.needs_slicing = true;
    }
    info
}

/// `ListVector::GetConsecutiveChildSelVector`.
fn consecutive_child_sel(source: &Source<'_>, offset: usize, count: usize, length: usize) -> SelectionVector {
    let entries = source.vector.values::<ListEntry>();
    let sel = source.sel();
    let mut indices = Vec::with_capacity(length);
    for i in offset..offset + count {
        let slot = sel.slot(i);
        if !source.is_valid(slot) {
            continue;
        }
        let entry = entries[slot];
        indices.extend((0..entry.length).map(|k| (entry.offset + k) as u32));
    }
    SelectionVector::new(indices)
}

/// `ColumnDataCopy<list_entry_t>`.
fn copy_list(
    ctx: &mut CopyContext<'_>,
    child_function: &CopyFunction,
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    copy_count: usize,
) -> Result<()> {
    let child_type = source
        .vector
        .logical_type()
        .child_type()
        .ok_or_else(|| Error::internal("LIST without a child type"))?
        .clone();
    if ctx.segment.vector_data[vector_index].child_index.is_none() {
        let child_index = ctx
            .segment
            .allocate_vector(&child_type, ctx.chunk_index, Some(ctx.state), None)?;
        ctx.segment.vector_data[vector_index].child_index = Some(ctx.segment.add_child_index(child_index));
    }
    let child_index = ctx
        .segment
        .child_at(ctx.segment.vector_data[vector_index].child_index, 0)?;

    // The current list size: the values of the child vectors so far.
    let mut current_list_size = 0;
    let mut current_child_index = Some(child_index);
    while let Some(index) = current_child_index {
        current_list_size += ctx.segment.vector_data[index].count as usize;
        current_child_index = ctx.segment.vector_data[index].next_data;
    }

    let info = consecutive_child_list_info(source, offset, copy_count);
    let (child_vector, _) = source
        .vector
        .list_child()
        .ok_or_else(|| Error::internal("LIST vector without a child"))?;
    if info.needs_slicing {
        let sel = consecutive_child_sel(source, offset, copy_count, info.length);
        let sliced = child_vector.slice(&sel).flattened(info.length);
        let child_source = Source::new(&sliced, info.length);
        copy(ctx, child_function, child_index, &child_source, 0, info.length)?;
    } else {
        let child_source = Source::new(child_vector, info.offset + info.length);
        copy(
            ctx,
            child_function,
            child_index,
            &child_source,
            info.offset,
            info.length,
        )?;
    }

    // Then the list entries, pointing into the collection's child data.
    let entries = source.vector.values::<ListEntry>();
    let mut child_list_size = current_list_size as u64;
    let is_constant = info.is_constant;
    templated_copy(
        ctx,
        vector_index,
        source,
        offset,
        copy_count,
        size_of::<ListEntry>(),
        |base, target, slot| {
            let mut entry = entries[slot];
            entry.offset = child_list_size;
            if !is_constant {
                child_list_size += entry.length;
            }
            // SAFETY: `target < STANDARD_VECTOR_SIZE` entries of the vector's data.
            unsafe { store(entry, base.add(target * size_of::<ListEntry>())) };
        },
    )
}

/// `ColumnDataCopyStruct`.
fn copy_struct(
    ctx: &mut CopyContext<'_>,
    child_functions: &[CopyFunction],
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    copy_count: usize,
) -> Result<()> {
    // The NULLs of the struct itself.
    templated_copy(ctx, vector_index, source, offset, copy_count, 0, |_, _, _| {})?;
    let children = source
        .vector
        .struct_children()
        .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
    for (child_idx, (child_function, child)) in child_functions.iter().zip(children).enumerate() {
        let child_index = ctx
            .segment
            .child_at(ctx.segment.vector_data[vector_index].child_index, child_idx)?;
        let child_source = Source::new(child, offset + copy_count);
        copy(
            ctx,
            child_function,
            child_index,
            &child_source,
            offset,
            copy_count,
        )?;
    }
    Ok(())
}

/// `ColumnDataCopyArray`.
fn copy_array(
    ctx: &mut CopyContext<'_>,
    child_function: &CopyFunction,
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    copy_count: usize,
) -> Result<()> {
    // The NULLs of the arrays themselves.
    templated_copy(ctx, vector_index, source, offset, copy_count, 0, |_, _, _| {})?;
    let ty = source.vector.logical_type();
    let (child_type, array_size) = ty
        .array_child()
        .ok_or_else(|| Error::internal("ARRAY without a child type"))?;
    let array_size = array_size as usize;
    let child_type = child_type.clone();
    let child_vector = source
        .vector
        .array_child()
        .ok_or_else(|| Error::internal("ARRAY vector without a child"))?;
    if ctx.segment.vector_data[vector_index].child_index.is_none() {
        let child_index = ctx
            .segment
            .allocate_vector(&child_type, ctx.chunk_index, Some(ctx.state), None)?;
        ctx.segment.vector_data[vector_index].child_index = Some(ctx.segment.add_child_index(child_index));
    }
    let child_index = ctx
        .segment
        .child_at(ctx.segment.vector_data[vector_index].child_index, 0)?;

    let is_constant = source.vector.kind() == VectorKind::Constant;
    let child_count = if is_constant {
        array_size
    } else {
        (offset + copy_count) * array_size
    };
    let mut child_source = Source::new(child_vector, child_count);
    // The NULL arrays' elements are NULL too (a copy: the input is not modified).
    if !source.validity.is_all_valid() {
        let capacity = child_vector.capacity().max(child_count);
        let validity = child_source.validity.to_mut();
        let sel = source.sel();
        for i in 0..copy_count {
            let slot = sel.slot(offset + i);
            if !source.is_valid(slot) {
                for j in 0..array_size {
                    validity.set_invalid(slot * array_size + j, capacity);
                }
            }
        }
    }
    if is_constant {
        for _ in 0..copy_count {
            copy(ctx, child_function, child_index, &child_source, 0, array_size)?;
        }
        Ok(())
    } else {
        copy(
            ctx,
            child_function,
            child_index,
            &child_source,
            offset * array_size,
            copy_count * array_size,
        )
    }
}

/// Runs `function` (`ColumnDataCopyFunction::function`).
fn copy(
    ctx: &mut CopyContext<'_>,
    function: &CopyFunction,
    vector_index: usize,
    source: &Source<'_>,
    offset: usize,
    count: usize,
) -> Result<()> {
    if count == 0 {
        return Ok(());
    }
    match function {
        CopyFunction::Fixed(width) => copy_fixed(ctx, vector_index, source, offset, count, *width),
        CopyFunction::String => copy_strings(ctx, vector_index, source, offset, count),
        CopyFunction::List(child) => copy_list(ctx, child, vector_index, source, offset, count),
        CopyFunction::Struct(children) => copy_struct(ctx, children, vector_index, source, offset, count),
        CopyFunction::Array(child) => copy_array(ctx, child, vector_index, source, offset, count),
    }
}

/// The state of an append (`ColumnDataAppendState`): the pins of the chunk appended to.
#[derive(Debug, Default)]
pub struct ColumnDataAppendState {
    current_chunk_state: ChunkManagementState,
    /// Whether `InitializeAppend` pinned the chunk appended to.
    initialized: bool,
}

/// Where a scan is and the pins of its chunk (`ColumnDataScanState`).
#[derive(Debug, Default)]
pub struct ColumnDataScanState {
    chunk_index: usize,
    segment_index: usize,
    current_row_index: usize,
    next_row_index: usize,
    current_chunk_state: ChunkManagementState,
    column_ids: Vec<usize>,
}

/// Chunks of fixed column types in buffer-managed blocks (`ColumnDataCollection`); see
/// the module documentation.
#[derive(Debug)]
pub struct ColumnDataCollection {
    allocator: Arc<ColumnDataAllocator>,
    types: Vec<LogicalType>,
    copy_functions: Vec<CopyFunction>,
    count: usize,
    segments: Vec<ColumnDataCollectionSegment>,
    finished_append: bool,
}

impl ColumnDataCollection {
    /// Whether columns of `types` can be stored (every type has a copy function).
    pub fn supports(types: &[LogicalType]) -> bool {
        types.iter().all(|ty| CopyFunction::for_type(ty).is_ok())
    }

    /// An empty collection of `types` allocating blocks of the `tag` from
    /// `buffer_manager`.
    pub fn new(buffer_manager: BufferManager, types: Vec<LogicalType>, tag: MemoryTag) -> Result<Self> {
        Self::with_allocator(Arc::new(ColumnDataAllocator::new(buffer_manager, tag)), types)
    }

    /// An empty collection allocating from `allocator`, which other collections may share.
    fn with_allocator(allocator: Arc<ColumnDataAllocator>, types: Vec<LogicalType>) -> Result<Self> {
        let copy_functions = types.iter().map(CopyFunction::for_type).collect::<Result<_>>()?;
        Ok(Self {
            allocator,
            types,
            copy_functions,
            count: 0,
            segments: Vec::new(),
            finished_append: false,
        })
    }

    /// A new, empty collection sharing this collection's blocks, which takes no more
    /// appends (`ColumnDataCollection(ColumnDataCollection &other)`).
    #[must_use]
    pub fn share_allocator(&mut self) -> Self {
        self.finished_append = true;
        Self {
            allocator: Arc::clone(&self.allocator),
            types: self.types.clone(),
            copy_functions: self.copy_functions.clone(),
            count: 0,
            segments: Vec::new(),
            finished_append: false,
        }
    }

    pub fn types(&self) -> &[LogicalType] {
        &self.types
    }

    /// The eviction priority of future blocks (`SetPartitionIndex`), set before
    /// appending or sharing the allocator.
    pub fn set_partition_index(&mut self, index: usize) -> Result<()> {
        Arc::get_mut(&mut self.allocator)
            .ok_or_else(|| Error::internal("ColumnDataCollection allocator is already shared"))?
            .set_partition_index(index)
    }

    pub fn column_count(&self) -> usize {
        self.types.len()
    }

    /// The number of rows (`Count`).
    pub fn count(&self) -> usize {
        self.count
    }

    /// `ChunkCount`.
    pub fn chunk_count(&self) -> usize {
        self.segments
            .iter()
            .map(ColumnDataCollectionSegment::chunk_count)
            .sum()
    }

    /// The bytes of the blocks in use (`SizeInBytes`).
    pub fn size_in_bytes(&self) -> usize {
        self.allocator.size_in_bytes()
    }

    /// The bytes allocated for the blocks (`AllocationSize`).
    pub fn allocation_size(&self) -> usize {
        self.allocator.allocation_size()
    }

    fn create_segment(&mut self) {
        self.segments.push(ColumnDataCollectionSegment::new(
            Arc::clone(&self.allocator),
            self.types.clone(),
        ));
    }

    /// Prepares appending: the first chunk is allocated and its blocks pinned
    /// (`InitializeAppend`).
    pub fn initialize_append(&mut self, state: &mut ColumnDataAppendState) -> Result<()> {
        if self.finished_append {
            return Err(Error::internal(
                "ColumnDataCollection: append after the appends finished",
            ));
        }
        state.current_chunk_state.clear();
        state.initialized = true;
        if self.segments.is_empty() {
            self.create_segment();
        }
        let Some(segment) = self.segments.last_mut() else {
            unreachable!("a segment was just ensured")
        };
        if segment.chunk_data.is_empty() {
            segment.allocate_new_chunk()?;
        }
        segment.initialize_chunk_state(segment.chunk_data.len() - 1, &mut state.current_chunk_state)
    }

    /// Appends the rows of `input` (`Append`), initializing the append first if `state`
    /// is new (`Append(DataChunk &)`).
    pub fn append(&mut self, state: &mut ColumnDataAppendState, input: &DataChunk) -> Result<()> {
        let size = input.len();
        if size == 0 {
            return Ok(());
        }
        if input.column_count() != self.types.len() {
            return Err(Error::internal("chunk does not fit the column data collection"));
        }
        if !state.initialized {
            self.initialize_append(state)?;
        }
        let sources: Vec<Source<'_>> = input
            .columns()
            .iter()
            .map(|vector| Source::new(vector, size))
            .collect();
        let Some(segment) = self.segments.last_mut() else {
            return Err(Error::internal(
                "ColumnDataCollection: append before InitializeAppend",
            ));
        };
        let mut remaining = size;
        while remaining > 0 {
            let chunk_index = segment.chunk_data.len() - 1;
            let chunk_count = segment.chunk_data[chunk_index].count as usize;
            let append_amount = remaining.min(STANDARD_VECTOR_SIZE - chunk_count);
            if append_amount > 0 {
                let offset = size - remaining;
                let vector_data = segment.chunk_data[chunk_index].vector_data.clone();
                let mut ctx = CopyContext {
                    segment,
                    state: &mut state.current_chunk_state,
                    chunk_index,
                };
                for ((function, source), vector_index) in
                    self.copy_functions.iter().zip(&sources).zip(vector_data)
                {
                    copy(&mut ctx, function, vector_index, source, offset, append_amount)?;
                }
                segment.chunk_data[chunk_index].count += append_amount as u16;
            }
            remaining -= append_amount;
            if remaining > 0 {
                segment.allocate_new_chunk()?;
                segment
                    .initialize_chunk_state(segment.chunk_data.len() - 1, &mut state.current_chunk_state)?;
            }
        }
        segment.count += size;
        self.count += size;
        Ok(())
    }

    /// Releases the pins of an append; a further append initializes it again.
    pub fn finalize_append(&self, state: &mut ColumnDataAppendState) {
        state.current_chunk_state.clear();
        state.initialized = false;
    }

    /// Moves the chunks of `other` behind this collection's (`Combine`).
    pub fn combine(&mut self, mut other: Self) -> Result<()> {
        if other.count == 0 {
            return Ok(());
        }
        if self.types != other.types {
            return Err(Error::internal(
                "Attempting to combine ColumnDataCollections with mismatching types",
            ));
        }
        self.count += other.count;
        self.segments.append(&mut other.segments);
        Ok(())
    }

    /// A scan over every column (`InitializeScan`).
    pub fn initialize_scan(&self) -> ColumnDataScanState {
        ColumnDataScanState {
            column_ids: (0..self.types.len()).collect(),
            ..ColumnDataScanState::default()
        }
    }

    /// A replayable scan copying selected columns, with one chunk's blocks pinned.
    pub fn initialize_scan_columns(&self, columns: &[usize]) -> Result<ColumnDataScanState> {
        if columns.iter().any(|&index| index >= self.types.len()) {
            return Err(Error::internal("ColumnDataCollection scan column out of range"));
        }
        Ok(ColumnDataScanState {
            column_ids: columns.to_vec(),
            ..ColumnDataScanState::default()
        })
    }

    /// The position of the next chunk of the scan (`NextScanIndex`).
    fn next_scan_index(&self, state: &mut ColumnDataScanState) -> Option<(usize, usize)> {
        state.current_row_index = state.next_row_index;
        if state.segment_index >= self.segments.len() {
            return None;
        }
        while state.chunk_index >= self.segments[state.segment_index].chunk_data.len() {
            state.chunk_index = 0;
            state.segment_index += 1;
            state.current_chunk_state.clear();
            if state.segment_index >= self.segments.len() {
                return None;
            }
        }
        state.next_row_index +=
            self.segments[state.segment_index].chunk_data[state.chunk_index].count as usize;
        let chunk_index = state.chunk_index;
        state.chunk_index += 1;
        Some((state.segment_index, chunk_index))
    }

    /// The next chunk of the scan, `None` at the end (`Scan`).
    pub fn scan(&self, state: &mut ColumnDataScanState) -> Result<Option<DataChunk>> {
        let Some((segment_index, chunk_index)) = self.next_scan_index(state) else {
            return Ok(None);
        };
        let column_ids = std::mem::take(&mut state.column_ids);
        let chunk =
            self.segments[segment_index].read_chunk(chunk_index, &mut state.current_chunk_state, &column_ids);
        state.column_ids = column_ids;
        chunk.map(Some)
    }

    /// Reads columns `column_ids` of chunk `chunk_index` into `result`, pinning its blocks
    /// in `state` and releasing those of the chunk read before (`FetchChunk` / `ReadChunk`).
    pub fn fetch_chunk(
        &self,
        state: &mut ColumnDataScanState,
        chunk_index: usize,
        column_ids: &[usize],
        result: &mut DataChunk,
    ) -> Result<()> {
        let mut chunk_idx = chunk_index;
        for (segment_index, segment) in self.segments.iter().enumerate() {
            if chunk_idx >= segment.chunk_count() {
                chunk_idx -= segment.chunk_count();
                continue;
            }
            if segment_index != state.segment_index {
                // Block ids are per allocator: the pins of another segment's do not apply.
                state.current_chunk_state.clear();
                state.segment_index = segment_index;
            }
            *result = segment.read_chunk(chunk_idx, &mut state.current_chunk_state, column_ids)?;
            return Ok(());
        }
        Err(Error::internal("Failed to find chunk in ColumnDataCollection"))
    }

    /// The number of rows of every chunk, in order.
    pub fn chunk_row_counts(&self) -> impl Iterator<Item = usize> + '_ {
        self.segments
            .iter()
            .flat_map(|segment| segment.chunk_data.iter().map(|chunk| chunk.count as usize))
    }
}

#[cfg(test)]
mod tests {
    use roastedduck_buffer::{BufferManagerConfig, MemoryTag};
    use roastedduck_types::Value;

    use super::*;

    fn buffer_manager() -> BufferManager {
        BufferManager::new(BufferManagerConfig::default()).unwrap()
    }

    fn values(collection: &ColumnDataCollection) -> Vec<Vec<Value>> {
        let mut rows = Vec::new();
        let mut state = collection.initialize_scan();
        while let Some(chunk) = collection.scan(&mut state).unwrap() {
            for row in 0..chunk.len() {
                rows.push(
                    (0..chunk.column_count())
                        .map(|c| chunk.column(c).get_value(row))
                        .collect(),
                );
            }
        }
        rows
    }

    fn chunk_of(types: &[LogicalType], rows: &[Vec<Value>]) -> DataChunk {
        let mut columns: Vec<Vector> = types
            .iter()
            .map(|ty| Vector::flat(ty.clone(), rows.len().max(1)))
            .collect();
        for (r, row) in rows.iter().enumerate() {
            for (c, value) in row.iter().enumerate() {
                columns[c].set_value(r, value).unwrap();
            }
        }
        DataChunk::new(columns, rows.len())
    }

    #[test]
    fn integers_round_trip_and_fill_blocks_like_duckdb() {
        let manager = buffer_manager();
        let mut collection =
            ColumnDataCollection::new(manager.clone(), vec![LogicalType::BIGINT], MemoryTag::ColumnData)
                .unwrap();
        let mut append = ColumnDataAppendState::default();
        collection.initialize_append(&mut append).unwrap();
        for start in (0..100_000).step_by(STANDARD_VECTOR_SIZE) {
            let count = (100_000 - start).min(STANDARD_VECTOR_SIZE);
            let mut vector = Vector::flat(LogicalType::BIGINT, count);
            for (i, value) in vector.values_mut::<i64>()[..count].iter_mut().enumerate() {
                *value = (start + i) as i64;
            }
            collection
                .append(&mut append, &DataChunk::new(vec![vector], count))
                .unwrap();
        }
        drop(append);
        assert_eq!(collection.count(), 100_000);
        // 49 vectors of 16,640 bytes, 15 to a block: 4 blocks, as in DuckDB.
        assert_eq!(collection.allocator.block_count(), 4);
        let usage = manager
            .memory_usage_info()
            .into_iter()
            .find(|info| info.tag == MemoryTag::ColumnData)
            .unwrap();
        assert_eq!(usage.size, 4 * 262_144);
        let rows = values(&collection);
        assert_eq!(rows.len(), 100_000);
        assert_eq!(rows[99_999][0], Value::bigint(99_999));
        drop(collection);
        assert_eq!(manager.used_memory(), 0);
    }

    #[test]
    fn strings_nulls_lists_and_structs_round_trip() {
        let list = LogicalType::list(LogicalType::VARCHAR);
        let st = LogicalType::struct_type(vec![
            ("a".into(), LogicalType::INTEGER),
            ("b".into(), LogicalType::VARCHAR),
        ]);
        let array = LogicalType::array(LogicalType::INTEGER, 2);
        let types = vec![LogicalType::VARCHAR, list.clone(), st.clone(), array.clone()];
        let long = "a string that is longer than twelve bytes";
        let rows: Vec<Vec<Value>> = (0..5000)
            .map(|i| {
                vec![
                    if i % 7 == 0 {
                        Value::null(LogicalType::VARCHAR)
                    } else {
                        Value::varchar(format!("{long} {i}"))
                    },
                    Value::list(
                        LogicalType::VARCHAR,
                        (0..i % 4).map(|j| Value::varchar(format!("{long}{j}"))).collect(),
                    ),
                    if i % 5 == 0 {
                        Value::null(st.clone())
                    } else {
                        Value::struct_value(vec![
                            ("a".into(), Value::integer(i)),
                            ("b".into(), Value::varchar(format!("s{i}"))),
                        ])
                    },
                    Value::array(LogicalType::INTEGER, vec![Value::integer(i), Value::integer(-i)]),
                ]
            })
            .collect();
        let mut collection =
            ColumnDataCollection::new(buffer_manager(), types.clone(), MemoryTag::ColumnData).unwrap();
        let mut append = ColumnDataAppendState::default();
        collection.initialize_append(&mut append).unwrap();
        for batch in rows.chunks(1000) {
            collection.append(&mut append, &chunk_of(&types, batch)).unwrap();
        }
        drop(append);
        assert_eq!(collection.count(), 5000);
        assert_eq!(collection.chunk_count(), 3);
        assert_eq!(values(&collection), rows);
        let mut second = DataChunk::default();
        collection
            .fetch_chunk(&mut ColumnDataScanState::default(), 1, &[0, 3], &mut second)
            .unwrap();
        assert_eq!(second.column(0).get_value(0), rows[STANDARD_VECTOR_SIZE][0]);
        assert_eq!(second.column(1).get_value(0), rows[STANDARD_VECTOR_SIZE][3]);
    }

    #[test]
    fn constant_and_dictionary_strings_and_shared_blocks() {
        let manager = buffer_manager();
        let mut first =
            ColumnDataCollection::new(manager.clone(), vec![LogicalType::VARCHAR], MemoryTag::ColumnData)
                .unwrap();
        let long = Value::varchar("a constant that does not fit inline");
        let constant = Vector::constant(&long).unwrap();
        let mut append = ColumnDataAppendState::default();
        first
            .append(&mut append, &DataChunk::new(vec![constant.clone()], 3000))
            .unwrap();
        // A collection sharing the blocks, as a batch collector starts one per batch.
        let mut second = first.share_allocator();
        let mut append = ColumnDataAppendState::default();
        second
            .append(&mut append, &DataChunk::new(vec![constant], 10))
            .unwrap();
        second.finalize_append(&mut append);
        first.combine(second).unwrap();
        assert_eq!(first.count(), 3010);
        assert_eq!(first.allocator.block_count(), 1);
        let rows = values(&first);
        assert_eq!(rows.len(), 3010);
        assert!(rows.iter().all(|row| row[0] == long));
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
