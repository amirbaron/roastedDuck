//! VARIANT vectors read and extracted in place (DuckDB's `UnifiedVariantVectorData` and
//! `VariantUtils::VariantExtract`), without unpacking rows into [`VariantData`]s.

use rustdb_common::{Error, Result};

use crate::variant::{VariantData, VariantLogicalType, VariantNested, VariantPathComponent, varint_decode};
use crate::{ListEntry, LogicalType, Sel, SelectionVector, Vector, VectorKind};

/// A VARIANT vector (or a vector with the VARIANT layout), read row by row in place
/// (DuckDB's `UnifiedVariantVectorData`). Every value is addressed by its row's slot in
/// the vector's storage and its index in the row's `values`.
#[derive(Clone, Copy, Debug)]
pub struct VariantVectorView<'a> {
    vector: &'a Vector,
    key_entries: &'a [ListEntry],
    key_strings: &'a Vector,
    children_entries: &'a [ListEntry],
    child_keys_index: &'a Vector,
    child_values_index: &'a [u32],
    values_entries: &'a [ListEntry],
    type_ids: &'a [u8],
    byte_offsets: &'a [u32],
    data: &'a Vector,
}

/// The flat LIST vector `index` of a flat STRUCT's fields, with its flat element vector.
fn flat_list(fields: &[Vector], index: usize) -> Option<(&Vector, &Vector)> {
    let list = fields.get(index)?;
    let (child, _) = list.list_child()?;
    (list.kind() == VectorKind::Flat && child.kind() == VectorKind::Flat).then_some((list, child))
}

/// The flat fields of a flat STRUCT element vector.
fn flat_fields(vector: &Vector) -> Option<&[Vector]> {
    let fields = vector.struct_children()?;
    fields
        .iter()
        .all(|field| field.kind() == VectorKind::Flat)
        .then_some(fields)
}

impl<'a> VariantVectorView<'a> {
    /// A view of `vector`, whose storage must be flat below its own selection (as scans
    /// and the VARIANT functions produce); `None` otherwise.
    pub fn new(vector: &'a Vector) -> Option<Self> {
        if vector.kind() == VectorKind::Sequence {
            return None;
        }
        let fields = vector.struct_children()?;
        if fields.len() != 4 || fields.iter().any(|field| field.kind() != VectorKind::Flat) {
            return None;
        }
        let (keys, key_strings) = flat_list(fields, 0)?;
        let (children, child_entries) = flat_list(fields, 1)?;
        let (values, value_entries) = flat_list(fields, 2)?;
        let [child_keys_index, child_values_index] = flat_fields(child_entries)? else {
            return None;
        };
        let [type_ids, byte_offsets] = flat_fields(value_entries)? else {
            return None;
        };
        Some(Self {
            vector,
            key_entries: keys.values::<ListEntry>(),
            key_strings,
            children_entries: children.values::<ListEntry>(),
            child_keys_index,
            child_values_index: child_values_index.values::<u32>(),
            values_entries: values.values::<ListEntry>(),
            type_ids: type_ids.values::<u8>(),
            byte_offsets: byte_offsets.values::<u32>(),
            data: &fields[3],
        })
    }

    /// The storage slot of row `row`.
    #[inline]
    pub fn slot(&self, row: usize) -> usize {
        self.vector.sel().slot(row)
    }

    /// Whether row `row` is not NULL.
    #[inline]
    pub fn is_valid(&self, row: usize) -> bool {
        !self.vector.is_null(row)
    }

    /// The position in the `values` entries of value `value_index` of slot `slot`.
    #[inline]
    fn value_position(&self, slot: usize, value_index: u32) -> Result<usize> {
        let entry = self
            .values_entries
            .get(slot)
            .ok_or_else(|| Error::internal("VARIANT row out of range"))?;
        if u64::from(value_index) >= entry.length {
            return Err(Error::internal("VARIANT value index out of range"));
        }
        Ok((entry.offset + u64::from(value_index)) as usize)
    }

    /// The type of value `value_index` of slot `slot`.
    #[inline]
    pub fn type_id(&self, slot: usize, value_index: u32) -> Result<VariantLogicalType> {
        VariantLogicalType::from_u8(self.type_ids[self.value_position(slot, value_index)?])
    }

    /// The `data` blob of slot `slot`.
    #[inline]
    pub fn data(&self, slot: usize) -> &'a [u8] {
        if self.data.is_null(slot) {
            return &[];
        }
        self.data.string_bytes(slot)
    }

    /// The payload bytes of value `value_index` of slot `slot` (to the end of the blob).
    #[inline]
    pub fn payload(&self, slot: usize, value_index: u32) -> Result<&'a [u8]> {
        let offset = self.byte_offsets[self.value_position(slot, value_index)?] as usize;
        self.data(slot)
            .get(offset..)
            .ok_or_else(|| Error::internal("VARIANT byte offset out of range"))
    }

    /// The count and first child of an OBJECT or ARRAY value (`DecodeNestedData`).
    pub fn nested(&self, slot: usize, value_index: u32) -> Result<VariantNested> {
        let data = self.payload(slot, value_index)?;
        let mut pos = 0;
        let child_count = u32::try_from(varint_decode(data, &mut pos)?)
            .map_err(|_| Error::internal("VARIANT child count out of range"))?;
        let children_idx = if child_count == 0 {
            0
        } else {
            u32::try_from(varint_decode(data, &mut pos)?)
                .map_err(|_| Error::internal("VARIANT children index out of range"))?
        };
        Ok(VariantNested {
            child_count,
            children_idx,
        })
    }

    /// The `values` index and key of child `index` of slot `slot`'s `children`.
    fn child(&self, slot: usize, index: u32) -> Result<(u32, Option<&'a [u8]>)> {
        let entry = self
            .children_entries
            .get(slot)
            .ok_or_else(|| Error::internal("VARIANT row out of range"))?;
        if u64::from(index) >= entry.length {
            return Err(Error::internal("VARIANT child index out of range"));
        }
        let position = (entry.offset + u64::from(index)) as usize;
        let values_index = self.child_values_index[position];
        if self.child_keys_index.is_null(position) {
            return Ok((values_index, None));
        }
        let keys_index = self.child_keys_index.values::<u32>()[position];
        let keys = self.key_entries[slot];
        if u64::from(keys_index) >= keys.length {
            return Err(Error::internal("VARIANT key index out of range"));
        }
        let key = self.key_strings.string_bytes((keys.offset + u64::from(keys_index)) as usize);
        Ok((values_index, Some(key)))
    }

    /// The child value of an OBJECT by key, or of an ARRAY by 0-based index
    /// (`VariantUtils::FindChildValues`).
    pub fn find_child(&self, slot: usize, nested: VariantNested, component: &VariantPathComponent) -> Result<Option<u32>> {
        match component {
            VariantPathComponent::Index(index) => {
                if *index >= nested.child_count {
                    return Ok(None);
                }
                Ok(Some(self.child(slot, nested.children_idx + index)?.0))
            }
            VariantPathComponent::Key(key) => {
                for index in 0..nested.child_count {
                    let (values_index, child_key) = self.child(slot, nested.children_idx + index)?;
                    if child_key == Some(key.as_bytes()) {
                        return Ok(Some(values_index));
                    }
                }
                Ok(None)
            }
        }
    }

    /// The value `path` leads to from the root of slot `slot`, if every level is an
    /// OBJECT (ARRAY for an index) with that child and the value is not a VARIANT NULL
    /// (the lookups of `VariantUtils::VariantExtract`).
    pub fn find_path(&self, slot: usize, path: &[VariantPathComponent]) -> Result<Option<u32>> {
        let mut value_index = 0;
        for component in path {
            let expected = match component {
                VariantPathComponent::Index(_) => VariantLogicalType::Array,
                VariantPathComponent::Key(_) => VariantLogicalType::Object,
            };
            if self.type_id(slot, value_index)? != expected {
                return Ok(None);
            }
            let nested = self.nested(slot, value_index)?;
            let Some(child) = self.find_child(slot, nested, component)? else {
                return Ok(None);
            };
            if self.type_id(slot, child)? == VariantLogicalType::VariantNull {
                return Ok(None);
            }
            value_index = child;
        }
        Ok(Some(value_index))
    }
}

/// `VariantUtils::VariantExtract`: each row's VARIANT rooted at the value `path` leads to;
/// NULL where a level is not an OBJECT (ARRAY), lacks the child, or the value is a VARIANT
/// NULL. Like DuckDB's, the result shares the keys, children and data of the input and only
/// remaps value 0 of each row.
pub fn extract(input: &Vector, path: &[VariantPathComponent], count: usize) -> Result<Vector> {
    let Some(view) = VariantVectorView::new(input) else {
        return extract_rows(input, path, count);
    };
    let Some(fields) = input.struct_children() else {
        return Err(Error::internal("VARIANT vector without its fields"));
    };
    let rows = if input.is_constant() { count.min(1) } else { count };
    // The value each referenced slot is rooted at; `None` for NULL results.
    let slots = fields[2].capacity();
    let mut roots: Vec<Option<u32>> = vec![None; slots];
    for row in 0..rows {
        if !view.is_valid(row) {
            continue;
        }
        let slot = view.slot(row);
        if slot < slots {
            roots[slot] = view.find_path(slot, path)?;
        }
    }
    // The `values` entry of each row's root, and of the value it becomes.
    let positions: Vec<(usize, usize)> = roots
        .iter()
        .enumerate()
        .filter_map(|(slot, root)| {
            let root = (*root)?;
            if root == 0 {
                return None;
            }
            let entry = view.values_entries.get(slot)?;
            Some((entry.offset as usize, (entry.offset + u64::from(root)) as usize))
        })
        .collect();
    let mut values = fields[2].clone();
    if !positions.is_empty() {
        let entries = values
            .list_child_mut()
            .and_then(Vector::struct_children_mut)
            .ok_or_else(|| Error::internal("VARIANT values without entries"))?;
        {
            let type_ids = entries[0].values_mut::<u8>();
            for &(root, value) in &positions {
                type_ids[root] = type_ids[value];
            }
        }
        let byte_offsets = entries[1].values_mut::<u32>();
        for &(root, value) in &positions {
            byte_offsets[root] = byte_offsets[value];
        }
    }
    let mut result = Vector::flat(LogicalType::variant(), slots.max(1));
    if let Some(result_fields) = result.struct_children_mut() {
        result_fields[0] = fields[0].clone();
        result_fields[1] = fields[1].clone();
        result_fields[2] = values;
        result_fields[3] = fields[3].clone();
    }
    for (slot, root) in roots.iter().enumerate() {
        if root.is_none() {
            result.set_null(slot);
        }
    }
    Ok(match input.sel() {
        Sel::Identity => result,
        Sel::Constant => {
            result.make_constant();
            result
        }
        Sel::Indices(indices) => result.slice(&SelectionVector::new(indices.to_vec())),
    })
}

/// [`extract`] row by row through [`VariantData`], for vectors whose storage is not flat.
fn extract_rows(input: &Vector, path: &[VariantPathComponent], count: usize) -> Result<Vector> {
    let rows = if input.is_constant() { count.min(1) } else { count };
    let mut result = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut current = VariantData::from_vector(input, row)?;
        for component in path {
            let Some(data) = current.take() else {
                break;
            };
            let expected = match component {
                VariantPathComponent::Index(_) => VariantLogicalType::Array,
                VariantPathComponent::Key(_) => VariantLogicalType::Object,
            };
            if data.type_id(0)? != expected {
                break;
            }
            let Some(child) = data.find_child(data.nested(0)?, component)? else {
                break;
            };
            if data.type_id(child)? == VariantLogicalType::VariantNull {
                break;
            }
            current = Some(data.rooted_at(child));
        }
        result.push(current);
    }
    let mut vector = VariantData::write_vector(&result, &LogicalType::variant())?;
    if input.is_constant() {
        vector.make_constant();
    }
    Ok(vector)
}
