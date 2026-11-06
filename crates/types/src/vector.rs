use std::sync::Arc;

use rustdb_common::{Error, Result};

mod access;

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

/// A whole dictionary of a storage segment, shared by the dictionary vectors scanned from
/// it (DuckDB's `VectorChildBuffer` with its `size` and `id`). Operators may work on its
/// entries once instead of once per row, e.g. hash them once
/// (`DictionaryVector::GetCachedHashes`).
#[derive(Debug)]
pub struct DictionaryInfo {
    size: usize,
    hashes: std::sync::OnceLock<Box<[u64]>>,
}

impl DictionaryInfo {
    /// The information of a dictionary of `size` entries.
    pub fn new(size: usize) -> Self {
        Self {
            size,
            hashes: std::sync::OnceLock::new(),
        }
    }

    /// The number of dictionary entries (`DictionaryVector::DictionarySize`).
    pub fn size(&self) -> usize {
        self.size
    }

    /// The hashes of the dictionary's entries, computed by `compute` on first use.
    pub fn hashes(&self, compute: impl FnOnce() -> Vec<u64>) -> &[u64] {
        self.hashes.get_or_init(|| compute().into_boxed_slice())
    }
}

/// `(offset, length)` of one list value inside the list's child vector (`list_entry_t`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ListEntry {
    pub offset: u64,
    pub length: u64,
}

impl ListEntry {
    pub const fn new(offset: usize, length: usize) -> Self {
        Self {
            offset: offset as u64,
            length: length as u64,
        }
    }

    /// The child rows of this list.
    pub const fn range(self) -> std::ops::Range<usize> {
        self.offset as usize..(self.offset + self.length) as usize
    }
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
    /// Memory owned elsewhere that strings may point into (see [`Vector::add_keep_alive`]).
    #[expect(dead_code, reason = "held only to keep foreign string memory alive")]
    keep_alive: Option<access::KeepAlive>,
}

impl StringHeap {
    const MIN_CHUNK: usize = 4096;

    /// A heap holding `bytes` (filled up), whose strings are taken with
    /// [`StringHeap::string_at`]: a writer can build many strings in one buffer.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let used = bytes.len();
        Self {
            chunks: vec![bytes.into_boxed_slice()],
            used,
            keep_alive: None,
        }
    }

    /// The string of `len` bytes at `offset` of the heap's last chunk (built with
    /// [`StringHeap::from_bytes`]).
    pub fn string_at(&self, offset: usize, len: usize) -> StringT {
        let Some(chunk) = self.chunks.last() else {
            return StringT::inline(&[]);
        };
        let bytes = &chunk[offset..offset + len];
        if len <= StringT::INLINE_LENGTH {
            return StringT::inline(bytes);
        }
        // SAFETY: `bytes` lives in a boxed chunk owned by this heap; it never moves and is
        // only freed when the heap is dropped. Vectors keep the heap alive while referenced.
        unsafe { StringT::from_raw(bytes) }
    }

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

    /// Copies the string of the first `len` (at most 16) bytes of `bytes` into the heap,
    /// unless it is short enough to be inlined: one 16-byte copy instead of a copy of
    /// `len` bytes (DuckDB's `StringDecompress` allocates `sizeof(INPUT_TYPE)` bytes).
    pub fn add_short(&mut self, bytes: &[u8; 16], len: usize) -> StringT {
        if len <= StringT::INLINE_LENGTH {
            return StringT::inline_bits(u128::from_le_bytes(*bytes), len);
        }
        let fits = self
            .chunks
            .last()
            .is_some_and(|chunk| chunk.len() - self.used >= 16);
        if !fits {
            let grown = self.chunks.last().map_or(0, |chunk| chunk.len() * 2);
            self.chunks
                .push(vec![0; Self::MIN_CHUNK.max(grown)].into_boxed_slice());
            self.used = 0;
        }
        let used = self.used;
        let Some(chunk) = self.chunks.last_mut() else {
            unreachable!("a chunk was just ensured")
        };
        let target = &mut chunk[used..used + 16];
        target.copy_from_slice(bytes);
        self.used += 16;
        // SAFETY: as in `add`.
        unsafe { StringT::from_raw(&target[..len.min(16)]) }
    }

    /// Empties the heap for new strings, keeping its largest chunk (and any memory it
    /// keeps alive). The caller owns the heap alone, so no string it handed out is
    /// referenced any more.
    pub fn clear(&mut self) {
        if let Some(largest) = self.chunks.pop() {
            self.chunks.clear();
            self.chunks.push(largest);
        }
        self.used = 0;
    }

    /// Whether no string has been stored out of line.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
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
    /// STRUCT/UNION: one child per field, indexed by the parent's slots.
    Struct(Vec<Vector>),
    /// ARRAY: child holding `array_size` elements per parent slot.
    Array(Box<Vector>),
}

/// Physical storage of a flat or constant vector.
#[derive(Clone, Debug)]
struct Flat {
    data: SharedBuffer,
    validity: ValidityMask,
    aux: Aux,
    /// Number of slots (the data buffer may be empty for STRUCT/ARRAY parents).
    capacity: usize,
}

#[derive(Clone, Debug)]
enum Repr {
    Flat(Flat),
    Constant(Flat),
    /// Invariant: `child` is always flat (slicing a dictionary composes selections).
    Dictionary {
        sel: SelectionVector,
        child: Arc<Vector>,
        /// Set when `child` is a whole dictionary of a storage segment (DuckDB's
        /// `DictionarySize` / `DictionaryId`); kept by slices.
        info: Option<Arc<DictionaryInfo>>,
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

/// The physical width in bytes of one slot of `ty` (0 for STRUCT/ARRAY parents).
fn slot_width(ty: &LogicalType) -> usize {
    ty.physical_type().size()
}

/// The ARRAY size of `ty`, if it is an ARRAY.
fn array_size(ty: &LogicalType) -> usize {
    ty.array_child().map_or(0, |(_, size)| size as usize)
}

impl Vector {
    /// A flat vector with room for `capacity` rows, all zero/valid.
    pub fn flat(ty: LogicalType, capacity: usize) -> Self {
        Self::flat_with(ty, capacity, RawBuffer::zeroed)
    }

    /// A dictionary vector over a whole storage dictionary (DuckDB's
    /// `Vector::Dictionary(buffer_ptr<VectorChildBuffer>, sel)`): row `i` is entry `sel[i]`
    /// of `dictionary`, a flat vector.
    pub fn storage_dictionary(
        dictionary: Arc<Self>,
        sel: SelectionVector,
        info: Arc<DictionaryInfo>,
    ) -> Self {
        Self {
            ty: dictionary.ty.clone(),
            repr: Repr::Dictionary {
                sel,
                child: dictionary,
                info: Some(info),
            },
        }
    }

    /// The dictionary, selection and dictionary information of a vector over a storage
    /// dictionary (DuckDB's `DictionaryVector::DictionaryId` is set).
    pub fn storage_dictionary_parts(&self) -> Option<(&Arc<Self>, &SelectionVector, &Arc<DictionaryInfo>)> {
        match &self.repr {
            Repr::Dictionary {
                sel,
                child,
                info: Some(info),
            } => Some((child, sel, info)),
            _ => None,
        }
    }

    /// A flat vector for a writer that sets every slot it later reads, such as a column
    /// scan: fixed-width slots start with unspecified values (a recycled buffer is not
    /// cleared, like DuckDB's vector cache), strings and nested types start empty as in
    /// [`Vector::flat`]. Only types for which every bit pattern is a valid value are
    /// recycled (not BOOL, whose slots must hold 0 or 1).
    pub fn flat_for_overwrite(ty: LogicalType, capacity: usize) -> Self {
        let any_bits_valid = matches!(
            ty.physical_type(),
            PhysicalType::UInt8
                | PhysicalType::Int8
                | PhysicalType::UInt16
                | PhysicalType::Int16
                | PhysicalType::UInt32
                | PhysicalType::Int32
                | PhysicalType::UInt64
                | PhysicalType::Int64
                | PhysicalType::Float
                | PhysicalType::Double
                | PhysicalType::Interval
                | PhysicalType::UInt128
                | PhysicalType::Int128
        );
        if any_bits_valid {
            Self::flat_with(ty, capacity, RawBuffer::recycled)
        } else {
            Self::flat(ty, capacity)
        }
    }

    fn flat_with(ty: LogicalType, capacity: usize, buffer: fn(usize) -> RawBuffer) -> Self {
        let width = slot_width(&ty);
        let aux = match (ty.physical_type(), ty.info()) {
            (PhysicalType::Varchar, _) => Aux::Strings(Vec::new()),
            (PhysicalType::List, Some(TypeInfo::List(info))) => Aux::List {
                child: Box::new(Self::flat(info.child_type.clone(), capacity)),
                size: 0,
            },
            (PhysicalType::Struct, Some(TypeInfo::Struct(info))) => Aux::Struct(
                info.child_types
                    .iter()
                    .map(|(_, ty)| Self::flat(ty.clone(), capacity))
                    .collect(),
            ),
            (PhysicalType::Array, Some(TypeInfo::Array(info))) => Aux::Array(Box::new(Self::flat(
                info.child_type.clone(),
                capacity * info.size as usize,
            ))),
            _ => Aux::None,
        };
        let flat = Flat {
            data: Arc::new(buffer(width * capacity)),
            validity: ValidityMask::all_valid(),
            aux,
            capacity,
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
        vector.make_constant();
        Ok(vector)
    }

    /// A vector of `count` rows that all hold row `row` of `self` (DuckDB's
    /// `ConstantVector::Reference`). Values of nested types are referenced through a
    /// selection rather than copied out, so a large LIST or MAP costs nothing per call.
    pub fn reference_row(&self, row: usize, count: usize) -> Result<Self> {
        if self.ty.is_nested() {
            return Ok(self.slice(&SelectionVector::new(vec![row as u32; count])));
        }
        Self::constant(&self.get_value(row))
    }

    /// A constant vector that is NULL for every row.
    pub fn constant_null(ty: LogicalType) -> Self {
        let mut vector = Self::flat(ty, 1);
        // Nested children are NULL too (`ConstantVector::SetNull`).
        vector.set_null(0);
        vector.make_constant();
        vector
    }

    /// Turns a flat vector into a constant vector whose value is slot 0.
    pub fn make_constant(&mut self) {
        if let Repr::Flat(flat) = &self.repr {
            self.repr = Repr::Constant(flat.clone());
        }
    }

    /// Turns `self` into an all-valid flat vector with room for `capacity` rows whose slot
    /// contents are unspecified, to be overwritten by a kernel.
    ///
    /// The data buffer is reused when this vector owns a large enough flat buffer of a
    /// non-nested type, so kernels writing chunk after chunk into the same result do not
    /// allocate. Reused string slots are zeroed, so no slot ever points into a released
    /// string heap.
    pub fn reset_flat(&mut self, capacity: usize) {
        if let Repr::Flat(flat) = &mut self.repr
            && flat.capacity >= capacity
            && matches!(flat.aux, Aux::None | Aux::Strings(_))
            && let Some(data) = Arc::get_mut(&mut flat.data)
        {
            if let Aux::Strings(heaps) = &mut flat.aux {
                // SAFETY: the buffer is uniquely owned (`get_mut`) and `len` bytes long; all
                // zero bytes is the empty inline string.
                unsafe { std::ptr::write_bytes(data.as_mut_ptr(), 0, data.len()) };
                heaps.clear();
            }
            flat.validity = ValidityMask::all_valid();
            return;
        }
        *self = Self::flat_for_overwrite(self.ty.clone(), capacity);
    }

    /// A sequence vector: row `i` is `start + i * increment`.
    pub fn sequence(ty: LogicalType, start: i64, increment: i64) -> Self {
        Self {
            ty,
            repr: Repr::Sequence { start, increment },
        }
    }

    /// The same data seen as type `ty`, which must have the same physical layout
    /// (DuckDB's `Vector::Reinterpret`).
    pub fn reinterpret(&self, ty: LogicalType) -> Result<Self> {
        if ty.physical_type() != self.ty.physical_type() {
            return Err(Error::internal(format!(
                "Cannot reinterpret a vector of type {} as {ty}",
                self.ty
            )));
        }
        Ok(Self {
            ty,
            repr: self.repr.clone(),
        })
    }

    /// A vector whose row `i` is row `sel[i]` of `self`, without copying data.
    #[must_use]
    pub fn slice(&self, sel: &SelectionVector) -> Self {
        let repr = match &self.repr {
            Repr::Constant(_) => return self.clone(),
            Repr::Sequence { .. } => {
                let mut flat = self.clone();
                flat.flatten(sel.as_slice().iter().map(|&i| i as usize + 1).max().unwrap_or(0));
                return flat.slice(sel);
            }
            Repr::Dictionary {
                sel: inner,
                child,
                info,
            } => Repr::Dictionary {
                sel: sel.compose(inner),
                child: Arc::clone(child),
                info: info.clone(),
            },
            Repr::Flat(_) => Repr::Dictionary {
                sel: sel.clone(),
                child: Arc::new(self.clone()),
                info: None,
            },
        };
        Self {
            ty: self.ty.clone(),
            repr,
        }
    }

    /// Slices every vector of `columns` with `sel` (`DataChunk::Slice` with a
    /// `SelCache`): dictionaries over the same selection, typically all columns of a
    /// filtered chunk, share one composed selection instead of composing it per column.
    pub fn slice_all(columns: &[Self], sel: &SelectionVector) -> Vec<Self> {
        let mut cache: Vec<(usize, SelectionVector)> = Vec::new();
        columns
            .iter()
            .map(|column| match &column.repr {
                Repr::Dictionary {
                    sel: inner,
                    child,
                    info,
                } => {
                    let key = inner.buffer_key();
                    let composed = if let Some((_, composed)) = cache.iter().find(|(k, _)| *k == key) {
                        composed.clone()
                    } else {
                        let composed = sel.compose(inner);
                        cache.push((key, composed.clone()));
                        composed
                    };
                    Self {
                        ty: column.ty.clone(),
                        repr: Repr::Dictionary {
                            sel: composed,
                            child: Arc::clone(child),
                            info: info.clone(),
                        },
                    }
                }
                _ => column.slice(sel),
            })
            .collect()
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.ty
    }

    /// The bytes `cardinality` rows of this vector occupy, nested children included
    /// (DuckDB's `Vector::GetAllocationSize`).
    pub fn allocation_size(&self, cardinality: usize) -> usize {
        match self.ty.physical_type() {
            PhysicalType::List => {
                let child = self
                    .list_child()
                    .map_or(0, |(child, size)| child.allocation_size(size));
                PhysicalType::List.size() * cardinality + child
            }
            PhysicalType::Array => {
                let size = self.ty.array_child().map_or(0, |(_, size)| size as usize);
                self.array_child()
                    .map_or(0, |child| child.allocation_size(size * cardinality))
            }
            PhysicalType::Struct => self.struct_children().map_or(0, |children| {
                children
                    .iter()
                    .map(|child| child.allocation_size(cardinality))
                    .sum()
            }),
            physical => physical.size() * cardinality,
        }
    }

    pub fn kind(&self) -> VectorKind {
        match self.repr {
            Repr::Flat(_) => VectorKind::Flat,
            Repr::Constant(_) => VectorKind::Constant,
            Repr::Dictionary { .. } => VectorKind::Dictionary,
            Repr::Sequence { .. } => VectorKind::Sequence,
        }
    }

    /// Whether this is a constant vector.
    pub fn is_constant(&self) -> bool {
        matches!(self.repr, Repr::Constant(_))
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

    /// Replaces the slot validity of a flat or constant vector.
    pub fn set_validity(&mut self, validity: ValidityMask) {
        self.storage_mut().validity = validity;
    }

    /// Number of slots in the storage.
    pub fn capacity(&self) -> usize {
        self.storage().capacity
    }

    /// Grows the storage of a flat or constant vector to at least `capacity` slots,
    /// keeping its contents (DuckDB's `Vector::Resize`).
    pub fn resize(&mut self, capacity: usize) {
        let array_size = array_size(&self.ty);
        let width = slot_width(&self.ty);
        let storage = self.storage_mut();
        if capacity <= storage.capacity {
            return;
        }
        let mut data = RawBuffer::zeroed(width * capacity);
        let used = width * storage.capacity;
        // SAFETY: the old buffer holds `used` bytes, the new one at least as many, and the
        // two allocations are distinct.
        unsafe { std::ptr::copy_nonoverlapping(storage.data.as_ptr(), data.as_mut_ptr(), used) };
        storage.data = Arc::new(data);
        storage.capacity = capacity;
        // New rows are valid: extend an existing NULL mask with all-valid words.
        if let Some(words) = storage.validity.words()
            && words.len() < capacity.div_ceil(64)
        {
            let mut grown = words.to_vec();
            grown.resize(capacity.div_ceil(64), u64::MAX);
            storage.validity = ValidityMask::from_words(grown);
        }
        match &mut storage.aux {
            Aux::Struct(children) => children.iter_mut().for_each(|child| child.resize(capacity)),
            Aux::Array(child) => child.resize(capacity * array_size),
            Aux::None | Aux::Strings(_) | Aux::List { .. } => {}
        }
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

    /// Keeps `heap` alive as long as this string vector, so `StringT`s pointing into it
    /// can be stored in the vector (DuckDB's `StringVector::AddBuffer`).
    pub fn add_heap(&mut self, heap: StringHeap) {
        if heap.is_empty() {
            return;
        }
        if let Aux::Strings(heaps) = &mut self.storage_mut().aux {
            // Keep our own appendable heap last.
            let own = heaps.pop();
            heaps.push(Arc::new(heap));
            heaps.extend(own);
        }
    }

    /// Like [`Vector::add_heap`] for a heap someone else may hold too (and reuse once it is
    /// theirs alone again).
    pub fn add_shared_heap(&mut self, heap: Arc<StringHeap>) {
        if heap.is_empty() {
            return;
        }
        if let Aux::Strings(heaps) = &mut self.storage_mut().aux {
            // Keep our own appendable heap last.
            let own = heaps.pop();
            heaps.push(heap);
            heaps.extend(own);
        }
    }

    /// Keeps the string heaps of `other` alive in `self`, so `StringT`s copied from `other`
    /// stay valid (DuckDB's `StringVector::AddHeapReference`).
    pub fn add_heap_references(&mut self, other: &Vector) {
        if matches!(other.repr, Repr::Sequence { .. }) {
            return;
        }
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

    /// Mutable access to the child vector of a LIST/MAP vector.
    pub fn list_child_mut(&mut self) -> Option<&mut Vector> {
        match &mut self.storage_mut().aux {
            Aux::List { child, .. } => Some(child),
            _ => None,
        }
    }

    /// Replaces the child of a LIST/MAP vector; its first `size` rows are in use.
    pub fn set_list_child(&mut self, new_child: Vector, new_size: usize) {
        if let Aux::List { child, size } = &mut self.storage_mut().aux {
            **child = new_child;
            *size = new_size;
        }
    }

    /// The element vector of an ARRAY vector (`array_size` rows per slot).
    pub fn array_child(&self) -> Option<&Vector> {
        match &self.storage().aux {
            Aux::Array(child) => Some(child),
            _ => None,
        }
    }

    /// Mutable access to the element vector of an ARRAY vector.
    pub fn array_child_mut(&mut self) -> Option<&mut Vector> {
        match &mut self.storage_mut().aux {
            Aux::Array(child) => Some(child),
            _ => None,
        }
    }

    /// Replaces the element vector of an ARRAY vector.
    pub fn set_array_child(&mut self, new_child: Vector) {
        if let Aux::Array(child) = &mut self.storage_mut().aux {
            **child = new_child;
        }
    }

    /// The field vectors of a STRUCT (or UNION) vector, indexed by this vector's slots.
    pub fn struct_children(&self) -> Option<&[Vector]> {
        match &self.storage().aux {
            Aux::Struct(children) => Some(children),
            _ => None,
        }
    }

    /// Mutable access to the field vectors of a STRUCT (or UNION) vector.
    pub fn struct_children_mut(&mut self) -> Option<&mut [Vector]> {
        match &mut self.storage_mut().aux {
            Aux::Struct(children) => Some(children),
            _ => None,
        }
    }

    /// Marks slot `row` of a flat vector NULL, including the fields of a STRUCT and the
    /// elements of an ARRAY (DuckDB's `FlatVector::SetNull`), so a NULL nested value never
    /// carries child data that hashing or comparisons would see.
    pub fn set_null(&mut self, row: usize) {
        let capacity = self.capacity().max(row + 1);
        self.validity_mut().set_invalid(row, capacity);
        match self.ty.physical_type() {
            PhysicalType::Struct => {
                if let Some(children) = self.struct_children_mut() {
                    for child in children {
                        child.set_null(row);
                    }
                }
            }
            PhysicalType::Array => {
                let size = array_size(&self.ty);
                if let Some(child) = self.array_child_mut() {
                    for element in row * size..(row + 1) * size {
                        child.set_null(element);
                    }
                }
            }
            _ => {}
        }
    }

    /// Reads row `row` as a `Value`.
    pub fn get_value(&self, row: usize) -> Value {
        if let Repr::Sequence { start, increment } = self.repr {
            let value = start.wrapping_add(increment.wrapping_mul(row as i64));
            return Value::from_i64_as(&self.ty, value);
        }
        let slot = self.sel().slot(row);
        let storage = self.storage();
        if !storage.validity.is_valid(slot) {
            return Value::null(self.ty.clone());
        }
        match &storage.aux {
            Aux::List { child, .. } => {
                let entry = self.values::<ListEntry>()[slot];
                let items = entry.range().map(|i| child.get_value(i)).collect();
                Value::from_children(self.ty.clone(), items)
            }
            Aux::Struct(children) => {
                let fields = children.iter().map(|child| child.get_value(slot)).collect();
                Value::from_children(self.ty.clone(), fields)
            }
            Aux::Array(child) => {
                let size = array_size(&self.ty);
                let items = (slot * size..(slot + 1) * size)
                    .map(|i| child.get_value(i))
                    .collect();
                Value::from_children(self.ty.clone(), items)
            }
            Aux::None | Aux::Strings(_) => Value::from_vector_slot(self, slot),
        }
    }

    /// Writes `value` into row `row` (the vector is flattened first if needed).
    ///
    /// LIST values are appended to the end of the child vector.
    pub fn set_value(&mut self, row: usize, value: &Value) -> Result<()> {
        let slot = match self.repr {
            Repr::Constant(_) => 0,
            _ => row,
        };
        let array_size = array_size(&self.ty);
        let storage = self.storage_mut();
        let capacity = storage.capacity;
        if value.is_null() {
            storage.validity.set_invalid(slot, capacity.max(slot + 1));
            match &mut storage.aux {
                // NULL structs/arrays also have NULL children, like DuckDB.
                Aux::Struct(children) => {
                    for child in children {
                        child.set_value(slot, &Value::null(child.ty.clone()))?;
                    }
                }
                Aux::Array(child) => {
                    let null = Value::null(child.ty.clone());
                    for i in slot * array_size..(slot + 1) * array_size {
                        child.set_value(i, &null)?;
                    }
                }
                Aux::None | Aux::Strings(_) | Aux::List { .. } => {}
            }
            return Ok(());
        }
        storage.validity.set_valid(slot);
        match &mut storage.aux {
            Aux::List { child, size } => {
                let items = value.children().unwrap_or_default();
                let offset = *size;
                let needed = offset + items.len();
                if needed > child.capacity() {
                    child.resize(needed.max(child.capacity() * 2));
                }
                for (i, item) in items.iter().enumerate() {
                    child.set_value(offset + i, item)?;
                }
                *size = needed;
                self.values_mut::<ListEntry>()[slot] = ListEntry::new(offset, items.len());
                Ok(())
            }
            Aux::Struct(children) => {
                let fields = value.children().unwrap_or_default();
                for (child, field) in children.iter_mut().zip(fields) {
                    child.set_value(slot, field)?;
                }
                Ok(())
            }
            Aux::Array(child) => {
                let items = value.children().unwrap_or_default();
                if items.len() != array_size {
                    return Err(Error::internal(format!(
                        "Cannot write an array of {} elements into {}",
                        items.len(),
                        self.ty
                    )));
                }
                for (i, item) in items.iter().enumerate() {
                    child.set_value(slot * array_size + i, item)?;
                }
                Ok(())
            }
            Aux::None | Aux::Strings(_) => value.write_to_vector(self, slot),
        }
    }

    /// Converts the vector into a flat vector of `count` rows.
    pub fn flatten(&mut self, count: usize) {
        match &self.repr {
            Repr::Flat(_) => {}
            Repr::Constant(_) => {
                let sel = SelectionVector::new(vec![0; count]);
                *self = self.gather(&sel);
            }
            Repr::Dictionary { sel, child, .. } => {
                let sel = sel.clone();
                let mut flat = child.gather(&sel);
                // The dictionary may have been reinterpreted as a type of the same layout.
                flat.ty = self.ty.clone();
                *self = flat;
            }
            Repr::Sequence { start, increment } => {
                let (start, increment) = (*start, *increment);
                let mut flat = Self::flat(self.ty.clone(), count.max(1));
                for row in 0..count {
                    let value = start.wrapping_add(increment.wrapping_mul(row as i64));
                    // Writing a value of the vector's own type cannot fail.
                    let _ = flat.set_value(row, &Value::from_i64_as(&self.ty, value));
                }
                *self = flat;
            }
        }
    }

    /// The first `count` rows in a vector that holds nothing else, for operators that keep
    /// rows for long (DuckDB copies them into a `ColumnDataCollection`): a selection of a
    /// few rows of a large vector would otherwise keep the whole vector and its string
    /// heaps alive. Full flat vectors are shared, not copied; LIST, STRUCT
    /// and ARRAY rows are gathered but share their children's storage.
    #[must_use]
    pub fn compacted(&self, count: usize) -> Self {
        match &self.repr {
            Repr::Constant(_) | Repr::Sequence { .. } => return self.clone(),
            Repr::Flat(flat) if flat.capacity <= count => return self.clone(),
            _ => {}
        }
        if self.ty.physical_type() != PhysicalType::Varchar {
            return self.slice(&SelectionVector::incremental(count)).flattened(count);
        }
        let mut result = Self::flat(self.ty.clone(), count.max(1));
        let mut values = Vec::with_capacity(count);
        for row in 0..count {
            if self.is_null(row) {
                result.validity_mut().set_invalid(row, count);
                values.push(StringT::default());
            } else {
                values.push(result.add_string(self.string_bytes(row)));
            }
        }
        result.values_mut::<StringT>()[..count].copy_from_slice(&values);
        result
    }

    /// A flat copy of the first `count` rows (DuckDB's `Vector::Flatten` on a copy).
    #[must_use]
    pub fn flattened(&self, count: usize) -> Self {
        let mut copy = self.clone();
        copy.flatten(count);
        copy
    }

    /// A flat vector whose row `i` is slot `sel[i]` of this flat or constant vector's
    /// storage. Strings share heaps, lists share their child, nested children are gathered.
    fn gather(&self, sel: &SelectionVector) -> Self {
        let storage = self.storage();
        let count = sel.len();
        let width = slot_width(&self.ty);
        let mut data = RawBuffer::zeroed(width * count);
        if width > 0 {
            let source = storage.data.as_ptr();
            let target = data.as_mut_ptr();
            for (row, &slot) in sel.as_slice().iter().enumerate() {
                let slot = slot as usize;
                if slot < storage.capacity {
                    // SAFETY: `slot < capacity` keeps the read in bounds of the source buffer,
                    // `row < count` keeps the write in bounds of the new buffer.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            source.add(slot * width),
                            target.add(row * width),
                            width,
                        );
                    };
                }
            }
        }
        let mut validity = ValidityMask::all_valid();
        if !storage.validity.is_all_valid() {
            for (row, &slot) in sel.as_slice().iter().enumerate() {
                if !storage.validity.is_valid(slot as usize) {
                    validity.set_invalid(row, count);
                }
            }
        }
        let aux = match &storage.aux {
            Aux::None => Aux::None,
            Aux::Strings(heaps) => Aux::Strings(heaps.clone()),
            Aux::List { child, size } => Aux::List {
                child: child.clone(),
                size: *size,
            },
            Aux::Struct(children) => {
                Aux::Struct(children.iter().map(|child| child.gather_rows(sel)).collect())
            }
            Aux::Array(child) => {
                let size = array_size(&self.ty);
                let expanded: Vec<u32> = sel
                    .as_slice()
                    .iter()
                    .flat_map(|&slot| (0..size as u32).map(move |i| slot * size as u32 + i))
                    .collect();
                Aux::Array(Box::new(child.gather_rows(&SelectionVector::new(expanded))))
            }
        };
        Self {
            ty: self.ty.clone(),
            repr: Repr::Flat(Flat {
                data: Arc::new(data),
                validity,
                aux,
                capacity: count,
            }),
        }
    }

    /// A flat vector whose row `i` is logical row `rows[i]` of `self`, or NULL where
    /// `rows[i]` is `None` (a copying `slice` that can introduce NULLs).
    #[must_use]
    pub fn select_rows(&self, rows: &[Option<u32>]) -> Self {
        let sel = SelectionVector::new(rows.iter().map(|row| row.unwrap_or(0)).collect::<Vec<_>>());
        let mut selected = self.gather_rows(&sel);
        if let Repr::Flat(flat) = &mut selected.repr {
            for (row, _) in rows.iter().enumerate().filter(|(_, row)| row.is_none()) {
                flat.validity.set_invalid(row, rows.len());
            }
        }
        selected
    }

    /// Like `gather`, but `sel` selects logical rows of any representation.
    fn gather_rows(&self, sel: &SelectionVector) -> Self {
        match &self.repr {
            Repr::Flat(_) => self.gather(sel),
            Repr::Constant(_) => self.gather(&SelectionVector::new(vec![0; sel.len()])),
            Repr::Dictionary {
                sel: inner, child, ..
            } => child.gather(&sel.compose(inner)),
            Repr::Sequence { .. } => {
                let max = sel.as_slice().iter().map(|&i| i as usize + 1).max().unwrap_or(0);
                self.flattened(max).gather(sel)
            }
        }
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
        let flat = once.flattened(3);
        assert_eq!(flat.kind(), VectorKind::Flat);
        assert_eq!(flat.values::<i32>()[0], 40);
        assert_eq!(flat.values::<i32>()[2], 20);
        assert!(flat.is_null(1));
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

    #[test]
    fn nested_values_round_trip() {
        let list = Value::list(
            LogicalType::INTEGER,
            vec![
                Value::integer(1),
                Value::null(LogicalType::INTEGER),
                Value::integer(3),
            ],
        );
        let row = Value::struct_value(vec![
            ("a".into(), list.clone()),
            ("b".into(), Value::varchar("a string longer than twelve")),
        ]);
        let mut vector = Vector::flat(row.logical_type().clone(), 2);
        vector.set_value(0, &row).unwrap();
        vector
            .set_value(1, &Value::null(row.logical_type().clone()))
            .unwrap();
        assert_eq!(vector.get_value(0), row);
        assert!(vector.get_value(1).is_null());
        let constant = Vector::constant(&row).unwrap();
        let flat = constant.flattened(3);
        assert_eq!(flat.get_value(2), row);
        let mut lists = Vector::flat(list.logical_type().clone(), 1);
        for row in 0..40 {
            lists.resize(row + 1);
            lists.set_value(row, &list).unwrap();
        }
        assert_eq!(lists.get_value(39), list);
        assert_eq!(lists.list_child().map(|(_, size)| size), Some(120));
    }
}
