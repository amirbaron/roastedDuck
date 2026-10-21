//! Arrow arrays to DuckDB vectors (DuckDB's `function/table/arrow_conversion.cpp` and
//! `arrow/arrow_array_scan_state.cpp`).
//!
//! DuckDB points flat vectors at the Arrow buffers where the layouts agree. rustdb's
//! vectors own their buffers, so fixed-width data is copied; strings still point into the
//! Arrow data, which the vectors keep alive. Everything else follows DuckDB, including
//! how offsets combine: `chunk_offset` is how far the scan is into the Arrow chunk,
//! `parent_offset` the offset of an enclosing struct, and `nested_offset` (when not -1)
//! the first element of a list's children, which replaces both.
//!
//! The functions read Arrow arrays through raw pointers; their callers guarantee that
//! the arrays follow the C data interface and have the type they are read as.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{
    DataChunk, Interval, ListEntry, LogicalType, LogicalTypeId, NativeType, PhysicalType, SelectionVector,
    StringT, ValidityMask, Value, Vector,
};

use crate::duck_schema::{
    ArrowArrayPhysicalType, ArrowDateTimeType, ArrowTableSchema, ArrowType, ArrowVariableSizeType,
    DecimalBitWidth,
};
use crate::ffi::{ArrowArray, ArrowArrayWrapper};

/// The run ends and values of a run-end encoded array, converted once per Arrow chunk
/// (DuckDB's `ArrowRunEndEncodingState`).
#[derive(Debug, Default)]
struct ArrowRunEndEncodingState {
    run_ends: Option<Vector>,
    values: Option<Vector>,
}

/// The scan state of one Arrow column (DuckDB's `ArrowArrayScanState`).
#[derive(Debug, Default)]
pub(crate) struct ArrowArrayScanState {
    /// The Arrow chunk being converted, which vectors pointing into it keep alive.
    pub(crate) owned_data: Option<Arc<ArrowArrayWrapper>>,
    children: HashMap<usize, ArrowArrayScanState>,
    /// The address of the Arrow dictionary `dictionary` was made from.
    arrow_dictionary: usize,
    /// The converted dictionary, the same for every chunk of the column.
    dictionary: Option<Arc<Vector>>,
    run_end_encoding: ArrowRunEndEncodingState,
}

impl ArrowArrayScanState {
    /// The state of child `child_index`, holding the same Arrow chunk (`GetChild`).
    fn child(&mut self, child_index: usize) -> &mut Self {
        let owned_data = self.owned_data.clone();
        let child = self.children.entry(child_index).or_default();
        if child.owned_data.is_none() {
            child.owned_data = owned_data;
        }
        child
    }

    /// Forgets the converted Arrow chunk; the dictionary is kept, as it is the same for
    /// every chunk of a column (`Reset`).
    pub(crate) fn reset(&mut self) {
        self.run_end_encoding = ArrowRunEndEncodingState::default();
        for child in self.children.values_mut() {
            child.reset();
        }
        self.owned_data = None;
    }

    /// Whether the cached dictionary was not made from `dictionary` (`CacheOutdated`).
    fn cache_outdated(&self, dictionary: *const ArrowArray) -> bool {
        dictionary.is_null() || dictionary as usize != self.arrow_dictionary || self.dictionary.is_none()
    }

    /// Keeps the vector made from the Arrow dictionary `arrow_dictionary`
    /// (`AddDictionary`).
    fn add_dictionary(&mut self, dictionary: Vector, arrow_dictionary: *const ArrowArray) {
        self.dictionary = Some(Arc::new(dictionary));
        self.arrow_dictionary = arrow_dictionary as usize;
    }

    /// Keeps the Arrow chunk alive as long as `vector`, whose strings point into it.
    fn keep_alive(&self, vector: &mut Vector) {
        if let Some(owned_data) = &self.owned_data {
            vector.add_keep_alive(Arc::clone(owned_data) as Arc<dyn std::any::Any + Send + Sync>);
        }
    }
}

/// Element `index` of the values at `data`, which need not be aligned.
///
/// # Safety
/// The element must be readable.
unsafe fn read<T: Copy>(data: *const T, index: usize) -> T {
    // SAFETY: guaranteed by the caller.
    unsafe { data.add(index).read_unaligned() }
}

/// Buffer `index` of `array`.
///
/// # Safety
/// `array` must follow the C data interface and have more than `index` buffers.
unsafe fn buffer<T>(array: &ArrowArray, index: usize) -> *const T {
    // SAFETY: guaranteed by the caller.
    unsafe { (*array.buffers.add(index)).cast() }
}

/// Child `index` of `array`.
///
/// # Safety
/// `array` must follow the C data interface and have more than `index` children.
unsafe fn child_array(array: &ArrowArray, index: usize) -> &ArrowArray {
    // SAFETY: guaranteed by the caller.
    unsafe { &**array.children.add(index) }
}

/// The position in the Arrow array of the first row to convert (`GetEffectiveOffset`):
/// below a list, its element offset; otherwise the array's own, the enclosing struct's
/// and the scan's offsets.
fn effective_offset(
    array: &ArrowArray,
    parent_offset: i64,
    chunk_offset: usize,
    nested_offset: i64,
) -> usize {
    if nested_offset != -1 {
        // The parent is a list, whose offset was already applied.
        return (array.offset + nested_offset) as usize;
    }
    (array.offset + parent_offset) as usize + chunk_offset
}

/// The validity of `size` rows of an Arrow array, or `None` when it has no NULLs, is of
/// the `null` type or has no validity buffer (`GetValidityMask`).
///
/// # Safety
/// `array` must follow the C data interface.
unsafe fn arrow_validity(
    array: &ArrowArray,
    chunk_offset: usize,
    size: usize,
    parent_offset: i64,
    nested_offset: i64,
) -> Option<ValidityMask> {
    // SAFETY: an array with buffers has its validity buffer first.
    if array.null_count == 0 || array.n_buffers == 0 || unsafe { buffer::<u8>(array, 0) }.is_null() {
        return None;
    }
    let bit_offset = effective_offset(array, parent_offset, chunk_offset, nested_offset);
    // SAFETY: checked non-null above.
    let bitmap = unsafe { buffer::<u8>(array, 0) };
    let mut words = vec![u64::MAX; size.div_ceil(64)];
    for row in 0..size {
        let bit = bit_offset + row;
        // SAFETY: the bitmap covers the array's rows.
        if unsafe { read(bitmap, bit / 8) } >> (bit % 8) & 1 == 0 {
            words[row / 64] &= !(1 << (row % 64));
        }
    }
    Some(ValidityMask::from_words(words))
}

/// Copies the validity of `size` rows of `array` into `vector`; with `add_null` row
/// `size` (a dictionary's NULL entry) becomes NULL too (`SetValidityMask`).
///
/// # Safety
/// `array` must follow the C data interface.
unsafe fn set_validity_mask(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    size: usize,
    parent_offset: i64,
    nested_offset: i64,
    add_null: bool,
) {
    // SAFETY: guaranteed by the caller.
    if let Some(mask) = unsafe { arrow_validity(array, chunk_offset, size, parent_offset, nested_offset) } {
        vector.set_validity(mask);
    }
    if add_null {
        vector.validity_mut().set_invalid(size, size + 1);
    }
}

/// Marks the rows that are NULL in `parent_mask` NULL in `vector` too (a nested value
/// is NULL where its parent is).
fn apply_parent_mask(vector: &mut Vector, parent_mask: Option<&ValidityMask>, size: usize) {
    let Some(parent_mask) = parent_mask.filter(|mask| !mask.is_all_valid()) else {
        return;
    };
    for row in 0..size {
        if !parent_mask.is_valid(row) {
            vector.validity_mut().set_invalid(row, size);
        }
    }
}

/// The child rows of a list chunk: how many there are and where they start.
#[derive(Debug, Default)]
struct ArrowListOffsetData {
    list_size: usize,
    start_offset: usize,
}

/// The list entries of `size` rows from Arrow list offsets of type `O`
/// (`ConvertArrowListOffsetsTemplated`).
///
/// # Safety
/// `array` must be a list array of the C data interface with offsets of type `O`.
unsafe fn convert_list_offsets<O: Copy + Into<u64>>(
    vector: &mut Vector,
    array: &ArrowArray,
    size: usize,
    effective_offset: usize,
) -> ArrowListOffsetData {
    if size == 0 {
        return ArrowListOffsetData::default();
    }
    // SAFETY: a list's second buffer holds `length + 1` offsets.
    let offsets = unsafe { buffer::<O>(array, 1).add(effective_offset) };
    // SAFETY: as above, for every read below.
    let offset_at = |index: usize| -> usize { unsafe { read(offsets, index) }.into() as usize };
    let start_offset = offset_at(0);
    let entries = vector.values_mut::<ListEntry>();
    let mut current_offset = 0;
    for (row, entry) in entries.iter_mut().enumerate().take(size) {
        let length = offset_at(row + 1).wrapping_sub(offset_at(row));
        *entry = ListEntry::new(current_offset, length);
        current_offset += length;
    }
    ArrowListOffsetData {
        list_size: offset_at(size) - start_offset,
        start_offset,
    }
}

/// The list entries of `size` rows of an Arrow list view, whose elements need not be in
/// order; the entries are relative to the lowest offset (`ConvertArrowListViewOffsetsTemplated`).
///
/// # Safety
/// `array` must be a list view array of the C data interface with offsets of type `O`.
unsafe fn convert_list_view_offsets<O: Copy + Into<u64>>(
    vector: &mut Vector,
    array: &ArrowArray,
    size: usize,
    effective_offset: usize,
) -> ArrowListOffsetData {
    // SAFETY: a list view's second and third buffers hold an offset and a size per row.
    let (offsets, sizes) = unsafe {
        (
            buffer::<O>(array, 1).add(effective_offset),
            buffer::<O>(array, 2).add(effective_offset),
        )
    };
    // SAFETY: as above, for every read below.
    let offset_at = |index: usize| -> usize { unsafe { read(offsets, index) }.into() as usize };
    // SAFETY: as above.
    let size_at = |index: usize| -> usize { unsafe { read(sizes, index) }.into() as usize };
    let mut list_size = 0;
    let mut lowest_offset = if size > 0 { offset_at(0) } else { 0 };
    let entries = vector.values_mut::<ListEntry>();
    for (row, entry) in entries.iter_mut().enumerate().take(size) {
        *entry = ListEntry::new(offset_at(row), size_at(row));
        list_size += size_at(row);
        if size_at(row) != 0 {
            lowest_offset = lowest_offset.min(offset_at(row));
        }
    }
    // The child data is scanned from the lowest offset on.
    if lowest_offset != 0 {
        for entry in entries.iter_mut().take(size) {
            let offset = entry.offset as usize;
            entry.offset = if offset <= lowest_offset {
                0
            } else {
                (offset - lowest_offset) as u64
            };
        }
    }
    ArrowListOffsetData {
        list_size,
        start_offset: lowest_offset,
    }
}

/// Converts a list (or map) array (`ArrowToDuckDBList`).
///
/// # Safety
/// `array` must be a list, list view or map array of the C data interface of
/// `arrow_type`.
#[allow(clippy::too_many_arguments, reason = "DuckDB's conversion signature")]
unsafe fn arrow_to_duckdb_list(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    nested_offset: i64,
    parent_mask: Option<&ValidityMask>,
    parent_offset: i64,
) -> Result<()> {
    let (child_type, size_type, is_view) = arrow_type.list_info()?;
    // SAFETY: guaranteed by the caller, for every access to `array` below.
    unsafe {
        set_validity_mask(
            vector,
            array,
            chunk_offset,
            size,
            parent_offset,
            nested_offset,
            false,
        );
    }
    let offset = effective_offset(array, parent_offset, chunk_offset, nested_offset);
    // SAFETY: as above; the size type and layout are the array's.
    let list_data = unsafe {
        match (is_view, size_type) {
            (true, ArrowVariableSizeType::SuperSize) => {
                convert_list_view_offsets::<u64>(vector, array, size, offset)
            }
            (true, _) => convert_list_view_offsets::<u32>(vector, array, size, offset),
            (false, ArrowVariableSizeType::SuperSize) => {
                convert_list_offsets::<u64>(vector, array, size, offset)
            }
            (false, _) => convert_list_offsets::<u32>(vector, array, size, offset),
        }
    };
    let ArrowListOffsetData {
        list_size,
        start_offset,
    } = list_data;
    apply_parent_mask(vector, parent_mask, size);
    // SAFETY: a list has one child.
    let child = unsafe { child_array(array, 0) };
    let child_state = array_state.child(0);
    let child_vector = vector
        .list_child_mut()
        .ok_or_else(|| Error::internal("LIST vector without a child vector"))?;
    child_vector.resize(list_size);
    // SAFETY: the child follows the C data interface.
    unsafe {
        set_validity_mask(
            child_vector,
            child,
            chunk_offset,
            list_size,
            array.offset,
            start_offset as i64,
            false,
        );
    }
    // Without child rows there is nothing to convert.
    if list_size != 0 || start_offset != 0 {
        // SAFETY: as above.
        unsafe {
            convert_nested_child(
                child_vector,
                child,
                chunk_offset,
                child_state,
                list_size,
                child_type,
                start_offset as i64,
                None,
                0,
            )
        }?;
    }
    vector.set_list_size(list_size);
    Ok(())
}

/// Converts the child of a nested array by its physical layout; dictionary results are
/// flattened, as nested vectors hold flat children.
///
/// # Safety
/// `array` must follow the C data interface and be of `arrow_type`.
#[allow(clippy::too_many_arguments, reason = "DuckDB's conversion signature")]
unsafe fn convert_nested_child(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    nested_offset: i64,
    parent_mask: Option<&ValidityMask>,
    parent_offset: u64,
) -> Result<()> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match arrow_type.physical_type() {
            ArrowArrayPhysicalType::DictionaryEncoded => {
                column_arrow_to_duckdb_dictionary(
                    vector,
                    array,
                    chunk_offset,
                    array_state,
                    size,
                    arrow_type,
                    nested_offset,
                    parent_mask,
                    parent_offset,
                )?;
                vector.flatten(size);
                Ok(())
            }
            ArrowArrayPhysicalType::RunEndEncoded => column_arrow_to_duckdb_run_end_encoded(
                vector,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                nested_offset,
                parent_offset,
            ),
            ArrowArrayPhysicalType::Default => column_arrow_to_duckdb(
                vector,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                nested_offset,
                parent_mask,
                parent_offset,
                false,
            ),
        }
    }
}

/// Converts a fixed-size list array (`ArrowToDuckDBArray`).
///
/// # Safety
/// `array` must be a fixed-size list array of the C data interface of `arrow_type`.
#[allow(clippy::too_many_arguments, reason = "DuckDB's conversion signature")]
unsafe fn arrow_to_duckdb_array(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    nested_offset: i64,
    parent_mask: Option<&ValidityMask>,
    parent_offset: i64,
) -> Result<()> {
    let (child_type, array_size) = arrow_type.array_info()?;
    let child_count = array_size * size;
    let child_offset = effective_offset(array, parent_offset, chunk_offset, nested_offset) * array_size;
    // SAFETY: guaranteed by the caller, for every access to `array` below.
    unsafe {
        set_validity_mask(
            vector,
            array,
            chunk_offset,
            size,
            parent_offset,
            nested_offset,
            false,
        );
    }
    apply_parent_mask(vector, parent_mask, size);
    let array_mask = vector.validity().clone();
    // SAFETY: a fixed-size list has one child.
    let child = unsafe { child_array(array, 0) };
    let child_state = array_state.child(0);
    let child_vector = vector
        .array_child_mut()
        .ok_or_else(|| Error::internal("ARRAY vector without a child vector"))?;
    // SAFETY: the child follows the C data interface.
    unsafe {
        set_validity_mask(
            child_vector,
            child,
            chunk_offset,
            child_count,
            array.offset,
            child_offset as i64,
            false,
        );
    }
    // The elements of a NULL array are NULL.
    if !array_mask.is_all_valid() {
        for row in (0..size).filter(|&row| !array_mask.is_valid(row)) {
            for element in row * array_size..(row + 1) * array_size {
                child_vector.validity_mut().set_invalid(element, child_count);
            }
        }
    }
    // SAFETY: as above.
    unsafe {
        if child_count == 0 && child_offset == 0 {
            // Without child rows there is nothing to convert.
            Ok(())
        } else if child.dictionary.is_null() {
            column_arrow_to_duckdb(
                child_vector,
                child,
                chunk_offset,
                child_state,
                child_count,
                child_type,
                child_offset as i64,
                None,
                0,
                false,
            )
        } else {
            column_arrow_to_duckdb_dictionary(
                child_vector,
                child,
                chunk_offset,
                child_state,
                child_count,
                child_type,
                child_offset as i64,
                None,
                0,
            )?;
            child_vector.flatten(child_count);
            Ok(())
        }
    }
}

/// Rejects maps with NULL or duplicate keys, which DuckDB maps cannot hold
/// (`ArrowToDuckDBMapVerify`).
fn verify_map(vector: &Vector, count: usize) -> Result<()> {
    let Some((entries, _)) = vector.list_child() else {
        return Ok(());
    };
    let Some(keys) = entries.struct_children().and_then(|fields| fields.first()) else {
        return Ok(());
    };
    for row in (0..count).filter(|&row| vector.validity().is_valid(row)) {
        let entry = vector.values::<ListEntry>()[row];
        let mut seen = HashSet::new();
        for index in entry.range() {
            let key = keys.get_value(index);
            if key.is_null() {
                return Err(Error::invalid_input(
                    "Arrow map contains NULL as map key, which isn't supported by DuckDB map type",
                ));
            }
            if !seen.insert(key) {
                return Err(Error::invalid_input(
                    "Arrow map contains duplicate key, which isn't supported by DuckDB map type",
                ));
            }
        }
    }
    Ok(())
}

/// Sets the strings of `size` rows from Arrow offsets of type `O` into `data`: the
/// strings point into the Arrow buffer (`SetVectorString`).
///
/// # Safety
/// `offsets` must hold `size + 1` offsets into `data`.
unsafe fn set_vector_string<O: Copy + Into<u64>>(
    vector: &mut Vector,
    size: usize,
    data: *const u8,
    offsets: *const O,
) -> Result<()> {
    let validity = vector.validity().clone();
    let strings = vector.values_mut::<StringT>();
    for (row, string) in strings.iter_mut().enumerate().take(size) {
        if !validity.is_valid(row) {
            continue;
        }
        // SAFETY: guaranteed by the caller.
        let (start, end) = unsafe { (read(offsets, row).into(), read(offsets, row + 1).into()) };
        let length = end.wrapping_sub(start);
        if length > u64::from(u32::MAX) {
            return Err(Error::conversion("DuckDB does not support Strings over 4GB"));
        }
        // SAFETY: the offsets delimit bytes of the data buffer; the vector keeps the
        // Arrow data alive (`keep_alive`).
        *string = unsafe {
            StringT::from_raw(std::slice::from_raw_parts(
                data.add(start as usize),
                length as usize,
            ))
        };
    }
    Ok(())
}

/// Sets the strings of `size` binary or string views starting at view `position`
/// (`SetVectorStringView`).
///
/// # Safety
/// `array` must be a view array of the C data interface.
unsafe fn set_vector_string_view(vector: &mut Vector, size: usize, array: &ArrowArray, position: usize) {
    const MAX_INLINED_BYTES: usize = 12;
    // SAFETY: a view array's second buffer holds 16-byte views.
    let views = unsafe { buffer::<[u8; 16]>(array, 1).add(position) };
    let validity = vector.validity().clone();
    let strings = vector.values_mut::<StringT>();
    for (row, string) in strings.iter_mut().enumerate().take(size) {
        if !validity.is_valid(row) {
            continue;
        }
        // SAFETY: as above.
        let view = unsafe { read(views, row) };
        let field = |at: usize| i32::from_le_bytes([view[at], view[at + 1], view[at + 2], view[at + 3]]);
        let length = field(0) as usize;
        *string = if length <= MAX_INLINED_BYTES {
            // | length | data (zero-padded) |
            StringT::inline(&view[4..4 + length])
        } else {
            // | length | prefix | buffer index | offset |
            let buffer_index = field(8) as usize;
            let offset = field(12) as usize;
            // SAFETY: the view's buffer (after the validity and view buffers) holds its
            // bytes; the vector keeps the Arrow data alive (`keep_alive`).
            unsafe {
                let data = buffer::<u8>(array, 2 + buffer_index);
                StringT::from_raw(std::slice::from_raw_parts(data.add(offset), length))
            }
        };
    }
}

/// Copies `size` values of `width` bytes, from row `offset` of the Arrow data buffer
/// (DuckDB's `DirectConversion`, which points the vector at it).
///
/// # Safety
/// `array`'s second buffer must hold `offset + size` values of `width` bytes.
unsafe fn direct_conversion(
    vector: &mut Vector,
    array: &ArrowArray,
    offset: usize,
    size: usize,
    width: usize,
) {
    let bytes = size * width;
    if bytes == 0 {
        return;
    }
    let target = vector.data_bytes_mut();
    // SAFETY: guaranteed by the caller; the vector has room for `size` rows.
    unsafe {
        let source = buffer::<u8>(array, 1).add(offset * width);
        std::ptr::copy_nonoverlapping(source, target.as_mut_ptr(), bytes);
    }
}

/// Converts `size` values of type `S` from row `offset` of the Arrow data buffer with
/// `convert`; with `skip_nulls` NULL rows are left alone.
///
/// # Safety
/// `array`'s second buffer must hold `offset + size` values of type `S`.
unsafe fn convert_values<S: Copy, T: NativeType>(
    vector: &mut Vector,
    array: &ArrowArray,
    offset: usize,
    size: usize,
    skip_nulls: bool,
    convert: impl Fn(S) -> Result<T>,
) -> Result<()> {
    // SAFETY: guaranteed by the caller.
    let source = unsafe { buffer::<S>(array, 1).add(offset) };
    let validity = vector.validity().clone();
    let skip = skip_nulls && !validity.is_all_valid();
    let target = vector.values_mut::<T>();
    for (row, target) in target.iter_mut().enumerate().take(size) {
        if skip && !validity.is_valid(row) {
            continue;
        }
        // SAFETY: as above.
        *target = convert(unsafe { read(source, row) })?;
    }
    Ok(())
}

/// `value * factor`, failing with `message` on overflow (`TryMultiplyOperator`).
fn multiply(value: i64, factor: i64, message: &'static str) -> Result<i64> {
    value
        .checked_mul(factor)
        .ok_or_else(|| Error::conversion(message))
}

/// Converts a decimal of the Arrow width `S` to the vector's decimal width
/// (`ConvertDecimal`); NULL rows are skipped.
///
/// # Safety
/// `array`'s second buffer must hold `offset + size` values of type `S`.
unsafe fn convert_decimal<S: Copy + Into<i128>>(
    vector: &mut Vector,
    array: &ArrowArray,
    offset: usize,
    size: usize,
) -> Result<()> {
    // The value fits the vector's width, by the decimal's width.
    // SAFETY: guaranteed by the caller, for every conversion below.
    unsafe {
        match vector.logical_type().physical_type() {
            PhysicalType::Int16 => {
                convert_values(vector, array, offset, size, true, |v: S| Ok(v.into() as i16))
            }
            PhysicalType::Int32 => {
                convert_values(vector, array, offset, size, true, |v: S| Ok(v.into() as i32))
            }
            PhysicalType::Int64 => {
                convert_values(vector, array, offset, size, true, |v: S| Ok(v.into() as i64))
            }
            PhysicalType::Int128 => convert_values(vector, array, offset, size, true, |v: S| Ok(v.into())),
            physical => Err(Error::not_implemented(format!(
                "Unsupported physical type for Decimal: {}",
                physical.name()
            ))),
        }
    }
}

/// Converts `size` rows of a non-dictionary, non-run-end encoded Arrow array into
/// `vector`, whose validity the caller set (`ColumnArrowToDuckDB`). Extension types are
/// converted from their storage type unless `ignore_extensions`.
///
/// # Safety
/// `array` must follow the C data interface and be of `arrow_type`.
#[allow(clippy::too_many_arguments, reason = "DuckDB's conversion signature")]
unsafe fn column_arrow_to_duckdb(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    nested_offset: i64,
    parent_mask: Option<&ValidityMask>,
    parent_offset: u64,
    ignore_extensions: bool,
) -> Result<()> {
    if !ignore_extensions
        && let Some(extension) = &arrow_type.extension_data
        && let Some(arrow_to_duckdb) = extension.arrow_to_duckdb
    {
        // Convert the storage, then cast it to the extension's type.
        let mut input_data = Vector::flat(extension.internal_type().clone(), size.max(STANDARD_VECTOR_SIZE));
        // SAFETY: guaranteed by the caller.
        unsafe {
            column_arrow_to_duckdb(
                &mut input_data,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                nested_offset,
                parent_mask,
                parent_offset,
                true,
            )
        }?;
        return arrow_to_duckdb(&input_data, vector, size);
    }
    let parent = parent_offset as i64;
    let offset = effective_offset(array, parent, chunk_offset, nested_offset);
    let ty = vector.logical_type().clone();
    // SAFETY: guaranteed by the caller: `array` has the buffers of its type, holding at
    // least `offset + size` rows; every arm reads only those.
    unsafe {
        match ty.id() {
            LogicalTypeId::SqlNull => *vector = Vector::constant_null(ty),
            LogicalTypeId::Boolean => {
                // Arrow bit-packs booleans.
                let bits = buffer::<u8>(array, 1);
                let values = vector.values_mut::<bool>();
                for (row, value) in values.iter_mut().enumerate().take(size) {
                    let bit = offset + row;
                    *value = read(bits, bit / 8) >> (bit % 8) & 1 == 1;
                }
            }
            LogicalTypeId::TinyInt
            | LogicalTypeId::SmallInt
            | LogicalTypeId::Integer
            | LogicalTypeId::Float
            | LogicalTypeId::Double
            | LogicalTypeId::UTinyInt
            | LogicalTypeId::USmallInt
            | LogicalTypeId::UInteger
            | LogicalTypeId::UBigInt
            | LogicalTypeId::BigInt
            | LogicalTypeId::HugeInt
            | LogicalTypeId::UHugeInt
            | LogicalTypeId::Timestamp
            | LogicalTypeId::TimestampSec
            | LogicalTypeId::TimestampMs
            | LogicalTypeId::TimestampNs
            | LogicalTypeId::TimeTz => {
                direct_conversion(vector, array, offset, size, ty.physical_type().size());
            }
            // Arrow UUIDs are big-endian bytes; DuckDB flips the top bit (`UUIDConversion`).
            LogicalTypeId::Uuid => convert_values(vector, array, offset, size, true, |bytes: [u8; 16]| {
                Ok(rustdb_types::Uuid::from_blob(&bytes).0)
            })?,
            LogicalTypeId::Blob | LogicalTypeId::Bit | LogicalTypeId::Bignum | LogicalTypeId::Varchar => {
                let (size_type, fixed_size) = arrow_type.string_info()?;
                match size_type {
                    ArrowVariableSizeType::SuperSize => {
                        set_vector_string(
                            vector,
                            size,
                            buffer::<u8>(array, 2),
                            buffer::<u64>(array, 1).add(offset),
                        )?;
                        array_state.keep_alive(vector);
                    }
                    ArrowVariableSizeType::Normal => {
                        set_vector_string(
                            vector,
                            size,
                            buffer::<u8>(array, 2),
                            buffer::<u32>(array, 1).add(offset),
                        )?;
                        array_state.keep_alive(vector);
                    }
                    ArrowVariableSizeType::View => {
                        set_vector_string_view(vector, size, array, offset);
                        array_state.keep_alive(vector);
                    }
                    ArrowVariableSizeType::FixedSize => {
                        set_validity_mask(vector, array, chunk_offset, size, parent, nested_offset, false);
                        let data = buffer::<u8>(array, 1);
                        for row in 0..size {
                            if !vector.validity().is_valid(row) {
                                continue;
                            }
                            let bytes =
                                std::slice::from_raw_parts(data.add((offset + row) * fixed_size), fixed_size);
                            let blob = vector.add_string(bytes);
                            vector.values_mut::<StringT>()[row] = blob;
                        }
                    }
                }
            }
            LogicalTypeId::Date => match arrow_type.date_time_info()? {
                ArrowDateTimeType::Days => direct_conversion(vector, array, offset, size, 4),
                ArrowDateTimeType::Milliseconds => {
                    convert_values(vector, array, offset, size, false, |millis: u64| {
                        Ok(rustdb_types::Date((millis as i64 / (1000 * 60 * 60 * 24)) as i32))
                    })?;
                }
                _ => return Err(Error::not_implemented("Unsupported precision for Date Type ")),
            },
            LogicalTypeId::Time => {
                const MESSAGE: &str = "Could not convert Time to Microsecond";
                let to_time = |micros: i64| rustdb_types::Time(micros);
                match arrow_type.date_time_info()? {
                    ArrowDateTimeType::Seconds => {
                        convert_values(vector, array, offset, size, true, |v: i32| {
                            Ok(to_time(multiply(i64::from(v), 1_000_000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Milliseconds => {
                        convert_values(vector, array, offset, size, true, |v: i32| {
                            Ok(to_time(multiply(i64::from(v), 1000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Microseconds => {
                        convert_values(vector, array, offset, size, true, |v: i64| Ok(to_time(v)))?;
                    }
                    ArrowDateTimeType::Nanoseconds => {
                        convert_values(vector, array, offset, size, false, |v: i64| Ok(to_time(v / 1000)))?;
                    }
                    _ => return Err(Error::not_implemented("Unsupported precision for Time Type ")),
                }
            }
            LogicalTypeId::TimeNs => {
                const MESSAGE: &str = "Could not convert TimeNS to Nanoseconds";
                let to_time = |nanos: i64| rustdb_types::TimeNs(nanos);
                match arrow_type.date_time_info()? {
                    ArrowDateTimeType::Seconds => {
                        convert_values(vector, array, offset, size, true, |v: i32| {
                            Ok(to_time(multiply(i64::from(v), 1_000_000_000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Milliseconds => {
                        convert_values(vector, array, offset, size, true, |v: i32| {
                            Ok(to_time(multiply(i64::from(v), 1_000_000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Microseconds => {
                        convert_values(vector, array, offset, size, true, |v: i64| {
                            Ok(to_time(multiply(v, 1000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Nanoseconds => {
                        convert_values(vector, array, offset, size, true, |v: i64| Ok(to_time(v)))?;
                    }
                    _ => return Err(Error::not_implemented("Unsupported precision for TimeNS Type ")),
                }
            }
            LogicalTypeId::TimestampTz => {
                const MESSAGE: &str = "Could not convert TimestampTZ to Microsecond";
                let to_timestamp = |micros: i64| rustdb_types::Timestamp(micros);
                match arrow_type.date_time_info()? {
                    ArrowDateTimeType::Seconds => {
                        convert_values(vector, array, offset, size, true, |v: i64| {
                            Ok(to_timestamp(multiply(v, 1_000_000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Milliseconds => {
                        convert_values(vector, array, offset, size, true, |v: i64| {
                            Ok(to_timestamp(multiply(v, 1000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Microseconds => direct_conversion(vector, array, offset, size, 8),
                    ArrowDateTimeType::Nanoseconds => {
                        convert_values(vector, array, offset, size, false, |v: i64| {
                            Ok(to_timestamp(v / 1000))
                        })?;
                    }
                    _ => {
                        return Err(Error::not_implemented(
                            "Unsupported precision for TimestampTZ Type ",
                        ));
                    }
                }
            }
            LogicalTypeId::Interval => {
                const MESSAGE: &str = "Could not convert Interval to Microsecond";
                let micros = |micros: i64| Interval {
                    months: 0,
                    days: 0,
                    micros,
                };
                match arrow_type.date_time_info()? {
                    ArrowDateTimeType::Seconds => {
                        convert_values(vector, array, offset, size, false, |v: i64| {
                            Ok(micros(multiply(v, 1_000_000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Days | ArrowDateTimeType::Milliseconds => {
                        convert_values(vector, array, offset, size, false, |v: i64| {
                            Ok(micros(multiply(v, 1000, MESSAGE)?))
                        })?;
                    }
                    ArrowDateTimeType::Microseconds => {
                        convert_values(vector, array, offset, size, false, |v: i64| Ok(micros(v)))?;
                    }
                    ArrowDateTimeType::Nanoseconds => {
                        convert_values(vector, array, offset, size, false, |v: i64| Ok(micros(v / 1000)))?;
                    }
                    ArrowDateTimeType::Months => {
                        convert_values(vector, array, offset, size, false, |months: i32| {
                            Ok(Interval {
                                months,
                                days: 0,
                                micros: 0,
                            })
                        })?;
                    }
                    ArrowDateTimeType::MonthDayNano => {
                        // `ArrowInterval`: months, days and nanoseconds.
                        convert_values(vector, array, offset, size, false, |v: [u8; 16]| {
                            let months = i32::from_le_bytes([v[0], v[1], v[2], v[3]]);
                            let days = i32::from_le_bytes([v[4], v[5], v[6], v[7]]);
                            let nanos =
                                i64::from_le_bytes([v[8], v[9], v[10], v[11], v[12], v[13], v[14], v[15]]);
                            Ok(Interval {
                                months,
                                days,
                                micros: nanos / 1000,
                            })
                        })?;
                    }
                }
            }
            LogicalTypeId::Decimal => match arrow_type.decimal_info()? {
                DecimalBitWidth::Decimal32 => convert_decimal::<i32>(vector, array, offset, size)?,
                DecimalBitWidth::Decimal64 => convert_decimal::<i64>(vector, array, offset, size)?,
                DecimalBitWidth::Decimal128 => convert_decimal::<i128>(vector, array, offset, size)?,
            },
            LogicalTypeId::List => arrow_to_duckdb_list(
                vector,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                nested_offset,
                parent_mask,
                parent,
            )?,
            LogicalTypeId::Array => arrow_to_duckdb_array(
                vector,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                nested_offset,
                parent_mask,
                parent,
            )?,
            LogicalTypeId::Map => {
                arrow_to_duckdb_list(
                    vector,
                    array,
                    chunk_offset,
                    array_state,
                    size,
                    arrow_type,
                    nested_offset,
                    parent_mask,
                    parent,
                )?;
                verify_map(vector, size)?;
            }
            LogicalTypeId::Struct => {
                let children = arrow_type.struct_children()?;
                let struct_mask = vector.validity().clone();
                let child_vectors = vector
                    .struct_children_mut()
                    .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
                for (index, (child_vector, child_type)) in child_vectors.iter_mut().zip(children).enumerate()
                {
                    let child = child_array(array, index);
                    let child_state = array_state.child(index);
                    set_validity_mask(
                        child_vector,
                        child,
                        chunk_offset,
                        size,
                        array.offset,
                        nested_offset,
                        false,
                    );
                    apply_parent_mask(child_vector, Some(&struct_mask), size);
                    convert_nested_child(
                        child_vector,
                        child,
                        chunk_offset,
                        child_state,
                        size,
                        child_type,
                        nested_offset,
                        Some(&struct_mask),
                        array.offset as u64,
                    )?;
                }
            }
            LogicalTypeId::Union => {
                // Unions have no validity buffer: the type ids may come first.
                let type_ids_buffer = usize::from(array.n_buffers != 1);
                let type_ids = buffer::<i8>(array, type_ids_buffer).add(offset);
                let members = ty.union_members().unwrap_or_default().to_vec();
                let union_mask = vector.validity().clone();
                let member_types = arrow_type.struct_children()?;
                let mut children = Vec::with_capacity(member_types.len());
                for (index, ((_, member_type), child_type)) in members.iter().zip(member_types).enumerate() {
                    let mut child_vector = Vector::flat(member_type.clone(), size);
                    let child = child_array(array, index);
                    let child_state = array_state.child(index);
                    set_validity_mask(
                        &mut child_vector,
                        child,
                        chunk_offset,
                        size,
                        parent,
                        nested_offset,
                        false,
                    );
                    match child_type.physical_type() {
                        ArrowArrayPhysicalType::DictionaryEncoded => column_arrow_to_duckdb_dictionary(
                            &mut child_vector,
                            child,
                            chunk_offset,
                            child_state,
                            size,
                            child_type,
                            -1,
                            None,
                            0,
                        )?,
                        ArrowArrayPhysicalType::RunEndEncoded => column_arrow_to_duckdb_run_end_encoded(
                            &mut child_vector,
                            child,
                            chunk_offset,
                            child_state,
                            size,
                            child_type,
                            -1,
                            0,
                        )?,
                        ArrowArrayPhysicalType::Default => column_arrow_to_duckdb(
                            &mut child_vector,
                            child,
                            chunk_offset,
                            child_state,
                            size,
                            child_type,
                            nested_offset,
                            Some(&union_mask),
                            0,
                            false,
                        )?,
                    }
                    children.push(child_vector);
                }
                for row in 0..size {
                    let tag = read(type_ids, row) as u8;
                    let Some(member) = children.get(usize::from(tag)) else {
                        return Err(Error::invalid_input(format!(
                            "Arrow union tag out of range: {tag}"
                        )));
                    };
                    let value = member.get_value(row);
                    let value = if value.is_null() {
                        Value::default()
                    } else {
                        Value::union_value(ty.clone(), tag, value)
                    };
                    vector.set_value(row, &value)?;
                }
            }
            _ => {
                return Err(Error::not_implemented(format!(
                    "Unsupported type for arrow conversion: {ty}"
                )));
            }
        }
    }
    Ok(())
}

/// The run that holds row `offset`: the first run end greater than it, or `count`
/// (`FindRunIndex`).
fn find_run_index(run_ends: &[i64], offset: usize) -> usize {
    run_ends.partition_point(|&run_end| offset as i64 >= run_end)
}

/// Expands the runs covering rows `scan_offset..scan_offset + count` into `result`
/// (`FlattenRunEnds`).
fn flatten_run_ends<T: NativeType>(
    result: &mut Vector,
    run_ends: &[i64],
    values: &Vector,
    scan_offset: usize,
    count: usize,
) {
    let format = values.unified::<T>();
    let mut run = find_run_index(run_ends, scan_offset);
    let mut index = 0;
    // The run ends are never NULL (by the Arrow specification).
    while run < run_ends.len() && index < count {
        let run_end = run_ends[run] as usize;
        let to_scan = (run_end - (scan_offset + index)).min(count - index);
        let slot = format.sel.slot(run);
        if format.validity.is_valid(slot) {
            let value = format.values[slot];
            result.values_mut::<T>()[index..index + to_scan].fill(value);
        } else {
            for row in index..index + to_scan {
                result.validity_mut().set_invalid(row, count);
            }
        }
        index += to_scan;
        run += 1;
    }
}

/// Converts a run-end encoded array: its run ends and values once per Arrow chunk, then
/// the runs covering the scanned rows (`ColumnArrowToDuckDBRunEndEncoded`).
///
/// # Safety
/// `array` must be a run-end encoded array of the C data interface of `arrow_type`.
#[allow(clippy::too_many_arguments, reason = "DuckDB's conversion signature")]
unsafe fn column_arrow_to_duckdb_run_end_encoded(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    nested_offset: i64,
    parent_offset: u64,
) -> Result<()> {
    let children = arrow_type.struct_children()?;
    let [run_ends_type, values_type] = children else {
        return Err(Error::internal(
            "run-end encoded type without run ends and values",
        ));
    };
    // SAFETY: a run-end encoded array has the run ends and the values as children.
    let (run_ends_array, values_array) = unsafe { (child_array(array, 0), child_array(array, 1)) };
    let compressed_size = run_ends_array.length as usize;
    if array_state.run_end_encoding.run_ends.is_none() {
        // The run ends and values have not been converted for this chunk yet.
        let mut run_ends = Vector::flat(run_ends_type.duck_type(false), compressed_size);
        let mut values = Vector::flat(values_type.duck_type(false), compressed_size);
        // SAFETY: the children follow the C data interface.
        unsafe {
            column_arrow_to_duckdb(
                &mut run_ends,
                run_ends_array,
                chunk_offset,
                array_state,
                compressed_size,
                run_ends_type,
                -1,
                None,
                0,
                false,
            )?;
            set_validity_mask(
                &mut values,
                values_array,
                chunk_offset,
                compressed_size,
                parent_offset as i64,
                nested_offset,
                false,
            );
            column_arrow_to_duckdb(
                &mut values,
                values_array,
                chunk_offset,
                array_state,
                compressed_size,
                values_type,
                -1,
                None,
                0,
                false,
            )?;
        }
        array_state.run_end_encoding = ArrowRunEndEncodingState {
            run_ends: Some(run_ends),
            values: Some(values),
        };
    }
    let (Some(run_ends), Some(values)) = (
        &array_state.run_end_encoding.run_ends,
        &array_state.run_end_encoding.values,
    ) else {
        return Err(Error::internal("run-end encoding state without run ends"));
    };
    let run_ends: Vec<i64> = match run_ends.logical_type().physical_type() {
        PhysicalType::Int16 => run_ends.values::<i16>()[..compressed_size]
            .iter()
            .map(|&v| i64::from(v))
            .collect(),
        PhysicalType::Int32 => run_ends.values::<i32>()[..compressed_size]
            .iter()
            .map(|&v| i64::from(v))
            .collect(),
        PhysicalType::Int64 => run_ends.values::<i64>()[..compressed_size].to_vec(),
        physical => {
            return Err(Error::not_implemented(format!(
                "Type '{}' not implemented for RunEndEncoding",
                physical.name()
            )));
        }
    };
    let scan_offset = effective_offset(array, parent_offset as i64, chunk_offset, nested_offset);
    match values.logical_type().physical_type() {
        PhysicalType::Int8 => flatten_run_ends::<i8>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Int16 => flatten_run_ends::<i16>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Int32 => flatten_run_ends::<i32>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Int64 => flatten_run_ends::<i64>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Int128 => flatten_run_ends::<i128>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::UInt8 => flatten_run_ends::<u8>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::UInt16 => flatten_run_ends::<u16>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::UInt32 => flatten_run_ends::<u32>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::UInt64 => flatten_run_ends::<u64>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Bool => flatten_run_ends::<bool>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Float => flatten_run_ends::<f32>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Double => flatten_run_ends::<f64>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Interval => flatten_run_ends::<Interval>(vector, &run_ends, values, scan_offset, size),
        PhysicalType::Varchar => {
            // The strings stay where the values' strings are.
            vector.add_heap_references(values);
            flatten_run_ends::<StringT>(vector, &run_ends, values, scan_offset, size);
        }
        physical => {
            return Err(Error::not_implemented(format!(
                "RunEndEncoded value type '{}' not supported yet",
                physical.name()
            )));
        }
    }
    Ok(())
}

/// The dictionary indices of `size` rows as a selection; NULL rows select the
/// dictionary's NULL entry at `null_index` (`SetSelectionVector`).
///
/// # Safety
/// `indices` must hold `size` indices of the integer type `index_type`.
unsafe fn selection_vector(
    indices: *const u8,
    index_type: &LogicalType,
    size: usize,
    mask: Option<&ValidityMask>,
    null_index: usize,
) -> Result<SelectionVector> {
    /// The index of row `row`, read as `T`.
    ///
    /// # Safety
    /// `indices` must hold more than `row` values of type `T`.
    unsafe fn index_at<T: Copy + TryInto<i64>>(indices: *const u8, row: usize) -> i64 {
        // SAFETY: guaranteed by the caller.
        unsafe { read(indices.cast::<T>(), row) }
            .try_into()
            .unwrap_or(i64::MAX)
    }
    let read_index: unsafe fn(*const u8, usize) -> i64 = match index_type.id() {
        LogicalTypeId::UTinyInt => index_at::<u8>,
        LogicalTypeId::TinyInt => index_at::<i8>,
        LogicalTypeId::USmallInt => index_at::<u16>,
        LogicalTypeId::SmallInt => index_at::<i16>,
        LogicalTypeId::UInteger => index_at::<u32>,
        LogicalTypeId::Integer => index_at::<i32>,
        LogicalTypeId::UBigInt => index_at::<u64>,
        LogicalTypeId::BigInt => index_at::<i64>,
        _ if mask.is_some() => {
            return Err(Error::not_implemented(format!(
                "(Arrow) Unsupported type for selection vectors {index_type}"
            )));
        }
        _ => {
            return Err(Error::conversion(format!(
                "(Arrow) Unsupported type for selection vectors {index_type}"
            )));
        }
    };
    let wide = matches!(
        index_type.id(),
        LogicalTypeId::UInteger | LogicalTypeId::UBigInt | LogicalTypeId::BigInt
    );
    if mask.is_some() && wide && null_index > u32::MAX as usize {
        // The NULL rows point at the last entry, which does not fit.
        return Err(Error::conversion(
            "DuckDB only supports indices that fit on an uint32",
        ));
    }
    let mut sel = Vec::with_capacity(size);
    for row in 0..size {
        if mask.is_some_and(|mask| !mask.is_valid(row)) {
            sel.push(null_index as u32);
            continue;
        }
        // SAFETY: guaranteed by the caller.
        let index = unsafe { read_index(indices, row) };
        if index > i64::from(u32::MAX) && mask.is_none() && null_index > u32::MAX as usize {
            return Err(Error::conversion(
                "DuckDB only supports indices that fit on an uint32",
            ));
        }
        sel.push(index as u32);
    }
    Ok(SelectionVector::new(sel))
}

/// Converts a dictionary-encoded array: the dictionary once per column, then the indices
/// as a selection of it (`ColumnArrowToDuckDBDictionary`).
///
/// # Safety
/// `array` must be a dictionary-encoded array of the C data interface of `arrow_type`.
#[allow(clippy::too_many_arguments, reason = "DuckDB's conversion signature")]
unsafe fn column_arrow_to_duckdb_dictionary(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    nested_offset: i64,
    parent_mask: Option<&ValidityMask>,
    parent_offset: u64,
) -> Result<()> {
    let dictionary_type = arrow_type
        .dictionary()
        .ok_or_else(|| Error::internal("dictionary conversion of a type without a dictionary"))?;
    let has_nulls = array.null_count > 0 || parent_mask.is_some_and(|mask| !mask.is_all_valid());
    // SAFETY: a dictionary-encoded array has a dictionary.
    let dictionary = unsafe { &*array.dictionary };
    let dictionary_length = dictionary.length as usize;
    if array_state.cache_outdated(array.dictionary) {
        // One entry beyond the dictionary is the NULL that NULL indices select.
        let mut base_vector = Vector::flat(vector.logical_type().clone(), dictionary_length + 1);
        // SAFETY: the dictionary follows the C data interface.
        unsafe {
            set_validity_mask(
                &mut base_vector,
                dictionary,
                chunk_offset,
                dictionary_length,
                0,
                0,
                has_nulls,
            );
            base_vector
                .validity_mut()
                .set_invalid(dictionary_length, dictionary_length + 1);
            match dictionary_type.physical_type() {
                ArrowArrayPhysicalType::DictionaryEncoded => column_arrow_to_duckdb_dictionary(
                    &mut base_vector,
                    dictionary,
                    chunk_offset,
                    array_state,
                    dictionary_length,
                    dictionary_type,
                    -1,
                    None,
                    0,
                )?,
                ArrowArrayPhysicalType::RunEndEncoded => column_arrow_to_duckdb_run_end_encoded(
                    &mut base_vector,
                    dictionary,
                    chunk_offset,
                    array_state,
                    dictionary_length,
                    dictionary_type,
                    -1,
                    0,
                )?,
                ArrowArrayPhysicalType::Default => column_arrow_to_duckdb(
                    &mut base_vector,
                    dictionary,
                    chunk_offset,
                    array_state,
                    dictionary_length,
                    dictionary_type,
                    -1,
                    None,
                    0,
                    false,
                )?,
            }
        }
        array_state.add_dictionary(base_vector, array.dictionary);
    }
    let index_type = arrow_type.duck_type(false);
    let offset = effective_offset(array, parent_offset as i64, chunk_offset, nested_offset);
    // SAFETY: the indices are the array's second buffer.
    let indices = unsafe { buffer::<u8>(array, 1).add(index_type.physical_type().size() * offset) };
    let sel = if has_nulls {
        // SAFETY: the array follows the C data interface.
        let mut indices_validity =
            unsafe { arrow_validity(array, chunk_offset, size, parent_offset as i64, -1) }
                .unwrap_or_default();
        if let Some(parent_mask) = parent_mask.filter(|mask| !mask.is_all_valid()) {
            for row in (0..size).filter(|&row| !parent_mask.is_valid(row)) {
                indices_validity.set_invalid(row, size);
            }
        }
        // SAFETY: the indices hold `size` values of the index type.
        unsafe {
            selection_vector(
                indices,
                &index_type,
                size,
                Some(&indices_validity),
                dictionary_length,
            )
        }?
    } else {
        // SAFETY: as above.
        unsafe { selection_vector(indices, &index_type, size, None, dictionary_length) }?
    };
    let dictionary = array_state
        .dictionary
        .as_ref()
        .ok_or_else(|| Error::internal("Arrow dictionary was not converted"))?;
    *vector = dictionary.slice(&sel);
    Ok(())
}

/// Converts `size` rows of a column of an Arrow struct array, from row `chunk_offset`
/// on, by the column's physical layout; `parent_offset` is the offset of the struct
/// array (the column loop of `ArrowTableFunction::ArrowToDuckDB`).
///
/// # Safety
/// `array` must follow the C data interface and be of `arrow_type`.
pub(crate) unsafe fn convert_column(
    vector: &mut Vector,
    array: &ArrowArray,
    chunk_offset: usize,
    array_state: &mut ArrowArrayScanState,
    size: usize,
    arrow_type: &ArrowType,
    parent_offset: i64,
) -> Result<()> {
    // SAFETY: guaranteed by the caller.
    unsafe {
        match arrow_type.physical_type() {
            ArrowArrayPhysicalType::DictionaryEncoded => column_arrow_to_duckdb_dictionary(
                vector,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                -1,
                None,
                0,
            ),
            ArrowArrayPhysicalType::RunEndEncoded => column_arrow_to_duckdb_run_end_encoded(
                vector,
                array,
                chunk_offset,
                array_state,
                size,
                arrow_type,
                -1,
                0,
            ),
            ArrowArrayPhysicalType::Default => {
                set_validity_mask(vector, array, chunk_offset, size, parent_offset, -1, false);
                column_arrow_to_duckdb(
                    vector,
                    array,
                    chunk_offset,
                    array_state,
                    size,
                    arrow_type,
                    -1,
                    None,
                    0,
                    false,
                )
            }
        }
    }
}

/// Converts an Arrow struct array of the columns of `arrow_table` into a data chunk,
/// taking ownership of the array, which the chunk keeps alive where its strings point
/// into it (the conversion of DuckDB's `duckdb_data_chunk_from_arrow`).
///
/// # Safety
/// `arrow_array` must be a struct array of the C data interface whose children are of
/// the types of `arrow_table`.
pub unsafe fn data_chunk_from_arrow(
    arrow_array: &mut ArrowArray,
    arrow_table: &ArrowTableSchema,
) -> Result<DataChunk> {
    let size = usize::try_from(arrow_array.length).unwrap_or(0);
    // The chunk owns the array from here on; the caller's copy is marked released.
    // SAFETY: the array is a plain C struct; only the copy keeps the release callback.
    let owned_data = Arc::new(ArrowArrayWrapper {
        arrow_array: unsafe { std::ptr::read(arrow_array) },
    });
    arrow_array.release = None;
    let parent_array = &owned_data.arrow_array;
    if parent_array.n_children < arrow_table.columns().len() as i64 {
        return Err(Error::invalid_input(
            "Arrow array has fewer columns than its schema",
        ));
    }
    let mut columns = Vec::with_capacity(arrow_table.columns().len());
    for (index, (arrow_type, ty)) in arrow_table.columns().iter().zip(arrow_table.types()).enumerate() {
        let mut array_state = ArrowArrayScanState {
            owned_data: Some(Arc::clone(&owned_data)),
            ..ArrowArrayScanState::default()
        };
        let mut vector = Vector::flat(ty.clone(), size);
        // SAFETY: guaranteed by the caller.
        unsafe {
            convert_column(
                &mut vector,
                child_array(parent_array, index),
                0,
                &mut array_state,
                size,
                arrow_type,
                parent_array.offset,
            )
        }?;
        columns.push(vector);
    }
    Ok(DataChunk::new(columns, size))
}
