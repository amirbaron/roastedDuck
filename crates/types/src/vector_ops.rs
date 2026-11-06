//! Copying rows between vectors of any type (DuckDB's `VectorOperations::Copy` and
//! `ListVector::Append`), used to assemble nested values from their children.

use crate::{ListEntry, PhysicalType, StringT, Vector, VectorKind};
use rustdb_common::{Error, Result};

/// Copies rows `src_start..src_start + count` of `src` into `dst` starting at row
/// `dst_start`; `dst` must be flat and large enough.
pub fn copy_rows(
    src: &Vector,
    src_start: usize,
    count: usize,
    dst: &mut Vector,
    dst_start: usize,
) -> Result<()> {
    if count == 0 {
        return Ok(());
    }
    if src.kind() != VectorKind::Flat {
        let flat = src.flattened(src_start + count);
        return copy_rows(&flat, src_start, count, dst, dst_start);
    }
    if dst.capacity() < dst_start + count && dst.logical_type().physical_type() != PhysicalType::Struct {
        dst.resize(dst_start + count);
    }
    // Validity first: nested copies below may set child NULLs themselves.
    if !src.validity().is_all_valid() {
        let capacity = dst.capacity().max(dst_start + count);
        for row in 0..count {
            if src.validity().is_valid(src_start + row) {
                dst.validity_mut().set_valid(dst_start + row);
            } else {
                dst.validity_mut().set_invalid(dst_start + row, capacity);
            }
        }
    } else if !dst.validity().is_all_valid() {
        for row in 0..count {
            dst.validity_mut().set_valid(dst_start + row);
        }
    }
    match src.logical_type().physical_type() {
        PhysicalType::Varchar => {
            let values = &src.values::<StringT>()[src_start..src_start + count];
            dst.values_mut::<StringT>()[dst_start..dst_start + count].copy_from_slice(values);
            dst.add_heap_references(src);
        }
        PhysicalType::Struct => {
            if dst.row_capacity() < dst_start + count {
                dst.resize(dst_start + count);
            }
            let src_children = src
                .struct_children()
                .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
            let dst_children = dst
                .struct_children_mut()
                .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
            for (src_child, dst_child) in src_children.iter().zip(dst_children) {
                copy_rows(src_child, src_start, count, dst_child, dst_start)?;
            }
        }
        PhysicalType::List => {
            let (src_child, _) = src
                .list_child()
                .ok_or_else(|| Error::internal("LIST vector without a child"))?;
            let entries: Vec<ListEntry> = src.values::<ListEntry>()[src_start..src_start + count].to_vec();
            let mut size = dst.list_child().map_or(0, |(_, size)| size);
            let total: usize = entries
                .iter()
                .enumerate()
                .filter(|(row, _)| src.validity().is_valid(src_start + row))
                .map(|(_, entry)| entry.length as usize)
                .sum();
            {
                let dst_child = dst
                    .list_child_mut()
                    .ok_or_else(|| Error::internal("LIST vector without a child"))?;
                if dst_child.capacity() < size + total {
                    dst_child.resize((size + total).max(dst_child.capacity() * 2));
                }
            }
            let mut new_entries = Vec::with_capacity(count);
            for (row, entry) in entries.iter().enumerate() {
                if !src.validity().is_valid(src_start + row) {
                    new_entries.push(ListEntry::default());
                    continue;
                }
                let length = entry.length as usize;
                let dst_child = dst
                    .list_child_mut()
                    .ok_or_else(|| Error::internal("LIST vector without a child"))?;
                copy_rows(src_child, entry.offset as usize, length, dst_child, size)?;
                new_entries.push(ListEntry::new(size, length));
                size += length;
            }
            dst.set_list_size(size);
            dst.values_mut::<ListEntry>()[dst_start..dst_start + count].copy_from_slice(&new_entries);
        }
        PhysicalType::Array => {
            let size = src
                .logical_type()
                .array_child()
                .map_or(0, |(_, size)| size as usize);
            let src_child = src
                .array_child()
                .ok_or_else(|| Error::internal("ARRAY vector without a child"))?
                .clone();
            let dst_child = dst
                .array_child_mut()
                .ok_or_else(|| Error::internal("ARRAY vector without a child"))?;
            copy_rows(
                &src_child,
                src_start * size,
                count * size,
                dst_child,
                dst_start * size,
            )?;
        }
        physical => {
            let width = physical.size();
            let bytes = &src.data_bytes()[src_start * width..(src_start + count) * width];
            dst.data_bytes_mut()[dst_start * width..(dst_start + count) * width].copy_from_slice(bytes);
        }
    }
    Ok(())
}

/// Appends the first `count` rows of `src` to the child of the LIST vector `list`
/// (`ListVector::Append`).
pub fn list_append(list: &mut Vector, src: &Vector, count: usize) -> Result<()> {
    let size = list.list_child().map_or(0, |(_, size)| size);
    let child = list
        .list_child_mut()
        .ok_or_else(|| Error::internal("LIST vector without a child"))?;
    if child.capacity() < size + count {
        child.resize((size + count).max(child.capacity() * 2));
    }
    copy_rows(src, 0, count, child, size)?;
    list.set_list_size(size + count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LogicalType, Value};

    #[test]
    fn copies_nested_rows() {
        let ty = LogicalType::list(LogicalType::struct_type(vec![(
            "a".into(),
            LogicalType::VARCHAR,
        )]));
        let value = |text: &str| {
            Value::list(
                LogicalType::struct_type(vec![("a".into(), LogicalType::VARCHAR)]),
                vec![Value::struct_value(vec![("a".into(), Value::varchar(text))])],
            )
        };
        let mut src = Vector::flat(ty.clone(), 3);
        src.set_value(0, &value("a string longer than inline")).unwrap();
        src.set_value(1, &Value::null(ty.clone())).unwrap();
        src.set_value(2, &value("x")).unwrap();
        let mut dst = Vector::flat(ty.clone(), 4);
        copy_rows(&src, 0, 3, &mut dst, 1).unwrap();
        assert_eq!(dst.get_value(1), value("a string longer than inline"));
        assert!(dst.get_value(2).is_null());
        assert_eq!(dst.get_value(3), value("x"));
    }
}
