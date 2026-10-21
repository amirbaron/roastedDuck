//! Writing vectors into rows (DuckDB's `TupleDataCollection::Scatter` and
//! `ComputeHeapSizes`).

use rustdb_common::{Error, Result};
use rustdb_types::StringT;

use crate::format::{ChunkFormat, FormatKind, SlotMap, VectorFormat};
use crate::heap::{child_rows, elements_size, write_elements};
use crate::layout::{HEAP_SIZE_WIDTH, TupleDataLayout, validity_bytes};
use crate::row::{RowPtr, copy, set_all_valid, set_invalid, store};

/// The input row of output position `i`: `sel[i]`, or `i` without a selection.
#[inline]
pub(crate) fn input_row(sel: Option<&[u32]>, i: usize) -> usize {
    sel.map_or(i, |sel| sel[i] as usize)
}

/// Computes the heap bytes each appended row needs (`ComputeHeapSizes`).
pub(crate) fn compute_heap_sizes(
    layout: &TupleDataLayout,
    formats: &ChunkFormat<'_>,
    sel: Option<&[u32]>,
    count: usize,
    heap_sizes: &mut [usize],
) -> Result<()> {
    heap_sizes[..count].fill(0);
    let mut scratch = Vec::new();
    for &col in layout.variable_columns() {
        add_heap_sizes(&formats.columns[col], sel, count, heap_sizes, &mut scratch);
    }
    if let Some(&too_large) = heap_sizes[..count].iter().find(|&&size| size > u32::MAX as usize) {
        return Err(Error::not_implemented(format!(
            "Rows with more than 4 GiB of variable-size data are not supported (row needs {too_large} bytes)"
        )));
    }
    Ok(())
}

fn add_heap_sizes(
    format: &VectorFormat<'_>,
    sel: Option<&[u32]>,
    count: usize,
    heap_sizes: &mut [usize],
    scratch: &mut Vec<usize>,
) {
    match format.kind {
        FormatKind::Fixed(_) => {}
        FormatKind::Varchar => {
            for (i, size) in heap_sizes[..count].iter_mut().enumerate() {
                let slot = format.slot(input_row(sel, i));
                if format.slot_is_valid(slot) {
                    let string = format.string(slot);
                    if !string.is_inlined() {
                        *size += string.len();
                    }
                }
            }
        }
        FormatKind::Struct => {
            for child in &format.children {
                add_heap_sizes(child, sel, count, heap_sizes, scratch);
            }
        }
        FormatKind::List | FormatKind::Array(_) => {
            for (i, size) in heap_sizes[..count].iter_mut().enumerate() {
                let row = input_row(sel, i);
                if !format.row_is_valid(row) {
                    continue;
                }
                child_rows(format, &[row], scratch);
                *size += size_of::<u64>() + elements_size(&format.children[0], scratch);
            }
        }
    }
}

/// Writes the rows of `formats` selected by `sel` into `rows`, and their variable-size
/// data at `heaps` (advanced past what was written).
///
/// # Safety
/// `rows[..count]` must point to writable rows of `layout`, and each `heaps[i]` to at
/// least `heap_sizes[i]` writable bytes, as computed by [`compute_heap_sizes`].
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors DuckDB's Scatter; all inputs are needed"
)]
pub(crate) unsafe fn scatter(
    layout: &TupleDataLayout,
    formats: &ChunkFormat<'_>,
    column_ids: &[usize],
    sel: Option<&[u32]>,
    count: usize,
    rows: &[RowPtr],
    heaps: &mut [RowPtr],
    heap_sizes: &[usize],
) {
    // SAFETY (whole function): guaranteed by the caller.
    unsafe {
        if !layout.all_valid() {
            let bytes = validity_bytes(layout.column_count());
            for row in &rows[..count] {
                set_all_valid(row.as_ptr(), bytes);
            }
        }
        if !layout.all_constant() {
            let offset = layout.heap_size_offset();
            for (row, &size) in rows[..count].iter().zip(heap_sizes) {
                store(size as u32, row.as_ptr().add(offset));
            }
        }
        for &col in column_ids {
            scatter_column(layout, col, &formats.columns[col], sel, count, rows, 0, heaps);
        }
    }
    debug_assert_eq!(HEAP_SIZE_WIDTH, size_of::<u32>());
}

/// Writes column `col` of rows that start `base` bytes into each row pointer (non-zero
/// for STRUCT fields).
///
/// # Safety
/// As for [`scatter`].
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors DuckDB's scatter function signature"
)]
pub(crate) unsafe fn scatter_column(
    layout: &TupleDataLayout,
    col: usize,
    format: &VectorFormat<'_>,
    sel: Option<&[u32]>,
    count: usize,
    rows: &[RowPtr],
    base: usize,
    heaps: &mut [RowPtr],
) {
    let offset = base + layout.offsets()[col];
    // SAFETY (whole function): rows and heaps are writable as the caller guarantees.
    unsafe {
        match format.kind {
            FormatKind::Fixed(width) => scatter_fixed(format, width, sel, count, rows, base, col, offset),
            FormatKind::Varchar => {
                for (i, row) in rows[..count].iter().enumerate() {
                    let slot = format.slot(input_row(sel, i));
                    let target = row.as_ptr().add(offset);
                    if format.slot_is_valid(slot) {
                        let string = format.string(slot);
                        if string.is_inlined() {
                            store(string, target);
                        } else {
                            let bytes = format.string_bytes(slot);
                            let heap = heaps[i].as_ptr();
                            copy(bytes.as_ptr(), heap, bytes.len());
                            // Length and prefix, then the pointer to the copy in the heap.
                            copy((&raw const string).cast::<u8>(), target, 8);
                            store(heap as u64, target.add(8));
                            heaps[i] = heaps[i].offset(bytes.len());
                        }
                    } else {
                        store(StringT::default(), target);
                        set_invalid(row.as_ptr().add(base), col);
                    }
                }
            }
            FormatKind::Struct => {
                let Some(struct_layout) = layout.struct_layout(col) else {
                    return;
                };
                for (i, row) in rows[..count].iter().enumerate() {
                    if !format.row_is_valid(input_row(sel, i)) {
                        set_invalid(row.as_ptr().add(base), col);
                    }
                    set_all_valid(
                        row.as_ptr().add(offset),
                        validity_bytes(struct_layout.column_count()),
                    );
                }
                for (child_col, child) in format.children.iter().enumerate() {
                    scatter_column(struct_layout, child_col, child, sel, count, rows, offset, heaps);
                }
            }
            FormatKind::List | FormatKind::Array(_) => {
                let mut elements = Vec::new();
                for (i, row) in rows[..count].iter().enumerate() {
                    let input = input_row(sel, i);
                    if !format.row_is_valid(input) {
                        set_invalid(row.as_ptr().add(base), col);
                        continue;
                    }
                    let mut heap = heaps[i].as_ptr();
                    store(heap as u64, row.as_ptr().add(offset));
                    let (_, length) = format.list_entry(format.slot(input));
                    store(length as u64, heap);
                    heap = heap.add(size_of::<u64>());
                    child_rows(format, &[input], &mut elements);
                    write_elements(&format.children[0], &elements, &mut heap);
                    heaps[i] = RowPtr::from_ptr(heap);
                }
            }
        }
    }
}

/// Fixed-width scatter, specialised for the common flat, NULL-free case.
///
/// # Safety
/// As for [`scatter`].
#[allow(clippy::too_many_arguments, reason = "hot loop kept free of indirection")]
unsafe fn scatter_fixed(
    format: &VectorFormat<'_>,
    width: usize,
    sel: Option<&[u32]>,
    count: usize,
    rows: &[RowPtr],
    base: usize,
    col: usize,
    offset: usize,
) {
    #[allow(
        clippy::inline_always,
        reason = "monomorphized hot loop; inlining removes the per-width call"
    )]
    #[inline(always)]
    unsafe fn run<const W: usize>(
        format: &VectorFormat<'_>,
        sel: Option<&[u32]>,
        count: usize,
        rows: &[RowPtr],
        base: usize,
        col: usize,
        offset: usize,
    ) {
        // SAFETY (whole function): rows are writable rows of the layout.
        unsafe {
            if format.all_valid() {
                // `TupleDataTemplatedScatter` without NULLs: no validity per row, and the
                // usual selections resolved outside the loop.
                match (sel, &format.sel) {
                    (None, SlotMap::Identity) => {
                        for (i, row) in rows[..count].iter().enumerate() {
                            copy(format.slot_ptr(i, W), row.as_ptr().add(offset), W);
                        }
                    }
                    (Some(sel), SlotMap::Identity) => {
                        for (&slot, row) in sel[..count].iter().zip(&rows[..count]) {
                            copy(format.slot_ptr(slot as usize, W), row.as_ptr().add(offset), W);
                        }
                    }
                    _ => {
                        for (i, row) in rows[..count].iter().enumerate() {
                            let slot = format.slot(input_row(sel, i));
                            copy(format.slot_ptr(slot, W), row.as_ptr().add(offset), W);
                        }
                    }
                }
                return;
            }
            for (i, row) in rows[..count].iter().enumerate() {
                let slot = format.slot(input_row(sel, i));
                let target = row.as_ptr().add(offset);
                if format.slot_is_valid(slot) {
                    copy(format.slot_ptr(slot, W), target, W);
                } else {
                    std::ptr::write_bytes(target, 0, W);
                    set_invalid(row.as_ptr().add(base), col);
                }
            }
        }
    }
    // SAFETY: forwarded from the caller.
    unsafe {
        match width {
            1 => run::<1>(format, sel, count, rows, base, col, offset),
            2 => run::<2>(format, sel, count, rows, base, col, offset),
            4 => run::<4>(format, sel, count, rows, base, col, offset),
            8 => run::<8>(format, sel, count, rows, base, col, offset),
            16 => run::<16>(format, sel, count, rows, base, col, offset),
            _ => {
                for (i, row) in rows[..count].iter().enumerate() {
                    let slot = format.slot(input_row(sel, i));
                    let target = row.as_ptr().add(offset);
                    if format.slot_is_valid(slot) {
                        copy(format.slot_ptr(slot, width), target, width);
                    } else {
                        set_invalid(row.as_ptr().add(base), col);
                    }
                }
            }
        }
    }
}
