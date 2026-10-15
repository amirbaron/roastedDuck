//! The heap encoding of list and array elements (DuckDB's "within collection"
//! scatter/gather).
//!
//! The `n` elements of a list value are stored as one element block:
//!
//! ```text
//! [validity: ceil(n / 8) bytes][fixed part][variable part]
//! ```
//!
//! - fixed-width types: `n` values,
//! - VARCHAR: `n` `u32` lengths, then the bytes of the valid strings,
//! - STRUCT: the struct validity only, followed by one element block per field,
//! - LIST / ARRAY: `n` `u64` lengths, then a single element block holding the elements of
//!   all valid nested lists, concatenated.
//!
//! An empty list has no element block at all. A top-level list column stores a `u64`
//! length followed by the element block.

use rustdb_common::{Error, Result};
use rustdb_types::{ListEntry, PhysicalType, StringT, TypeInfo, Vector};

use crate::format::{FormatKind, VectorFormat};
use crate::layout::validity_bytes;
use crate::row::{copy, is_valid, load, set_all_valid, set_invalid, store};

/// Logical rows of the child of `format` holding the elements of the valid lists among
/// `rows`.
pub(crate) fn child_rows(format: &VectorFormat<'_>, rows: &[usize], out: &mut Vec<usize>) {
    out.clear();
    for &row in rows {
        let slot = format.slot(row);
        if format.slot_is_valid(slot) {
            let (offset, length) = format.list_entry(slot);
            out.extend(offset..offset + length);
        }
    }
}

/// Bytes of the element block of elements `rows` of `format`.
pub(crate) fn elements_size(format: &VectorFormat<'_>, rows: &[usize]) -> usize {
    let n = rows.len();
    if n == 0 {
        return 0;
    }
    let mut size = validity_bytes(n);
    match format.kind {
        FormatKind::Fixed(width) => size += n * width,
        FormatKind::Varchar => {
            size += n * size_of::<u32>();
            for &row in rows {
                let slot = format.slot(row);
                if format.slot_is_valid(slot) {
                    size += format.string(slot).len();
                }
            }
        }
        FormatKind::Struct => {
            for child in &format.children {
                size += elements_size(child, rows);
            }
        }
        FormatKind::List | FormatKind::Array(_) => {
            size += n * size_of::<u64>();
            let mut nested = Vec::new();
            child_rows(format, rows, &mut nested);
            size += elements_size(&format.children[0], &nested);
        }
    }
    size
}

/// Writes the element block of elements `rows` of `format` at `*heap`, advancing it.
///
/// # Safety
/// `*heap` must be writable for [`elements_size`] bytes.
pub(crate) unsafe fn write_elements(format: &VectorFormat<'_>, rows: &[usize], heap: &mut *mut u8) {
    let n = rows.len();
    if n == 0 {
        return;
    }
    let validity = *heap;
    // SAFETY (whole function): every write stays within the element block, whose size the
    // caller reserved with `elements_size`, which mirrors this function exactly.
    unsafe {
        set_all_valid(validity, validity_bytes(n));
        *heap = heap.add(validity_bytes(n));
        if !format.all_valid() {
            for (i, &row) in rows.iter().enumerate() {
                if !format.row_is_valid(row) {
                    set_invalid(validity, i);
                }
            }
        }
        match format.kind {
            FormatKind::Fixed(width) => {
                let data = *heap;
                *heap = heap.add(n * width);
                for (i, &row) in rows.iter().enumerate() {
                    let slot = format.slot(row);
                    if format.slot_is_valid(slot) {
                        copy(format.slot_ptr(slot, width), data.add(i * width), width);
                    }
                }
            }
            FormatKind::Varchar => {
                let lengths = *heap;
                *heap = heap.add(n * size_of::<u32>());
                for (i, &row) in rows.iter().enumerate() {
                    let slot = format.slot(row);
                    if format.slot_is_valid(slot) {
                        let bytes = format.string_bytes(slot);
                        store(bytes.len() as u32, lengths.add(i * size_of::<u32>()));
                        copy(bytes.as_ptr(), *heap, bytes.len());
                        *heap = heap.add(bytes.len());
                    }
                }
            }
            FormatKind::Struct => {
                for child in &format.children {
                    write_elements(child, rows, heap);
                }
            }
            FormatKind::List | FormatKind::Array(_) => {
                let lengths = *heap;
                *heap = heap.add(n * size_of::<u64>());
                for (i, &row) in rows.iter().enumerate() {
                    let slot = format.slot(row);
                    if format.slot_is_valid(slot) {
                        let (_, length) = format.list_entry(slot);
                        store(length as u64, lengths.add(i * size_of::<u64>()));
                    }
                }
                let mut nested = Vec::new();
                child_rows(format, rows, &mut nested);
                write_elements(&format.children[0], &nested, heap);
            }
        }
    }
}

/// Where decoded elements go in the target vector.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Targets<'a> {
    /// Element `i` goes to row `start + i`.
    Contiguous(usize),
    /// Element `i` goes to row `rows[i]`.
    Rows(&'a [usize]),
}

impl Targets<'_> {
    #[inline]
    fn at(self, i: usize) -> usize {
        match self {
            Self::Contiguous(start) => start + i,
            Self::Rows(rows) => rows[i],
        }
    }

    /// One past the largest target row of `n` elements.
    fn end(self, n: usize) -> usize {
        match self {
            Self::Contiguous(start) => start + n,
            Self::Rows(rows) => rows[..n].iter().max().map_or(0, |&max| max + 1),
        }
    }
}

/// Grows `vector` to hold at least `rows` rows, at least doubling.
pub(crate) fn ensure_capacity(vector: &mut Vector, rows: usize) {
    let capacity = vector.row_capacity();
    if rows > capacity {
        vector.resize(rows.max(capacity * 2));
    }
}

fn type_error(vector: &Vector) -> Error {
    Error::not_implemented(format!(
        "Unsupported type for row gather: {}",
        vector.logical_type()
    ))
}

/// Decodes an element block of `n` elements at `*heap` into `target`, advancing `*heap`.
/// Non-inlined strings point into the heap: the caller keeps it alive.
///
/// # Safety
/// `*heap` must point to an element block of `n` elements written for `target`'s type.
pub(crate) unsafe fn read_elements(
    n: usize,
    heap: &mut *const u8,
    target: &mut Vector,
    targets: Targets<'_>,
) -> Result<()> {
    if n == 0 {
        return Ok(());
    }
    ensure_capacity(target, targets.end(n));
    let validity = *heap;
    // SAFETY (whole function): reads stay within the element block, whose shape mirrors
    // `write_elements` for the same type.
    unsafe {
        *heap = heap.add(validity_bytes(n));
        let mut nulls = Vec::new();
        let physical = target.logical_type().physical_type();
        match physical {
            PhysicalType::Varchar => {
                let lengths = *heap;
                *heap = heap.add(n * size_of::<u32>());
                let values = target.values_mut::<StringT>();
                for i in 0..n {
                    let row = targets.at(i);
                    if is_valid(validity, i) {
                        let length = load::<u32>(lengths.add(i * size_of::<u32>())) as usize;
                        let bytes = std::slice::from_raw_parts(*heap, length);
                        values[row] = StringT::from_raw(bytes);
                        *heap = heap.add(length);
                    } else {
                        nulls.push(row);
                    }
                }
            }
            PhysicalType::Struct => {
                for i in 0..n {
                    if !is_valid(validity, i) {
                        nulls.push(targets.at(i));
                    }
                }
                let children = target
                    .struct_children_mut()
                    .ok_or_else(|| Error::internal("struct without children"))?;
                for child in children {
                    read_elements(n, heap, child, targets)?;
                }
            }
            PhysicalType::List => {
                let lengths = *heap;
                *heap = heap.add(n * size_of::<u64>());
                let start = target.list_child().map_or(0, |(_, size)| size);
                let mut child_size = start;
                {
                    let entries = target.values_mut::<ListEntry>();
                    for i in 0..n {
                        let row = targets.at(i);
                        if is_valid(validity, i) {
                            let length = load::<u64>(lengths.add(i * size_of::<u64>()));
                            entries[row] = ListEntry {
                                offset: child_size as u64,
                                length,
                            };
                            child_size += length as usize;
                        } else {
                            nulls.push(row);
                        }
                    }
                }
                let error = type_error(target);
                let child = target.list_child_mut().ok_or(error)?;
                read_elements(child_size - start, heap, child, Targets::Contiguous(start))?;
                target.set_list_size(child_size);
            }
            PhysicalType::Array => {
                *heap = heap.add(n * size_of::<u64>());
                let size = match target.logical_type().info() {
                    Some(TypeInfo::Array(rustdb_types::ArrayTypeInfo { size, .. })) => *size as usize,
                    _ => return Err(type_error(target)),
                };
                let mut rows = Vec::new();
                for i in 0..n {
                    let row = targets.at(i);
                    if is_valid(validity, i) {
                        rows.extend(row * size..(row + 1) * size);
                    } else {
                        nulls.push(row);
                    }
                }
                let error = type_error(target);
                let child = target.array_child_mut().ok_or(error)?;
                let count = rows.len();
                read_elements(count, heap, child, Targets::Rows(&rows))?;
            }
            PhysicalType::Bit | PhysicalType::Unknown | PhysicalType::Invalid => {
                return Err(type_error(target));
            }
            fixed => {
                let width = fixed.size();
                let data = *heap;
                *heap = heap.add(n * width);
                let bytes = target.data_bytes_mut();
                for i in 0..n {
                    let row = targets.at(i);
                    if is_valid(validity, i) {
                        copy(data.add(i * width), bytes.as_mut_ptr().add(row * width), width);
                    } else {
                        nulls.push(row);
                    }
                }
            }
        }
        if !nulls.is_empty() {
            let capacity = target.row_capacity();
            let validity = target.validity_mut();
            for row in nulls {
                validity.set_invalid(row, capacity);
            }
        }
    }
    Ok(())
}
