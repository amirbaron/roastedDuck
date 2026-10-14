use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::buffer::{RawBuffer, SharedBuffer};
use crate::selection::{Sel, SelectionVector};
use crate::{LogicalType, NativeType, PhysicalType, StringT, TypeInfo, ValidityMask, Value};

/// The vector representations of DuckDB's `VectorType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorKind {
    /// One slot per row.
    Flat,
    /// A single slot that applies to every row.
    Constant,
    /// Rows select slots of a flat child vector.
    Dictionary,
    /// Row `i` holds `start + i * increment` (integer types only).
    Sequence,
}

/// `(offset, length)` of one list value inside the list's child vector (`list_entry_t`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ListEntry {
    pub offset: u64,
    pub length: u64,
}

// SAFETY: two `u64`s, POD.
unsafe impl NativeType for ListEntry {
    const PHYSICAL: PhysicalType = PhysicalType::List;
}

/// Append-only storage for string bytes referenced by `StringT` values.
///
/// Chunks never move once allocated, so pointers handed out stay valid for the heap's
/// lifetime. Vectors keep the heaps their strings point into alive via `Arc`.
#[derive(Debug, Default)]
pub struct StringHeap {
    chunks: Vec<Box<[u8]>>,
    used: usize,
}

impl StringHeap {
    const MIN_CHUNK: usize = 4096;

    /// Copies `bytes` into the heap and returns a `StringT` referencing them.
    pub fn add(&mut self, bytes: &[u8]) -> StringT {
        if bytes.len() <= StringT::INLINE_LENGTH {
            return StringT::inline(bytes);
        }
        let fits = self
            .chunks
            .last()
            .is_some_and(|chunk| chunk.len() - self.used >= bytes.len());
        if !fits {
            let grown = self.chunks.last().map_or(0, |chunk| chunk.len() * 2);
            let size = bytes.len().max(Self::MIN_CHUNK).max(grown);
            self.chunks.push(vec![0; size].into_boxed_slice());
            self.used = 0;
        }
        let used = self.used;
        let Some(chunk) = self.chunks.last_mut() else {
            unreachable!("a chunk was just ensured")
        };
        let target = &mut chunk[used..used + bytes.len()];
        target.copy_from_slice(bytes);
        self.used += bytes.len();
        // SAFETY: `target` lives in a boxed chunk owned by this heap; it never moves and is
        // only freed when the heap is dropped. Vectors keep the heap alive while referenced.
        unsafe { StringT::from_raw(target) }
    }
}

/// Type-specific auxiliary data of a flat/constant vector.
#[derive(Clone, Debug, Default)]
enum Aux {
    #[default]
    None,
    /// Heaps that `StringT` values may point into. New strings are appended to the last
    /// heap while this vector is its only owner.
    Strings(Vec<Arc<StringHeap>>),
    /// LIST/MAP: the child vector holding all list elements, and how many are in use.
    List { child: Box<Vector>, size: usize },
    /// STRUCT/UNION: one child per field.
    Struct(Vec<Vector>),
    /// ARRAY: child holding `size * array_size` elements.
    Array(Box<Vector>),
}

/// Physical storage of a flat or constant vector.
#[derive(Clone, Debug)]
struct Flat {
    data: SharedBuffer,
    validity: ValidityMask,
    aux: Aux,
}

#[derive(Clone, Debug)]
enum Repr {
    Flat(Flat),
    Constant(Flat),
    /// Invariant: `child` is always flat (slicing a dictionary composes selections).
    Dictionary {
        sel: SelectionVector,
        child: Arc<Vector>,
    },
    Sequence {
        start: i64,
        increment: i64,
    },
}

/// A column of up to `STANDARD_VECTOR_SIZE` values in DuckDB's in-memory format.
#[derive(Clone, Debug)]
pub struct Vector {
    ty: LogicalType,
    repr: Repr,
}

/// A vector's values seen through its selection, regardless of representation
/// (DuckDB's `UnifiedVectorFormat`).
#[derive(Clone, Copy, Debug)]
pub struct Unified<'a, T> {
    pub values: &'a [T],
    pub sel: Sel<'a>,
    pub validity: &'a ValidityMask,
}

impl<T: Copy> Unified<'_, T> {
    /// The slot holding logical row `row`.
    #[inline]
    pub fn slot(&self, row: usize) -> usize {
        self.sel.slot(row)
    }

    #[inline]
    pub fn is_valid(&self, row: usize) -> bool {
        self.validity.is_valid(self.sel.slot(row))
    }

    /// The value of row `row`, or `None` if it is NULL.
    #[inline]
    pub fn get(&self, row: usize) -> Option<T> {
        let slot = self.sel.slot(row);
        self.validity.is_valid(slot).then(|| self.values[slot])
    }
}

impl Vector {
    /// A flat vector with room for `capacity` rows, all zero/valid.
    pub fn flat(ty: LogicalType, capacity: usize) -> Self {
        let width = ty.physical_type().size();
        let aux = match (ty.physical_type(), ty.info()) {
            (PhysicalType::Varchar, _) => Aux::Strings(Vec::new()),
            (PhysicalType::List, Some(TypeInfo::List { child })) => Aux::List {
                child: Box::new(Self::flat(child.clone(), capacity)),
                size: 0,
            },
            (PhysicalType::Struct, Some(TypeInfo::Struct { children })) => Aux::Struct(
                children
                    .iter()
                    .map(|(_, ty)| Self::flat(ty.clone(), capacity))
                    .collect(),
            ),
            (PhysicalType::Array, Some(TypeInfo::Array { child, size })) => {
                Aux::Array(Box::new(Self::flat(child.clone(), capacity * *size as usize)))
            }
            _ => Aux::None,
        };
        let flat = Flat {
            data: Arc::new(RawBuffer::zeroed(width * capacity)),
            validity: ValidityMask::all_valid(),
            aux,
        };
        Self {
            ty,
            repr: Repr::Flat(flat),
        }
    }

    /// A constant vector holding `value` for every row.
    pub fn constant(value: &Value) -> Result<Self> {
        let mut vector = Self::flat(value.logical_type().clone(), 1);
        vector.set_value(0, value)?;
        if let Repr::Flat(flat) = vector.repr {
            vector.repr = Repr::Constant(flat);
        }
        Ok(vector)
    }

    /// Turns a flat vector into a constant vector whose value is slot 0.
    pub fn make_constant(&mut self) {
        if let Repr::Flat(flat) = &self.repr {
            self.repr = Repr::Constant(flat.clone());
        }
    }

    /// Replaces `self` with a fresh flat vector of the same type, reusing nothing.
    pub fn reset_flat(&mut self, capacity: usize) {
        *self = Self::flat(self.ty.clone(), capacity);
    }

    /// A sequence vector: row `i` is `start + i * increment`.
    pub fn sequence(ty: LogicalType, start: i64, increment: i64) -> Self {
        Self {
            ty,
            repr: Repr::Sequence { start, increment },
        }
    }

    /// A vector whose row `i` is row `sel[i]` of `self`, without copying data.
    pub fn slice(&self, sel: &SelectionVector) -> Self {
        let repr = match &self.repr {
            Repr::Constant(_) => return self.clone(),
            Repr::Sequence { .. } => {
                let mut flat = self.clone();
                flat.flatten(sel.as_slice().iter().map(|&i| i as usize + 1).max().unwrap_or(0));
                return flat.slice(sel);
            }
            Repr::Dictionary { sel: inner, child } => Repr::Dictionary {
                sel: sel.compose(inner),
                child: Arc::clone(child),
            },
            Repr::Flat(_) => Repr::Dictionary {
                sel: sel.clone(),
                child: Arc::new(self.clone()),
            },
        };
        Self {
            ty: self.ty.clone(),
            repr,
        }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub fn kind(&self) -> VectorKind {
        match self.repr {
            Repr::Flat(_) => VectorKind::Flat,
            Repr::Constant(_) => VectorKind::Constant,
            Repr::Dictionary { .. } => VectorKind::Dictionary,
            Repr::Sequence { .. } => VectorKind::Sequence,
        }
    }

    fn storage(&self) -> &Flat {
        match &self.repr {
            Repr::Flat(flat) | Repr::Constant(flat) => flat,
            Repr::Dictionary { child, .. } => child.storage(),
            Repr::Sequence { .. } => unreachable!("sequence vectors have no storage; flatten first"),
        }
    }

    /// Mutable storage; dictionaries and sequences are flattened first.
    fn storage_mut(&mut self) -> &mut Flat {
        if matches!(self.repr, Repr::Dictionary { .. } | Repr::Sequence { .. }) {
            let count = match &self.repr {
                Repr::Dictionary { sel, .. } => sel.len(),
                _ => crate::STANDARD_VECTOR_SIZE,
            };
            self.flatten(count);
        }
        match &mut self.repr {
            Repr::Flat(flat) | Repr::Constant(flat) => flat,
            Repr::Dictionary { .. } | Repr::Sequence { .. } => unreachable!("flattened above"),
        }
    }

    /// The validity of the vector's slots (see [`Vector::unified`] for row-level access).
    pub fn validity(&self) -> &ValidityMask {
        &self.storage().validity
    }

    pub fn validity_mut(&mut self) -> &mut ValidityMask {
        &mut self.storage_mut().validity
    }

    /// Number of slots in the data buffer.
    pub fn capacity(&self) -> usize {
        let width = self.ty.physical_type().size();
        self.storage().data.len().checked_div(width).unwrap_or(0)
    }

    /// Row-to-slot mapping for `values`/`validity` of a flat, constant or dictionary vector.
    #[inline]
    pub fn sel(&self) -> Sel<'_> {
        match &self.repr {
            Repr::Flat(_) => Sel::Identity,
            Repr::Constant(_) => Sel::Constant,
            Repr::Dictionary { sel, .. } => Sel::Indices(sel.as_slice()),
            Repr::Sequence { .. } => unreachable!("sequence vectors must be flattened before access"),
        }
    }

    pub fn is_null(&self, row: usize) -> bool {
        match self.repr {
            Repr::Sequence { .. } => false,
            _ => !self.storage().validity.is_valid(self.sel().slot(row)),
        }
    }

    /// The data slots reinterpreted as `T`.
    pub fn values<T: NativeType>(&self) -> &[T] {
        assert_eq!(
            T::PHYSICAL,
            self.ty.physical_type(),
            "vector accessed with the wrong native type"
        );
        let data = &self.storage().data;
        let len = data.len() / size_of::<T>();
        // SAFETY: the buffer is 16-byte aligned, holds `len` values of the vector's physical
        // type (checked above), and `T: NativeType` is POD for that physical type.
        unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<T>(), len) }
    }

    /// Mutable data slots, copying the buffer first if it is shared.
    pub fn values_mut<T: NativeType>(&mut self) -> &mut [T] {
        assert_eq!(
            T::PHYSICAL,
            self.ty.physical_type(),
            "vector accessed with the wrong native type"
        );
        let buffer = Arc::make_mut(&mut self.storage_mut().data);
        let len = buffer.len() / size_of::<T>();
        // SAFETY: as in `values`; `make_mut` guarantees unique access.
        unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<T>(), len) }
    }

    /// Values, selection and validity in one view (DuckDB's `ToUnifiedFormat`).
    ///
    /// Sequence vectors must be flattened before calling this.
    pub fn unified<T: NativeType>(&self) -> Unified<'_, T> {
        Unified {
            values: self.values::<T>(),
            sel: self.sel(),
            validity: self.validity(),
        }
    }

    /// Bytes of the string in row `row` (VARCHAR/BLOB vectors).
    pub fn string_bytes(&self, row: usize) -> &[u8] {
        let value = &self.values::<StringT>()[self.sel().slot(row)];
        // SAFETY: every non-inlined `StringT` reachable from this vector points into a heap
        // held by its storage (`Aux::Strings`), which lives as long as `self`.
        unsafe { value.as_bytes() }
    }

    /// Bytes of a `StringT` taken from this vector's values.
    pub fn string_value<'a>(&'a self, value: &'a StringT) -> &'a [u8] {
        // SAFETY: see `string_bytes`; the borrow ties the bytes to `self`.
        unsafe { value.as_bytes() }
    }

    /// Copies `bytes` into this vector's heap and returns the referencing `StringT`.
    pub fn add_string(&mut self, bytes: &[u8]) -> StringT {
        if bytes.len() <= StringT::INLINE_LENGTH {
            return StringT::inline(bytes);
        }
        let Aux::Strings(heaps) = &mut self.storage_mut().aux else {
            unreachable!("add_string on a non-string vector")
        };
        if let Some(heap) = heaps.last_mut().and_then(Arc::get_mut) {
            return heap.add(bytes);
        }
        let mut heap = StringHeap::default();
        let value = heap.add(bytes);
        heaps.push(Arc::new(heap));
        value
    }

    /// Keeps the string heaps of `other` alive in `self`, so `StringT`s copied from `other`
    /// stay valid (DuckDB's `StringVector::AddHeapReference`).
    pub fn add_heap_references(&mut self, other: &Vector) {
        let Aux::Strings(source) = &other.storage().aux else {
            return;
        };
        let source = source.clone();
        if let Aux::Strings(heaps) = &mut self.storage_mut().aux {
            // Keep our own appendable heap last.
            let own = heaps.pop();
            heaps.extend(source);
            heaps.extend(own);
        }
    }

    /// The child vector of a LIST/MAP vector and the number of child rows in use.
    pub fn list_child(&self) -> Option<(&Vector, usize)> {
        match &self.storage().aux {
            Aux::List { child, size } => Some((child, *size)),
            _ => None,
        }
    }

    /// The element vector of an ARRAY vector (`size * array_size` rows).
    pub fn array_child(&self) -> Option<&Vector> {
        match &self.storage().aux {
            Aux::Array(child) => Some(child),
            _ => None,
        }
    }

    /// The field vectors of a STRUCT vector.
    pub fn struct_children(&self) -> Option<&[Vector]> {
        match &self.storage().aux {
            Aux::Struct(children) => Some(children),
            _ => None,
        }
    }

    /// Reads row `row` as a `Value`.
    pub fn get_value(&self, row: usize) -> Value {
        if let Repr::Sequence { start, increment } = self.repr {
            let value = start.wrapping_add(increment.wrapping_mul(row as i64));
            return Value::from_i64_as(&self.ty, value);
        }
        let slot = self.sel().slot(row);
        if !self.storage().validity.is_valid(slot) {
            return Value::null(self.ty.clone());
        }
        Value::from_vector_slot(self, slot)
    }

    /// Writes `value` into row `row` (the vector is flattened first if needed).
    pub fn set_value(&mut self, row: usize, value: &Value) -> Result<()> {
        let capacity = self.capacity();
        let slot = match self.repr {
            Repr::Constant(_) => 0,
            _ => row,
        };
        let storage = self.storage_mut();
        if value.is_null() {
            storage.validity.set_invalid(slot, capacity.max(slot + 1));
            return Ok(());
        }
        storage.validity.set_valid(slot);
        value.write_to_vector(self, slot)
    }

    /// Converts the vector into a flat vector of `count` rows.
    pub fn flatten(&mut self, count: usize) {
        if matches!(self.repr, Repr::Flat(_)) {
            return;
        }
        let mut flat = Self::flat(self.ty.clone(), count.max(1));
        for row in 0..count {
            let value = self.get_value(row);
            // Writing a value of the vector's own type into a fresh flat vector cannot fail.
            let _ = flat.set_value(row, &value);
        }
        *self = flat;
    }

    /// Checks that the vector's physical type is `expected`.
    pub fn expect_physical(&self, expected: PhysicalType) -> Result<()> {
        if self.ty.physical_type() == expected {
            Ok(())
        } else {
            Err(Error::internal(format!(
                "Expected vector of physical type {expected:?}, found {}",
                self.ty
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_slices_compose() {
        let mut base = Vector::flat(LogicalType::INTEGER, 4);
        base.values_mut::<i32>().copy_from_slice(&[10, 20, 30, 40]);
        base.set_value(2, &Value::null(LogicalType::INTEGER)).unwrap();
        let once = base.slice(&SelectionVector::new(vec![3, 2, 1]));
        let twice = once.slice(&SelectionVector::new(vec![2, 0]));
        assert_eq!(twice.kind(), VectorKind::Dictionary);
        assert_eq!(twice.get_value(0), Value::integer(20));
        assert_eq!(twice.get_value(1), Value::integer(40));
        assert!(once.is_null(1));
        let unified = once.unified::<i32>();
        assert_eq!(unified.get(0), Some(40));
        assert_eq!(unified.get(1), None);
    }

    #[test]
    fn sequences_and_constants_flatten() {
        let mut seq = Vector::sequence(LogicalType::BIGINT, 5, 3);
        assert_eq!(seq.get_value(2), Value::bigint(11));
        seq.flatten(4);
        assert_eq!(seq.values::<i64>(), &[5, 8, 11, 14]);

        let mut constant = Vector::constant(&Value::varchar("a long string value here")).unwrap();
        constant.flatten(3);
        assert_eq!(constant.kind(), VectorKind::Flat);
        assert_eq!(constant.string_bytes(2), b"a long string value here");
    }
}
