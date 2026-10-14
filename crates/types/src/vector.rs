use std::sync::Arc;

use rustdb_common::{Error, Result};

use crate::buffer::{RawBuffer, SharedBuffer};
use crate::{LogicalType, NativeType, PhysicalType, StringT, ValidityMask, Value};

/// How a vector's values are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorKind {
    /// One slot per row.
    Flat,
    /// A single slot that applies to every row.
    Constant,
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
            let size = bytes
                .len()
                .max(Self::MIN_CHUNK)
                .max(self.chunks.last().map_or(0, |c| c.len() * 2));
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

/// A column of up to `STANDARD_VECTOR_SIZE` values in DuckDB's in-memory format.
#[derive(Clone, Debug)]
pub struct Vector {
    ty: LogicalType,
    kind: VectorKind,
    data: SharedBuffer,
    validity: ValidityMask,
    /// Heaps that `StringT` values in `data` may point into. New strings are appended to
    /// the last heap while this vector is its only owner.
    heaps: Vec<Arc<StringHeap>>,
}

impl Vector {
    /// A flat vector with room for `capacity` rows, all zero/valid.
    pub fn flat(ty: LogicalType, capacity: usize) -> Self {
        let width = ty.physical_type().size();
        Self {
            ty,
            kind: VectorKind::Flat,
            data: Arc::new(RawBuffer::zeroed(width * capacity)),
            validity: ValidityMask::all_valid(),
            heaps: Vec::new(),
        }
    }

    /// A constant vector holding `value` for every row.
    pub fn constant(value: &Value) -> Result<Self> {
        let mut vector = Self::flat(value.logical_type().clone(), 1);
        vector.kind = VectorKind::Constant;
        vector.set_value(0, value)?;
        Ok(vector)
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    pub fn kind(&self) -> VectorKind {
        self.kind
    }

    pub fn validity(&self) -> &ValidityMask {
        &self.validity
    }

    pub fn validity_mut(&mut self) -> &mut ValidityMask {
        &mut self.validity
    }

    /// Number of slots in the data buffer.
    pub fn capacity(&self) -> usize {
        let width = self.ty.physical_type().size();
        self.data.len().checked_div(width).unwrap_or(0)
    }

    /// The slot index holding row `row` (constant vectors have a single slot).
    #[inline]
    pub fn slot(&self, row: usize) -> usize {
        match self.kind {
            VectorKind::Flat => row,
            VectorKind::Constant => 0,
        }
    }

    pub fn is_null(&self, row: usize) -> bool {
        !self.validity.is_valid(self.slot(row))
    }

    /// The data slots reinterpreted as `T`.
    pub fn values<T: NativeType>(&self) -> &[T] {
        assert_eq!(
            T::PHYSICAL,
            self.ty.physical_type(),
            "vector accessed with the wrong native type"
        );
        let len = self.data.len() / size_of::<T>();
        // SAFETY: the buffer is 16-byte aligned, holds `len` values of the vector's physical
        // type (checked above), and `T: NativeType` is POD for that physical type.
        unsafe { std::slice::from_raw_parts(self.data.as_ptr().cast::<T>(), len) }
    }

    /// Mutable data slots, copying the buffer first if it is shared.
    pub fn values_mut<T: NativeType>(&mut self) -> &mut [T] {
        assert_eq!(
            T::PHYSICAL,
            self.ty.physical_type(),
            "vector accessed with the wrong native type"
        );
        let buffer = Arc::make_mut(&mut self.data);
        let len = buffer.len() / size_of::<T>();
        // SAFETY: as in `values`; `make_mut` guarantees unique access.
        unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<T>(), len) }
    }

    /// Bytes of the string in row `row` (VARCHAR/BLOB vectors).
    pub fn string_bytes(&self, row: usize) -> &[u8] {
        let value = &self.values::<StringT>()[self.slot(row)];
        // SAFETY: every non-inlined `StringT` in this vector points into a heap listed in
        // `self.heaps`, which lives as long as `self`.
        unsafe { value.as_bytes() }
    }

    /// Copies `bytes` into this vector's own heap and returns the referencing `StringT`.
    pub fn add_string(&mut self, bytes: &[u8]) -> StringT {
        if bytes.len() <= StringT::INLINE_LENGTH {
            return StringT::inline(bytes);
        }
        if let Some(heap) = self.heaps.last_mut().and_then(Arc::get_mut) {
            return heap.add(bytes);
        }
        let mut heap = StringHeap::default();
        let value = heap.add(bytes);
        self.heaps.push(Arc::new(heap));
        value
    }

    /// Keeps `heap` alive for as long as this vector exists.
    pub fn add_heap(&mut self, heap: Arc<StringHeap>) {
        self.heaps.push(heap);
    }

    /// Reads row `row` as a `Value`.
    pub fn get_value(&self, row: usize) -> Value {
        let slot = self.slot(row);
        if !self.validity.is_valid(slot) {
            return Value::null(self.ty.clone());
        }
        Value::from_vector_slot(self, slot)
    }

    /// Writes `value` into row `row`.
    pub fn set_value(&mut self, row: usize, value: &Value) -> Result<()> {
        let slot = self.slot(row);
        if value.is_null() {
            let capacity = self.capacity();
            self.validity.set_invalid(slot, capacity);
            return Ok(());
        }
        self.validity.set_valid(slot);
        value.write_to_vector(self, slot)
    }

    /// Converts a constant vector into a flat vector of `count` rows.
    pub fn flatten(&mut self, count: usize) -> Result<()> {
        if self.kind == VectorKind::Flat {
            return Ok(());
        }
        let value = self.get_value(0);
        let mut flat = Self::flat(self.ty.clone(), count);
        for row in 0..count {
            flat.set_value(row, &value)?;
        }
        *self = flat;
        Ok(())
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
