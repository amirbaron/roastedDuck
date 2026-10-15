//! Raw and nested access used by row-format code (scatter/gather, sort key decoding).
//!
//! These methods expose a vector's storage below the typed [`Vector::values`] API: the
//! raw slot bytes (any fixed-width physical type, including INTERVAL), the children of
//! nested vectors for in-place construction, and a way to keep foreign memory alive for
//! strings that point into it.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use super::{Aux, Repr, StringHeap, Vector};
use crate::TypeInfo;

/// Shared ownership of memory that `StringT` values of a vector point into.
#[derive(Clone)]
pub(super) struct KeepAlive(
    #[expect(dead_code, reason = "held only to keep the owner alive")] Arc<dyn Any + Send + Sync>,
);

impl fmt::Debug for KeepAlive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeepAlive")
    }
}

impl Vector {
    /// The raw bytes of the data buffer, `capacity() * width` long.
    ///
    /// Dictionary vectors expose their child's buffer (index it through [`Vector::sel`]);
    /// sequence vectors have no buffer and return an empty slice.
    pub fn data_bytes(&self) -> &[u8] {
        if matches!(self.repr, Repr::Sequence { .. }) {
            return &[];
        }
        let data = &self.storage().data;
        // SAFETY: the buffer owns `len` initialised (zeroed or written) bytes and lives as
        // long as `self`.
        unsafe { std::slice::from_raw_parts(data.as_ptr(), data.len()) }
    }

    /// Mutable raw bytes of the data buffer. Dictionaries and sequences are flattened
    /// first; a shared buffer is copied (copy-on-write).
    pub fn data_bytes_mut(&mut self) -> &mut [u8] {
        let buffer = Arc::make_mut(&mut self.storage_mut().data);
        let len = buffer.len();
        // SAFETY: `make_mut` guarantees unique access to the `len` owned bytes.
        unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr(), len) }
    }

    /// Rows the vector can hold. Unlike [`Vector::capacity`] this also covers STRUCT and
    /// ARRAY vectors, whose own data buffer is empty, through their children.
    pub fn row_capacity(&self) -> usize {
        match &self.storage().aux {
            Aux::Struct(children) => children.iter().map(Vector::row_capacity).min().unwrap_or(0),
            Aux::Array(child) => match self.ty.info() {
                Some(TypeInfo::Array(info)) if info.size > 0 => child.row_capacity() / info.size as usize,
                _ => 0,
            },
            _ => self.capacity(),
        }
    }

    /// Sets how many rows of a LIST/MAP vector's child are in use.
    pub fn set_list_size(&mut self, size: usize) {
        if let Aux::List { size: used, .. } = &mut self.storage_mut().aux {
            *used = size;
        }
    }

    /// Keeps `owner` alive as long as this vector (or any clone of it) exists.
    ///
    /// Used when `StringT` values are written that point into memory owned elsewhere
    /// (e.g. a pinned buffer-managed block), so the strings stay valid without being
    /// copied. Applies to every string-carrying descendant of nested vectors.
    pub fn add_keep_alive(&mut self, owner: Arc<dyn Any + Send + Sync>) {
        match &mut self.storage_mut().aux {
            Aux::Strings(heaps) => {
                let heap = StringHeap {
                    keep_alive: Some(KeepAlive(owner)),
                    ..StringHeap::default()
                };
                // Keep our own appendable heap last.
                let own = heaps.pop();
                heaps.push(Arc::new(heap));
                heaps.extend(own);
            }
            Aux::List { child, .. } | Aux::Array(child) => child.add_keep_alive(owner),
            Aux::Struct(children) => {
                for child in children {
                    child.add_keep_alive(Arc::clone(&owner));
                }
            }
            Aux::None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ListEntry, LogicalType, StringT, Value};

    #[test]
    fn resize_keeps_values_and_grows_struct_children() {
        let ty = LogicalType::struct_type(vec![("a".into(), LogicalType::INTEGER)]);
        let mut vector = Vector::flat(ty, 2);
        vector.struct_children_mut().unwrap()[0].values_mut::<i32>()[1] = 7;
        vector.resize(100);
        let child = &vector.struct_children().unwrap()[0];
        assert!(child.capacity() >= 100);
        assert_eq!(child.values::<i32>()[1], 7);
    }

    #[test]
    fn list_children_are_writable_in_place() {
        let mut list = Vector::flat(LogicalType::list(LogicalType::BIGINT), 2);
        let child = list.list_child_mut().unwrap();
        child.resize(3);
        child.values_mut::<i64>()[..3].copy_from_slice(&[1, 2, 3]);
        list.set_list_size(3);
        list.values_mut::<ListEntry>()[0] = ListEntry { offset: 0, length: 2 };
        list.values_mut::<ListEntry>()[1] = ListEntry { offset: 2, length: 1 };
        let (child, size) = list.list_child().unwrap();
        assert_eq!(size, 3);
        assert_eq!(child.get_value(2), Value::bigint(3));
    }

    #[test]
    fn keep_alive_owns_foreign_string_memory() {
        let owner: Arc<Vec<u8>> = Arc::new(b"a string that is definitely not inlined".to_vec());
        let mut vector = Vector::flat(LogicalType::VARCHAR, 1);
        // SAFETY: the bytes live in `owner`, which the vector keeps alive below.
        let string = unsafe { StringT::from_raw(&owner[..]) };
        vector.values_mut::<StringT>()[0] = string;
        vector.add_keep_alive(owner.clone());
        drop(owner);
        assert_eq!(vector.string_bytes(0), b"a string that is definitely not inlined");
        assert_eq!(vector.data_bytes().len(), 16);
    }
}
