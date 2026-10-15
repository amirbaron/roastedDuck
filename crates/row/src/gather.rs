//! Reading rows back into vectors (DuckDB's `TupleDataCollection::Gather`).
//!
//! Gathered strings that do not fit inline point into the row heap instead of being
//! copied; the target vector keeps the heap blocks pinned through a keep-alive handle.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_types::{ListEntry, PhysicalType, TypeInfo, Vector};

use crate::heap::{Targets, ensure_capacity, read_elements};
use crate::layout::TupleDataLayout;
use crate::row::{RowPtr, copy, is_valid, load};

/// An owner of pinned heap blocks attached to gathered vectors so that strings pointing
/// into the heap stay valid.
pub type HeapKeepAlive = Arc<dyn Any + Send + Sync>;

#[inline]
fn index(sel: Option<&[u32]>, i: usize) -> usize {
    sel.map_or(i, |sel| sel[i] as usize)
}

/// Whether a column of type `ty` may hold data in the heap.
pub(crate) fn has_heap_data(ty: &rustdb_types::LogicalType) -> bool {
    matches!(
        ty.physical_type(),
        PhysicalType::Varchar | PhysicalType::List | PhysicalType::Array | PhysicalType::Struct
    )
}

/// Gathers column `col` of `count` rows into `target`: row `rows[scan_sel[i]]` goes to
/// `target` row `target_sel[i]`. `target` must be a flat vector of the column's type with
/// valid rows at the target positions (they are only ever marked NULL).
///
/// # Safety
/// The selected rows must be rows of `layout` in pinned blocks, with valid heap pointers.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors DuckDB's gather function signature"
)]
pub(crate) unsafe fn gather_column(
    layout: &TupleDataLayout,
    col: usize,
    rows: &[RowPtr],
    scan_sel: Option<&[u32]>,
    count: usize,
    base: usize,
    target: &mut Vector,
    target_sel: Option<&[u32]>,
) -> Result<()> {
    let offset = base + layout.offsets()[col];
    let check_validity = !layout.all_valid();
    let end = (0..count).map(|i| index(target_sel, i) + 1).max().unwrap_or(0);
    ensure_capacity(target, end);
    let mut nulls = Vec::new();
    // SAFETY (whole function): guaranteed by the caller.
    unsafe {
        match target.logical_type().physical_type() {
            PhysicalType::Struct => {
                let Some(struct_layout) = layout.struct_layout(col) else {
                    return Err(Error::internal("struct column without struct layout"));
                };
                if check_validity {
                    for i in 0..count {
                        let row = rows[index(scan_sel, i)];
                        if !is_valid(row.as_ptr().add(base), col) {
                            nulls.push(index(target_sel, i));
                        }
                    }
                }
                let children = target
                    .struct_children_mut()
                    .ok_or_else(|| Error::internal("struct vector without children"))?;
                for (child_col, child) in children.iter_mut().enumerate() {
                    gather_column(
                        struct_layout,
                        child_col,
                        rows,
                        scan_sel,
                        count,
                        offset,
                        child,
                        target_sel,
                    )?;
                }
            }
            PhysicalType::List => {
                let mut child_size = target.list_child().map_or(0, |(_, size)| size);
                for i in 0..count {
                    let row = rows[index(scan_sel, i)];
                    let target_row = index(target_sel, i);
                    if check_validity && !is_valid(row.as_ptr().add(base), col) {
                        nulls.push(target_row);
                        continue;
                    }
                    let mut heap = load::<u64>(row.as_ptr().add(offset)) as usize as *const u8;
                    let length = load::<u64>(heap) as usize;
                    heap = heap.add(size_of::<u64>());
                    target.values_mut::<ListEntry>()[target_row] = ListEntry {
                        offset: child_size as u64,
                        length: length as u64,
                    };
                    let child = target
                        .list_child_mut()
                        .ok_or_else(|| Error::internal("list vector without child"))?;
                    read_elements(length, &mut heap, child, Targets::Contiguous(child_size))?;
                    child_size += length;
                }
                target.set_list_size(child_size);
            }
            PhysicalType::Array => {
                let size = match target.logical_type().info() {
                    Some(TypeInfo::Array(rustdb_types::ArrayTypeInfo { size, .. })) => *size as usize,
                    _ => return Err(Error::internal("ARRAY type without size")),
                };
                for i in 0..count {
                    let row = rows[index(scan_sel, i)];
                    let target_row = index(target_sel, i);
                    if check_validity && !is_valid(row.as_ptr().add(base), col) {
                        nulls.push(target_row);
                        continue;
                    }
                    let mut heap = load::<u64>(row.as_ptr().add(offset)) as usize as *const u8;
                    heap = heap.add(size_of::<u64>());
                    let child = target
                        .array_child_mut()
                        .ok_or_else(|| Error::internal("array vector without child"))?;
                    read_elements(size, &mut heap, child, Targets::Contiguous(target_row * size))?;
                }
            }
            PhysicalType::Bit | PhysicalType::Unknown | PhysicalType::Invalid => {
                return Err(Error::not_implemented(format!(
                    "Unsupported type for row gather: {}",
                    target.logical_type()
                )));
            }
            // Fixed-width values and `string_t`s (which already point into the heap) are
            // copied as raw bytes.
            fixed => gather_fixed(
                fixed.size(),
                rows,
                scan_sel,
                count,
                base,
                col,
                offset,
                check_validity,
                target,
                target_sel,
                &mut nulls,
            ),
        }
    }
    if !nulls.is_empty() {
        let capacity = target.row_capacity();
        let validity = target.validity_mut();
        for row in nulls {
            validity.set_invalid(row, capacity);
        }
    }
    Ok(())
}

/// # Safety
/// As for [`gather_column`].
#[allow(clippy::too_many_arguments, reason = "hot loop kept free of indirection")]
unsafe fn gather_fixed(
    width: usize,
    rows: &[RowPtr],
    scan_sel: Option<&[u32]>,
    count: usize,
    base: usize,
    col: usize,
    offset: usize,
    check_validity: bool,
    target: &mut Vector,
    target_sel: Option<&[u32]>,
    nulls: &mut Vec<usize>,
) {
    #[allow(
        clippy::inline_always,
        reason = "monomorphized hot loop; inlining removes the per-width call"
    )]
    #[inline(always)]
    #[allow(clippy::too_many_arguments, reason = "hot loop kept free of indirection")]
    unsafe fn run<const W: usize>(
        rows: &[RowPtr],
        scan_sel: Option<&[u32]>,
        count: usize,
        base: usize,
        col: usize,
        offset: usize,
        check_validity: bool,
        data: *mut u8,
        target_sel: Option<&[u32]>,
        nulls: &mut Vec<usize>,
    ) {
        // SAFETY (whole function): rows are valid rows; `data` holds the target rows.
        unsafe {
            if scan_sel.is_none() && target_sel.is_none() && !check_validity {
                for (i, row) in rows[..count].iter().enumerate() {
                    copy(row.as_ptr().add(offset), data.add(i * W), W);
                }
                return;
            }
            for i in 0..count {
                let row = rows[index(scan_sel, i)].as_ptr();
                let target_row = index(target_sel, i);
                copy(row.add(offset), data.add(target_row * W), W);
                if check_validity && !is_valid(row.add(base), col) {
                    nulls.push(target_row);
                }
            }
        }
    }
    let data = target.data_bytes_mut().as_mut_ptr();
    // SAFETY: forwarded from the caller; the target holds every target row.
    unsafe {
        match width {
            1 => run::<1>(
                rows,
                scan_sel,
                count,
                base,
                col,
                offset,
                check_validity,
                data,
                target_sel,
                nulls,
            ),
            2 => run::<2>(
                rows,
                scan_sel,
                count,
                base,
                col,
                offset,
                check_validity,
                data,
                target_sel,
                nulls,
            ),
            4 => run::<4>(
                rows,
                scan_sel,
                count,
                base,
                col,
                offset,
                check_validity,
                data,
                target_sel,
                nulls,
            ),
            8 => run::<8>(
                rows,
                scan_sel,
                count,
                base,
                col,
                offset,
                check_validity,
                data,
                target_sel,
                nulls,
            ),
            16 => run::<16>(
                rows,
                scan_sel,
                count,
                base,
                col,
                offset,
                check_validity,
                data,
                target_sel,
                nulls,
            ),
            _ => {
                for i in 0..count {
                    let row = rows[index(scan_sel, i)].as_ptr();
                    let target_row = index(target_sel, i);
                    copy(row.add(offset), data.add(target_row * width), width);
                    if check_validity && !is_valid(row.add(base), col) {
                        nulls.push(target_row);
                    }
                }
            }
        }
    }
}
