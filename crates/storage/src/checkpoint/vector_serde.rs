//! Serialization of vectors and data chunks (DuckDB's `Vector::Serialize/Deserialize` and
//! `DataChunk::Serialize/Deserialize`), used by the WAL and by table samples.

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_serde::{Deserializer, Serializer};
use rustdb_types::{
    DataChunk, ListEntry, LogicalType, LogicalTypeId, PhysicalType, SelectionVector, StringT, ValidityMask,
    Vector,
};

/// Bytes of a validity mask of `count` rows (`ValidityMask::ValidityMaskSize`).
fn mask_size(count: usize) -> usize {
    count.div_ceil(64) * 8
}

/// Whether a type is stored as fixed-size values (`TypeIsConstantSize`).
fn is_constant_size(ty: &LogicalType) -> bool {
    !matches!(
        ty.physical_type(),
        PhysicalType::Varchar | PhysicalType::List | PhysicalType::Struct | PhysicalType::Array
    )
}

/// `Vector::Serialize` of the first `count` rows (always flat).
pub fn serialize_vector(serializer: &mut dyn Serializer, vector: &Vector, count: usize) -> Result<()> {
    let ty = vector.logical_type().clone();
    if ty.id() == LogicalTypeId::Geometry {
        return Err(Error::not_implemented("serializing GEOMETRY vectors"));
    }
    let flat = vector.flattened(count);
    let has_validity_mask = count > 0 && (0..count).any(|row| flat.is_null(row));
    serializer.write_property(100, "has_validity_mask", &has_validity_mask)?;
    if has_validity_mask {
        let mut words = vec![u64::MAX; count.div_ceil(64)];
        for row in 0..count {
            if flat.is_null(row) {
                words[row / 64] &= !(1 << (row % 64));
            }
        }
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        serializer.on_property_begin(101, "validity");
        serializer.write_data(&bytes[..mask_size(count)]);
        serializer.on_property_end();
    }
    if is_constant_size(&ty) {
        let width = ty.physical_type().size();
        let bytes = flat.data_bytes();
        serializer.on_property_begin(102, "data");
        serializer.write_data(bytes.get(..count * width).unwrap_or(bytes));
        serializer.on_property_end();
        return Ok(());
    }
    match ty.physical_type() {
        PhysicalType::Varchar => serializer.write_list_property(102, "data", count, |s, row| {
            if flat.is_null(row) {
                s.write_string_bytes(&[]);
            } else {
                s.write_string_bytes(flat.string_bytes(row));
            }
            Ok(())
        }),
        PhysicalType::Struct => {
            let children = flat.struct_children().unwrap_or_default();
            serializer.write_list_property(103, "children", children.len(), |s, index| {
                s.on_object_begin();
                serialize_vector(s, &children[index], count)?;
                s.on_object_end();
                Ok(())
            })
        }
        PhysicalType::List => {
            let entries = flat.values::<ListEntry>();
            let (child, list_size) = flat
                .list_child()
                .ok_or_else(|| Error::internal("list vector without a child"))?;
            serializer.write_property(104, "list_size", &(list_size as u64))?;
            serializer.write_list_property(105, "entries", count, |s, row| {
                let entry = if flat.is_null(row) {
                    ListEntry::default()
                } else {
                    entries[row]
                };
                s.on_object_begin();
                s.write_property(100, "offset", &entry.offset)?;
                s.write_property(101, "length", &entry.length)?;
                s.on_object_end();
                Ok(())
            })?;
            serializer.write_object_property(106, "child", |s| serialize_vector(s, child, list_size))
        }
        PhysicalType::Array => {
            let size = ty.array_child().map_or(0, |(_, size)| size as usize);
            let child = flat
                .array_child()
                .ok_or_else(|| Error::internal("array vector without a child"))?;
            serializer.write_property(103, "array_size", &(size as u64))?;
            serializer.write_object_property(104, "child", |s| serialize_vector(s, child, size * count))
        }
        other => Err(Error::internal(format!(
            "Unimplemented variable width type for Vector::Serialize! ({other:?})"
        ))),
    }
}

/// `Vector::Deserialize` of `count` rows of type `ty`.
pub fn deserialize_vector(
    deserializer: &mut dyn Deserializer,
    ty: &LogicalType,
    count: usize,
) -> Result<Vector> {
    // `VectorType`: FLAT = 0, CONSTANT = 1, DICTIONARY = 2, SEQUENCE = 3.
    let vector_type: u8 = deserializer.read_optional(90, "vector_type")?.unwrap_or(0);
    match vector_type {
        1 => {
            let single = deserialize_vector(deserializer, ty, 1)?;
            let value = single.get_value(0);
            return Vector::constant(&value);
        }
        2 => {
            deserializer.on_property_begin(91, "sel_vector")?;
            let bytes = deserializer.read_data(4 * count)?;
            deserializer.on_property_end()?;
            let dict_count = usize::try_from(deserializer.read_property::<u64>(92, "dict_count")?)
                .map_err(|_| Error::serialization("dictionary too large"))?;
            let dictionary = deserialize_vector(deserializer, ty, dict_count)?;
            let indexes: Vec<u32> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|chunk| u32::from_le_bytes(*chunk))
                .collect();
            return Ok(dictionary.slice(&SelectionVector::new(indexes)).flattened(count));
        }
        3 => {
            let start: i64 = deserializer.read_property(91, "seq_start")?;
            let increment: i64 = deserializer.read_property(92, "seq_increment")?;
            return Ok(Vector::sequence(ty.clone(), start, increment).flattened(count));
        }
        _ => {}
    }
    if ty.id() == LogicalTypeId::Geometry {
        return Err(Error::not_implemented("deserializing GEOMETRY vectors"));
    }
    let capacity = count.max(STANDARD_VECTOR_SIZE);
    let mut vector = Vector::flat(ty.clone(), capacity);
    let has_validity_mask: bool = deserializer.read_property(100, "has_validity_mask")?;
    let mut validity = None;
    if has_validity_mask {
        deserializer.on_property_begin(101, "validity")?;
        let bytes = deserializer.read_data(mask_size(count))?;
        deserializer.on_property_end()?;
        let mut words: Vec<u64> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| u64::from_le_bytes(*chunk))
            .collect();
        words.resize(capacity.div_ceil(64), u64::MAX);
        validity = Some(ValidityMask::from_words(words));
    }
    if is_constant_size(ty) {
        let width = ty.physical_type().size();
        deserializer.on_property_begin(102, "data")?;
        let bytes = deserializer.read_data(width * count)?;
        deserializer.on_property_end()?;
        vector.data_bytes_mut()[..bytes.len()].copy_from_slice(&bytes);
    } else {
        match ty.physical_type() {
            PhysicalType::Varchar => {
                let mut strings = Vec::with_capacity(count);
                deserializer.read_list_property(102, "data", |d, _| {
                    strings.push(d.read_string_bytes()?);
                    Ok(())
                })?;
                for (row, bytes) in strings.iter().enumerate() {
                    let value: StringT = vector.add_string(bytes);
                    vector.values_mut::<StringT>()[row] = value;
                }
            }
            PhysicalType::Struct => {
                let fields: Vec<LogicalType> = ty
                    .struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(_, field)| field.clone())
                    .collect();
                let mut children = Vec::with_capacity(fields.len());
                deserializer.read_list_property(103, "children", |d, index| {
                    let field = fields
                        .get(index)
                        .ok_or_else(|| Error::serialization("too many struct children"))?;
                    d.on_object_begin()?;
                    children.push(deserialize_vector(d, field, count)?);
                    d.on_object_end()
                })?;
                if let Some(slots) = vector.struct_children_mut() {
                    for (slot, child) in slots.iter_mut().zip(children) {
                        *slot = child;
                    }
                }
            }
            PhysicalType::List => {
                let list_size = usize::try_from(deserializer.read_property::<u64>(104, "list_size")?)
                    .map_err(|_| Error::serialization("list too large"))?;
                let mut entries = Vec::with_capacity(count);
                deserializer.read_list_property(105, "entries", |d, _| {
                    d.on_object_begin()?;
                    let offset: u64 = d.read_property(100, "offset")?;
                    let length: u64 = d.read_property(101, "length")?;
                    entries.push(ListEntry { offset, length });
                    d.on_object_end()
                })?;
                let child_type = ty.child_type().cloned().unwrap_or_default();
                let child = deserializer
                    .read_object_property(106, "child", |d| deserialize_vector(d, &child_type, list_size))?;
                vector.values_mut::<ListEntry>()[..entries.len()].copy_from_slice(&entries);
                vector.set_list_child(child, list_size);
            }
            PhysicalType::Array => {
                let size = usize::try_from(deserializer.read_property::<u64>(103, "array_size")?)
                    .map_err(|_| Error::serialization("array too large"))?;
                let child_type = ty
                    .array_child()
                    .map(|(child, _)| child.clone())
                    .unwrap_or_default();
                let child = deserializer.read_object_property(104, "child", |d| {
                    deserialize_vector(d, &child_type, size * count)
                })?;
                vector.set_array_child(child);
            }
            other => {
                return Err(Error::internal(format!(
                    "Unimplemented variable width type for Vector::Deserialize! ({other:?})"
                )));
            }
        }
    }
    if let Some(validity) = validity {
        vector.set_validity(validity);
    }
    Ok(vector)
}

/// `DataChunk::Serialize`.
pub fn serialize_chunk(serializer: &mut dyn Serializer, chunk: &DataChunk) -> Result<()> {
    let count = chunk.len();
    serializer.write_property(100, "rows", &(count as u32))?;
    let columns = chunk.columns();
    serializer.write_list_property(101, "types", columns.len(), |s, index| {
        s.write_object(columns[index].logical_type())
    })?;
    serializer.write_list_property(102, "columns", columns.len(), |s, index| {
        s.on_object_begin();
        serialize_vector(s, &columns[index], count)?;
        s.on_object_end();
        Ok(())
    })
}

/// `DataChunk::Deserialize`.
pub fn deserialize_chunk(deserializer: &mut dyn Deserializer) -> Result<DataChunk> {
    let rows: u32 = deserializer.read_property(100, "rows")?;
    let count = rows as usize;
    let mut types: Vec<LogicalType> = Vec::new();
    deserializer.read_list_property(101, "types", |d, _| {
        types.push(d.read_object()?);
        Ok(())
    })?;
    let mut columns = Vec::with_capacity(types.len());
    deserializer.read_list_property(102, "columns", |d, index| {
        let ty = types
            .get(index)
            .ok_or_else(|| Error::serialization("more columns than types"))?;
        d.on_object_begin()?;
        columns.push(deserialize_vector(d, ty, count)?);
        d.on_object_end()
    })?;
    Ok(DataChunk::new(columns, count))
}
