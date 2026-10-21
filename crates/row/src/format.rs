//! Representation-independent views of input vectors (DuckDB's `TupleDataVectorFormat`).
//!
//! A [`VectorFormat`] resolves a vector's representation (flat, constant, dictionary) into
//! raw slot data plus a mapping from logical row to slot, recursively for nested types.
//! Struct children are mapped through their parent's selection, so a dictionary over a
//! STRUCT vector is read correctly without flattening it.

use std::borrow::Cow;

use rustdb_common::{Error, Result};
use rustdb_types::{
    DataChunk, ListEntry, PhysicalType, Sel, StringT, TypeInfo, ValidityMask, Vector, VectorKind,
};

/// Maps logical rows to slots of a vector's data.
#[derive(Clone, Debug)]
pub(crate) enum SlotMap<'a> {
    /// Row `i` is slot `i`.
    Identity,
    /// Every row is the given slot.
    Fixed(u32),
    /// Row `i` is slot `indices[i]`.
    Indices(Cow<'a, [u32]>),
}

impl SlotMap<'_> {
    #[inline]
    pub(crate) fn slot(&self, row: usize) -> usize {
        match self {
            Self::Identity => row,
            Self::Fixed(slot) => *slot as usize,
            Self::Indices(indices) => indices[row] as usize,
        }
    }

    fn from_sel(sel: Sel<'_>) -> SlotMap<'_> {
        match sel {
            Sel::Identity => SlotMap::Identity,
            Sel::Constant => SlotMap::Fixed(0),
            Sel::Indices(indices) => SlotMap::Indices(Cow::Borrowed(indices)),
        }
    }

    /// `inner ∘ self`: row `i` maps to `inner.slot(self.slot(i))`, for `count` rows.
    fn compose<'b>(&self, inner: SlotMap<'b>, count: usize) -> SlotMap<'b> {
        match (self, &inner) {
            (Self::Identity, _) => inner,
            (_, SlotMap::Identity) => match self {
                Self::Identity => SlotMap::Identity,
                Self::Fixed(slot) => SlotMap::Fixed(*slot),
                Self::Indices(indices) => SlotMap::Indices(Cow::Owned(indices.to_vec())),
            },
            (_, SlotMap::Fixed(slot)) => SlotMap::Fixed(*slot),
            (Self::Fixed(slot), _) => SlotMap::Fixed(inner.slot(*slot as usize) as u32),
            (Self::Indices(indices), _) => SlotMap::Indices(Cow::Owned(
                indices[..count.min(indices.len())]
                    .iter()
                    .map(|&slot| inner.slot(slot as usize) as u32)
                    .collect(),
            )),
        }
    }
}

/// How a vector stores its values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FormatKind {
    /// Fixed-width values of the given byte width.
    Fixed(usize),
    Varchar,
    Struct,
    List,
    /// Fixed-size lists of the given length.
    Array(u64),
}

/// A unified view of one vector; see the module documentation.
#[derive(Debug)]
pub struct VectorFormat<'a> {
    pub(crate) kind: FormatKind,
    pub(crate) vector: &'a Vector,
    data: &'a [u8],
    pub(crate) sel: SlotMap<'a>,
    validity: &'a ValidityMask,
    list_entries: &'a [ListEntry],
    pub(crate) children: Vec<VectorFormat<'a>>,
}

fn format_kind(vector: &Vector) -> Result<FormatKind> {
    let ty = vector.logical_type();
    Ok(match ty.physical_type() {
        PhysicalType::Varchar => FormatKind::Varchar,
        PhysicalType::Struct => FormatKind::Struct,
        PhysicalType::List => FormatKind::List,
        PhysicalType::Array => match ty.info() {
            Some(TypeInfo::Array(rustdb_types::ArrayTypeInfo { size, .. })) => {
                FormatKind::Array(u64::from(*size))
            }
            _ => return Err(Error::internal(format!("ARRAY type without size: {ty}"))),
        },
        PhysicalType::Bit | PhysicalType::Unknown | PhysicalType::Invalid => {
            return Err(Error::not_implemented(format!(
                "Unsupported type for row format: {ty}"
            )));
        }
        other => FormatKind::Fixed(other.size()),
    })
}

impl<'a> VectorFormat<'a> {
    /// A view of the first `count` rows of `vector`. Sequence vectors must be flattened
    /// first.
    pub fn new(vector: &'a Vector, count: usize) -> Result<Self> {
        if vector.kind() == VectorKind::Sequence {
            return Err(Error::internal(
                "sequence vectors must be flattened before row formatting",
            ));
        }
        Self::build(vector, SlotMap::from_sel(vector.sel()), count)
    }

    fn build(vector: &'a Vector, sel: SlotMap<'a>, count: usize) -> Result<Self> {
        let kind = format_kind(vector)?;
        let mut format = Self {
            kind,
            vector,
            data: vector.data_bytes(),
            sel,
            validity: vector.validity(),
            list_entries: &[],
            children: Vec::new(),
        };
        match kind {
            FormatKind::Struct => {
                for child in vector.struct_children().unwrap_or_default() {
                    if child.kind() == VectorKind::Sequence {
                        return Err(Error::internal(
                            "sequence vectors must be flattened before row formatting",
                        ));
                    }
                    let child_sel = format.sel.compose(SlotMap::from_sel(child.sel()), count);
                    format.children.push(Self::build(child, child_sel, count)?);
                }
            }
            FormatKind::List => {
                format.list_entries = vector.values::<ListEntry>();
                if let Some((child, size)) = vector.list_child() {
                    format.children.push(Self::new(child, size)?);
                }
            }
            FormatKind::Array(size) => {
                if let Some(child) = vector.array_child() {
                    let rows = vector.row_capacity().max(1) * size as usize;
                    format.children.push(Self::new(child, rows)?);
                }
            }
            FormatKind::Fixed(_) | FormatKind::Varchar => {}
        }
        Ok(format)
    }

    /// The slot holding logical row `row`.
    #[inline]
    pub(crate) fn slot(&self, row: usize) -> usize {
        self.sel.slot(row)
    }

    #[inline]
    pub(crate) fn all_valid(&self) -> bool {
        self.validity.is_all_valid()
    }

    /// Whether slot `slot` is valid.
    #[inline]
    pub(crate) fn slot_is_valid(&self, slot: usize) -> bool {
        self.validity.is_valid(slot)
    }

    /// Whether logical row `row` is valid.
    #[inline]
    pub(crate) fn row_is_valid(&self, row: usize) -> bool {
        self.validity.is_valid(self.sel.slot(row))
    }

    /// Raw bytes of slot `slot` of a fixed-width vector.
    #[inline]
    pub(crate) fn slot_bytes(&self, slot: usize, width: usize) -> &'a [u8] {
        &self.data[slot * width..(slot + 1) * width]
    }

    /// Pointer to the data of slot 0 of a fixed-width vector: slot `slot` is
    /// `slot * width` bytes further.
    #[inline]
    pub(crate) fn data_ptr(&self) -> *const u8 {
        self.data.as_ptr()
    }

    /// Pointer to the data of slot `slot` of a fixed-width vector.
    #[inline]
    pub(crate) fn slot_ptr(&self, slot: usize, width: usize) -> *const u8 {
        debug_assert!((slot + 1) * width <= self.data.len());
        // SAFETY: the slot lies within the data buffer (checked in debug builds; slots
        // come from the vector's own selection).
        unsafe { self.data.as_ptr().add(slot * width) }
    }

    /// The `string_t` in slot `slot` of a VARCHAR vector.
    #[inline]
    pub(crate) fn string(&self, slot: usize) -> StringT {
        // SAFETY: VARCHAR data is an array of 16-byte `StringT`s; the read is unaligned-safe.
        unsafe {
            self.slot_ptr(slot, size_of::<StringT>())
                .cast::<StringT>()
                .read_unaligned()
        }
    }

    /// The bytes of the string in slot `slot` (the vector keeps its heaps alive).
    #[inline]
    pub(crate) fn string_bytes(&self, slot: usize) -> &'a [u8] {
        let string: &'a [u8] = &self.data[slot * 16..(slot + 1) * 16];
        // SAFETY: `string` is a `StringT` of this vector (the read is unaligned-safe).
        let value = unsafe { string.as_ptr().cast::<StringT>().read_unaligned() };
        let len = value.len();
        if value.is_inlined() {
            // Inline strings live in the slot itself, after the 4-byte length.
            return &string[4..4 + len];
        }
        // SAFETY: non-inlined strings point into heaps owned by the vector, which
        // outlives `'a`.
        unsafe {
            let bytes = value.as_bytes();
            std::slice::from_raw_parts(bytes.as_ptr(), len)
        }
    }

    /// `(offset, length)` of the elements of the list/array in slot `slot`.
    #[inline]
    pub(crate) fn list_entry(&self, slot: usize) -> (usize, usize) {
        if let FormatKind::Array(size) = self.kind {
            (slot * size as usize, size as usize)
        } else {
            let entry = self.list_entries[slot];
            (entry.offset as usize, entry.length as usize)
        }
    }
}

/// Unified views of all columns of a chunk.
#[derive(Debug)]
pub struct ChunkFormat<'a> {
    pub(crate) columns: Vec<VectorFormat<'a>>,
    count: usize,
}

impl<'a> ChunkFormat<'a> {
    /// Views of every column of `chunk`.
    pub fn new(chunk: &'a DataChunk) -> Result<Self> {
        Self::from_columns(chunk.columns(), chunk.len())
    }

    /// Views of `columns`, each with `count` rows.
    pub fn from_columns(columns: &'a [Vector], count: usize) -> Result<Self> {
        Ok(Self {
            columns: columns
                .iter()
                .map(|column| VectorFormat::new(column, count))
                .collect::<Result<_>>()?,
            count,
        })
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn column(&self, index: usize) -> &VectorFormat<'a> {
        &self.columns[index]
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_types::{LogicalType, LogicalTypeId, SelectionVector};

    #[test]
    fn struct_children_follow_a_dictionary_parent() {
        let ty = LogicalType::with_info(
            LogicalTypeId::Struct,
            TypeInfo::Struct(rustdb_types::StructTypeInfo {
                child_types: vec![("a".into(), LogicalType::INTEGER)],
            }),
        );
        let mut base = Vector::flat(ty, 3);
        base.struct_children_mut().unwrap()[0]
            .values_mut::<i32>()
            .copy_from_slice(&[10, 20, 30]);
        let sliced = base.slice(&SelectionVector::new(vec![2, 0]));
        let format = VectorFormat::new(&sliced, 2).unwrap();
        let child = &format.children[0];
        let value = |row: usize| i32::from_le_bytes(child.slot_bytes(child.slot(row), 4).try_into().unwrap());
        assert_eq!((value(0), value(1)), (30, 10));
    }
}
