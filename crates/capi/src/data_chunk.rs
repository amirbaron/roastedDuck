//! Data chunks, vectors, validity masks and selection vectors (DuckDB's
//! `capi/data_chunk-c.cpp`).
//!
//! A `duckdb_data_chunk` points to a heap-allocated [`DataChunk`] and a `duckdb_vector` to
//! a [`Vector`], either one of a chunk's columns, a child of a nested vector or one from
//! `duckdb_create_vector`. The vectors use DuckDB's in-memory layout, so the data and
//! validity pointers handed out are the vectors' own buffers: writes through them are
//! seen by the engine, and they stay valid until the vector is restructured (resized,
//! reset, sliced or referenced) or dropped.

use std::ffi::c_char;

use rustdb_types::{
    DataChunk, PhysicalType, STANDARD_VECTOR_SIZE, SelectionVector, StringT, Vector, VectorKind,
};

use crate::error_data::duckdb_destroy_error_data;
use crate::helper::{bytes, duckdb_valid_utf8_check};
use crate::logical_types::{DuckdbLogicalType, is_unresolved, type_ref, wrap_type};
use crate::types::*;
use crate::value::{DuckdbValue, value_ref};

/// `duckdb_data_chunk`
pub type DuckdbDataChunk = *mut DataChunk;
/// `duckdb_vector`
pub type DuckdbVector = *mut Vector;

/// What a `duckdb_selection_vector` points to: `sel_t` entries the caller writes in place
/// (DuckDB's `SelectionVector` with its own buffer).
#[derive(Debug)]
pub struct CSelectionVector {
    indices: Vec<SelT>,
}

/// `duckdb_selection_vector`
pub type DuckdbSelectionVector = *mut CSelectionVector;

/// Hands `chunk` to C.
pub(crate) fn wrap_chunk(chunk: DataChunk) -> DuckdbDataChunk {
    Box::into_raw(Box::new(chunk))
}

/// A copy of the chunk's rows in new flat vectors (nested ones included) with room for
/// `duckdb_vector_size()` rows, as DuckDB hands out the chunks of a result
/// (`ColumnDataCollection::FetchChunk`): C callers own and may write to it.
pub(crate) fn flat_chunk(chunk: &DataChunk) -> DataChunk {
    let count = chunk.len();
    let types: Vec<_> = chunk
        .columns()
        .iter()
        .map(|column| column.logical_type().clone())
        .collect();
    let mut copy = DataChunk::with_types(&types, STANDARD_VECTOR_SIZE.max(count));
    for (index, column) in chunk.columns().iter().enumerate() {
        // Copying between vectors of one type only fails for corrupt vectors.
        let _ = rustdb_types::vector_ops::copy_rows(column, 0, count, copy.column_mut(index), 0);
    }
    copy.set_len(count);
    copy
}

/// Borrows the chunk behind a handle.
///
/// # Safety
/// `chunk` must be NULL or a live `duckdb_data_chunk`.
unsafe fn chunk_mut<'a>(chunk: DuckdbDataChunk) -> Option<&'a mut DataChunk> {
    // SAFETY: guaranteed by the caller.
    unsafe { chunk.as_mut() }
}

/// Borrows the vector behind a handle.
///
/// # Safety
/// `vector` must be NULL or a live `duckdb_vector`, not borrowed elsewhere for the
/// lifetime of the result.
pub(crate) unsafe fn vector_mut<'a>(vector: DuckdbVector) -> Option<&'a mut Vector> {
    // SAFETY: guaranteed by the caller.
    unsafe { vector.as_mut() }
}

/// `duckdb_data_chunk duckdb_create_data_chunk(duckdb_logical_type *types, idx_t column_count)`
/// (capi/data_chunk-c.cpp): an empty chunk with room for `duckdb_vector_size()` rows.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_data_chunk(
    types: *mut DuckdbLogicalType,
    column_count: Idx,
) -> DuckdbDataChunk {
    if types.is_null() {
        return std::ptr::null_mut();
    }
    let mut column_types = Vec::with_capacity(column_count as usize);
    for i in 0..column_count as usize {
        // SAFETY: `types` holds `column_count` handles per the API contract.
        let Some(ty) = (unsafe { type_ref(*types.add(i)) }) else {
            return std::ptr::null_mut();
        };
        if is_unresolved(ty) {
            return std::ptr::null_mut();
        }
        column_types.push(ty.clone());
    }
    wrap_chunk(DataChunk::with_types(&column_types, STANDARD_VECTOR_SIZE))
}

/// `void duckdb_destroy_data_chunk(duckdb_data_chunk *chunk)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_data_chunk(chunk: *mut DuckdbDataChunk) {
    // SAFETY: `chunk` is NULL or points to NULL or a handle owned by the caller.
    unsafe {
        let Some(slot) = chunk.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `void duckdb_data_chunk_reset(duckdb_data_chunk chunk)` (capi/data_chunk-c.cpp): no
/// rows, every vector emptied for reuse (`DataChunk::Reset`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_reset(chunk: DuckdbDataChunk) {
    // SAFETY: `chunk` is NULL or a live handle.
    let Some(chunk) = (unsafe { chunk_mut(chunk) }) else {
        return;
    };
    for column in 0..chunk.column_count() {
        chunk.column_mut(column).reset_from_cache(STANDARD_VECTOR_SIZE);
    }
    chunk.set_len(0);
}

/// `duckdb_vector duckdb_create_vector(duckdb_logical_type type, idx_t capacity)`
/// (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_create_vector(ty: DuckdbLogicalType, capacity: Idx) -> DuckdbVector {
    // SAFETY: `ty` is NULL or a live handle.
    unsafe { type_ref(ty) }.map_or(std::ptr::null_mut(), |ty| {
        Box::into_raw(Box::new(Vector::flat(ty.clone(), capacity as usize)))
    })
}

/// `void duckdb_destroy_vector(duckdb_vector *vector)` (capi/data_chunk-c.cpp): only for
/// vectors from `duckdb_create_vector`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_vector(vector: *mut DuckdbVector) {
    // SAFETY: `vector` is NULL or points to NULL or a handle from `duckdb_create_vector`.
    unsafe {
        let Some(slot) = vector.as_mut() else { return };
        if !slot.is_null() {
            drop(Box::from_raw(*slot));
            *slot = std::ptr::null_mut();
        }
    }
}

/// `idx_t duckdb_data_chunk_get_column_count(duckdb_data_chunk chunk)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_get_column_count(chunk: DuckdbDataChunk) -> Idx {
    // SAFETY: `chunk` is NULL or a live handle.
    unsafe { chunk_mut(chunk) }.map_or(0, |chunk| chunk.column_count() as Idx)
}

/// `duckdb_vector duckdb_data_chunk_get_vector(duckdb_data_chunk chunk, idx_t col_idx)`
/// (capi/data_chunk-c.cpp): the column, owned by the chunk.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_get_vector(chunk: DuckdbDataChunk, col_idx: Idx) -> DuckdbVector {
    // SAFETY: `chunk` is NULL or a live handle.
    match unsafe { chunk_mut(chunk) } {
        Some(chunk) if (col_idx as usize) < chunk.column_count() => chunk.column_mut(col_idx as usize),
        _ => std::ptr::null_mut(),
    }
}

/// `idx_t duckdb_data_chunk_get_size(duckdb_data_chunk chunk)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_get_size(chunk: DuckdbDataChunk) -> Idx {
    // SAFETY: `chunk` is NULL or a live handle.
    unsafe { chunk_mut(chunk) }.map_or(0, |chunk| chunk.len() as Idx)
}

/// `void duckdb_data_chunk_set_size(duckdb_data_chunk chunk, idx_t size)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_data_chunk_set_size(chunk: DuckdbDataChunk, size: Idx) {
    // SAFETY: `chunk` is NULL or a live handle.
    if let Some(chunk) = unsafe { chunk_mut(chunk) } {
        chunk.set_len(size as usize);
    }
}

/// `duckdb_logical_type duckdb_vector_get_column_type(duckdb_vector vector)`
/// (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_get_column_type(vector: DuckdbVector) -> DuckdbLogicalType {
    // SAFETY: `vector` is NULL or a live handle.
    unsafe { vector_mut(vector) }.map_or(std::ptr::null_mut(), |vector| {
        wrap_type(vector.logical_type().clone())
    })
}

/// `void *duckdb_vector_get_data(duckdb_vector vector)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_get_data(vector: DuckdbVector) -> *mut std::ffi::c_void {
    // SAFETY: `vector` is NULL or a live handle.
    unsafe { vector_mut(vector) }.map_or(std::ptr::null_mut(), |vector| vector.data_ptr_mut().cast())
}

/// `uint64_t *duckdb_vector_get_validity(duckdb_vector vector)` (capi/data_chunk-c.cpp):
/// NULL while every row is valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_get_validity(vector: DuckdbVector) -> *mut u64 {
    // SAFETY: `vector` is NULL or a live handle.
    let Some(vector) = (unsafe { vector_mut(vector) }) else {
        return std::ptr::null_mut();
    };
    if !matches!(vector.kind(), VectorKind::Flat | VectorKind::Constant) {
        return std::ptr::null_mut();
    }
    vector
        .validity_words_mut()
        .map_or(std::ptr::null_mut(), <[u64]>::as_mut_ptr)
}

/// `void duckdb_vector_ensure_validity_writable(duckdb_vector vector)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_ensure_validity_writable(vector: DuckdbVector) {
    // SAFETY: `vector` is NULL or a live handle.
    if let Some(vector) = unsafe { vector_mut(vector) } {
        vector.ensure_validity_writable();
    }
}

/// Marks row `index` NULL, the treatment of invalid UTF-8 in the string assignments.
fn set_null(vector: &mut Vector, index: Idx) {
    let words = vector.ensure_validity_writable();
    if let Some(word) = words.get_mut(index as usize / 64) {
        *word &= !(1 << (index % 64));
    }
}

/// `void duckdb_vector_assign_string_element(duckdb_vector vector, idx_t index, const char *str)`
/// (capi/data_chunk-c.cpp): invalid UTF-8 sets the row NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_assign_string_element(
    vector: DuckdbVector,
    index: Idx,
    str: *const c_char,
) {
    if vector.is_null() || str.is_null() {
        return;
    }
    // SAFETY: `str` is a NUL-terminated string per the API contract.
    let len = unsafe { libc::strlen(str) } as Idx;
    // SAFETY: `str` has `len` bytes.
    let mut error = unsafe { duckdb_valid_utf8_check(str, len) };
    if !error.is_null() {
        // SAFETY: `error` was just created; `vector` is a live handle.
        unsafe {
            duckdb_destroy_error_data(&raw mut error);
            if let Some(vector) = vector_mut(vector) {
                set_null(vector, index);
            }
        }
        return;
    }
    // SAFETY: forwarded API contract.
    unsafe { duckdb_unsafe_vector_assign_string_element_len(vector, index, str, len) };
}

/// `void duckdb_vector_assign_string_element_len(duckdb_vector vector, idx_t index,
/// const char *str, idx_t str_len)` (capi/data_chunk-c.cpp): VARCHAR vectors set the row
/// NULL for invalid UTF-8; BLOB vectors take any bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_assign_string_element_len(
    vector: DuckdbVector,
    index: Idx,
    str: *const c_char,
    str_len: Idx,
) {
    // SAFETY: `vector` is NULL or a live handle.
    let Some(target) = (unsafe { vector_mut(vector) }) else {
        return;
    };
    if target.logical_type().id() == rustdb_types::LogicalTypeId::Varchar {
        // SAFETY: `str` points to `str_len` bytes per the API contract.
        let mut error = unsafe { duckdb_valid_utf8_check(str, str_len) };
        if !error.is_null() {
            // SAFETY: `error` was just created.
            unsafe { duckdb_destroy_error_data(&raw mut error) };
            set_null(target, index);
            return;
        }
    }
    // SAFETY: forwarded API contract.
    unsafe { duckdb_unsafe_vector_assign_string_element_len(vector, index, str, str_len) };
}

/// `void duckdb_unsafe_vector_assign_string_element_len(duckdb_vector vector, idx_t index,
/// const char *str, idx_t str_len)` (capi/data_chunk-c.cpp): stores the bytes without
/// checking them (`StringVector::AddStringOrBlob`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_unsafe_vector_assign_string_element_len(
    vector: DuckdbVector,
    index: Idx,
    str: *const c_char,
    str_len: Idx,
) {
    // SAFETY: `vector` is NULL or a live handle.
    let Some(vector) = (unsafe { vector_mut(vector) }) else {
        return;
    };
    if vector.logical_type().physical_type() != PhysicalType::Varchar {
        return;
    }
    // SAFETY: `str` points to `str_len` bytes per the API contract.
    let text = unsafe { bytes(str.cast(), str_len) };
    let string = vector.add_string(text);
    if let Some(slot) = vector.values_mut::<StringT>().get_mut(index as usize) {
        *slot = string;
    }
}

/// `duckdb_vector duckdb_list_vector_get_child(duckdb_vector vector)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_list_vector_get_child(vector: DuckdbVector) -> DuckdbVector {
    // SAFETY: `vector` is NULL or a live handle.
    unsafe { vector_mut(vector) }
        .and_then(Vector::list_child_mut)
        .map_or(std::ptr::null_mut(), std::ptr::from_mut)
}

/// `idx_t duckdb_list_vector_get_size(duckdb_vector vector)` (capi/data_chunk-c.cpp): the
/// number of child rows in use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_list_vector_get_size(vector: DuckdbVector) -> Idx {
    // SAFETY: `vector` is NULL or a live handle.
    unsafe { vector_mut(vector) }
        .and_then(|vector| vector.list_child())
        .map_or(0, |(_, size)| size as Idx)
}

/// `duckdb_state duckdb_list_vector_set_size(duckdb_vector vector, idx_t size)`
/// (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_list_vector_set_size(vector: DuckdbVector, size: Idx) -> DuckdbState {
    // SAFETY: `vector` is NULL or a live handle.
    let Some(vector) = (unsafe { vector_mut(vector) }) else {
        return DuckdbState::DuckDBError;
    };
    vector.set_list_size(size as usize);
    DuckdbState::DuckDBSuccess
}

/// `duckdb_state duckdb_list_vector_reserve(duckdb_vector vector, idx_t required_capacity)`
/// (capi/data_chunk-c.cpp): grows the child to the next power of two that holds
/// `required_capacity` rows (`VectorListBuffer::Reserve`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_list_vector_reserve(
    vector: DuckdbVector,
    required_capacity: Idx,
) -> DuckdbState {
    // SAFETY: `vector` is NULL or a live handle.
    let Some(vector) = (unsafe { vector_mut(vector) }) else {
        return DuckdbState::DuckDBError;
    };
    if let Some(child) = vector.list_child_mut() {
        let required = required_capacity as usize;
        if required > child.capacity() {
            child.resize(required.next_power_of_two());
        }
    }
    DuckdbState::DuckDBSuccess
}

/// `duckdb_vector duckdb_struct_vector_get_child(duckdb_vector vector, idx_t index)`
/// (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_struct_vector_get_child(vector: DuckdbVector, index: Idx) -> DuckdbVector {
    // SAFETY: `vector` is NULL or a live handle.
    unsafe { vector_mut(vector) }
        .and_then(Vector::struct_children_mut)
        .and_then(|children| children.get_mut(index as usize))
        .map_or(std::ptr::null_mut(), std::ptr::from_mut)
}

/// `duckdb_vector duckdb_array_vector_get_child(duckdb_vector vector)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_array_vector_get_child(vector: DuckdbVector) -> DuckdbVector {
    // SAFETY: `vector` is NULL or a live handle.
    unsafe { vector_mut(vector) }
        .and_then(Vector::array_child_mut)
        .map_or(std::ptr::null_mut(), std::ptr::from_mut)
}

/// `bool duckdb_validity_row_is_valid(uint64_t *validity, idx_t row)` (capi/data_chunk-c.cpp)
pub unsafe extern "C" fn duckdb_validity_row_is_valid(validity: *mut u64, row: Idx) -> bool {
    if validity.is_null() {
        return true;
    }
    // SAFETY: `validity` covers `row` per the API contract.
    let word = unsafe { *validity.add(row as usize / 64) };
    word & (1 << (row % 64)) != 0
}

// Keep the exported C boolean's entire byte canonical. The optimized x86_64
// boolean implementation can return a shifted word with only bit zero defined;
// C callers compiled by GCC test the whole byte. Returning u8 at this boundary
// forces zero-extension to the C bool representation, without changing duckdb.h
// or the Rust-facing function's signature.
#[unsafe(export_name = "duckdb_validity_row_is_valid")]
unsafe extern "C" fn validity_row_is_valid_c(validity: *mut u64, row: Idx) -> u8 {
    // SAFETY: the exported function has the same pointer contract as the helper.
    u8::from(unsafe { duckdb_validity_row_is_valid(validity, row) })
}

/// `void duckdb_validity_set_row_validity(uint64_t *validity, idx_t row, bool valid)`
/// (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_validity_set_row_validity(validity: *mut u64, row: Idx, valid: bool) {
    // SAFETY: forwarded API contract.
    unsafe {
        if valid {
            duckdb_validity_set_row_valid(validity, row);
        } else {
            duckdb_validity_set_row_invalid(validity, row);
        }
    }
}

/// `void duckdb_validity_set_row_invalid(uint64_t *validity, idx_t row)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_validity_set_row_invalid(validity: *mut u64, row: Idx) {
    if validity.is_null() {
        return;
    }
    // SAFETY: `validity` covers `row` per the API contract.
    unsafe { *validity.add(row as usize / 64) &= !(1 << (row % 64)) };
}

/// `void duckdb_validity_set_row_valid(uint64_t *validity, idx_t row)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_validity_set_row_valid(validity: *mut u64, row: Idx) {
    if validity.is_null() {
        return;
    }
    // SAFETY: `validity` covers `row` per the API contract.
    unsafe { *validity.add(row as usize / 64) |= 1 << (row % 64) };
}

/// `duckdb_selection_vector duckdb_create_selection_vector(idx_t size)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub extern "C" fn duckdb_create_selection_vector(size: Idx) -> DuckdbSelectionVector {
    Box::into_raw(Box::new(CSelectionVector {
        indices: vec![0; size as usize],
    }))
}

/// `void duckdb_destroy_selection_vector(duckdb_selection_vector sel)` (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_destroy_selection_vector(sel: DuckdbSelectionVector) {
    if !sel.is_null() {
        // SAFETY: `sel` came from `duckdb_create_selection_vector`.
        drop(unsafe { Box::from_raw(sel) });
    }
}

/// `sel_t *duckdb_selection_vector_get_data_ptr(duckdb_selection_vector sel)`
/// (capi/data_chunk-c.cpp)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_selection_vector_get_data_ptr(sel: DuckdbSelectionVector) -> *mut SelT {
    // SAFETY: `sel` is NULL or a live handle.
    unsafe { sel.as_mut() }.map_or(std::ptr::null_mut(), |sel| sel.indices.as_mut_ptr())
}

/// The first `len` entries of a C selection vector.
///
/// # Safety
/// `sel` must be NULL or a live `duckdb_selection_vector`.
unsafe fn selection(sel: DuckdbSelectionVector, len: Idx) -> Option<SelectionVector> {
    // SAFETY: guaranteed by the caller.
    let indices = &unsafe { sel.as_ref() }?.indices;
    Some(SelectionVector::new(
        &indices[..(len as usize).min(indices.len())],
    ))
}

/// `void duckdb_slice_vector(duckdb_vector vector, duckdb_selection_vector sel, idx_t len)`
/// (capi/data_chunk-c.cpp): the vector becomes a dictionary over its old rows.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_slice_vector(vector: DuckdbVector, sel: DuckdbSelectionVector, len: Idx) {
    // SAFETY: both are NULL or live handles.
    let (Some(vector), Some(sel)) = (unsafe { (vector_mut(vector), selection(sel, len)) }) else {
        return;
    };
    *vector = vector.slice(&sel);
}

/// `void duckdb_vector_copy_sel(duckdb_vector src, duckdb_vector dst, duckdb_selection_vector sel,
/// idx_t src_count, idx_t src_offset, idx_t dst_offset)` (capi/data_chunk-c.cpp): row
/// `dst_offset + i` of `dst` becomes row `sel[src_offset + i]` of `src`
/// (`VectorOperations::Copy`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_copy_sel(
    src: DuckdbVector,
    dst: DuckdbVector,
    sel: DuckdbSelectionVector,
    src_count: Idx,
    src_offset: Idx,
    dst_offset: Idx,
) {
    if src == dst || src_offset >= src_count {
        return;
    }
    // SAFETY: the handles are NULL or live and distinct (checked above).
    let (Some(src), Some(dst), Some(sel)) =
        (unsafe { (vector_mut(src), vector_mut(dst), selection(sel, src_count)) })
    else {
        return;
    };
    let selected = src.slice(&sel);
    let count = (src_count - src_offset) as usize;
    // A failed copy leaves `dst` partially written, like an exception would in DuckDB.
    let _ =
        rustdb_types::vector_ops::copy_rows(&selected, src_offset as usize, count, dst, dst_offset as usize);
}

/// `void duckdb_vector_reference_value(duckdb_vector vector, duckdb_value value)`
/// (capi/data_chunk-c.cpp): the vector becomes a constant vector of the value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_reference_value(vector: DuckdbVector, value: DuckdbValue) {
    // SAFETY: both are NULL or live handles.
    let (Some(vector), Some(value)) = (unsafe { (vector_mut(vector), value_ref(value)) }) else {
        return;
    };
    if let Ok(constant) = Vector::constant(value) {
        *vector = constant;
    }
}

/// `void duckdb_vector_reference_vector(duckdb_vector to_vector, duckdb_vector from_vector)`
/// (capi/data_chunk-c.cpp): `to_vector` shares the buffers of `from_vector`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn duckdb_vector_reference_vector(to_vector: DuckdbVector, from_vector: DuckdbVector) {
    if to_vector == from_vector {
        return;
    }
    // SAFETY: both are NULL or live, distinct handles.
    let (Some(to), Some(from)) = (unsafe { (vector_mut(to_vector), vector_mut(from_vector)) }) else {
        return;
    };
    *to = from.clone();
}
