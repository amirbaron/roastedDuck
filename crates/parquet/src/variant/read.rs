//! Reading VARIANT columns (`VariantColumnReader` and `VariantShreddedConversion`): the
//! `metadata`, `value` and (shredded) `typed_value` children are read like a struct and
//! converted row by row.

use std::collections::BTreeMap;

use roastedduck_common::{Error, Result};
use roastedduck_types::{ListEntry, LogicalType, LogicalTypeId, PhysicalType, Value, Vector};

use super::{VariantMetadata, VariantValue, decode, to_variant};

/// The rows of a (nested) `value`/`typed_value` group to convert: the row in the group's
/// vectors and the row of the top-level `metadata` it belongs to.
#[derive(Clone, Copy, Debug)]
struct Row {
    index: usize,
    metadata: usize,
}

/// The `value` and `typed_value` vectors of a variant group.
struct Group<'a> {
    value: &'a Vector,
    typed_value: Option<&'a Vector>,
}

/// `VariantShreddedConversion::Convert`'s lookup of a group's children by name.
fn group_of(vector: &Vector) -> Result<Group<'_>> {
    let fields = vector.logical_type().struct_children().unwrap_or_default();
    let children = vector
        .struct_children()
        .ok_or_else(|| Error::internal("variant group is not a STRUCT"))?;
    let mut value = None;
    let mut typed_value = None;
    for ((name, _), child) in fields.iter().zip(children) {
        match name.as_bytes() {
            b"value" => value = Some(child),
            b"typed_value" => typed_value = Some(child),
            other => {
                return Err(Error::invalid_input(roastedduck_common::byte_format!(
                    "Variant group can only contain 'value'/'typed_value', not: {other}"
                )));
            }
        }
    }
    let value =
        value.ok_or_else(|| Error::invalid_input("Required column 'value' not found in Variant group"))?;
    Ok(Group { value, typed_value })
}

/// The parsed metadata of each top-level row.
struct Metadata {
    entries: Vec<Option<VariantMetadata>>,
}

impl Metadata {
    fn get(&self, row: Row) -> Result<&VariantMetadata> {
        self.entries
            .get(row.metadata)
            .and_then(Option::as_ref)
            .ok_or_else(|| super::io_error("Corrupted VARIANT 'metadata' buffer, empty or nullptr"))
    }

    /// Decodes the binary `value` of `row`.
    fn decode(&self, value: &Vector, row: Row) -> Result<VariantValue> {
        decode(self.get(row)?, value.string_bytes(row.index), 0)
    }
}

/// `Convert`: the values of `rows` of a group.
fn convert(metadata: &Metadata, group: &Group<'_>, rows: &[Row]) -> Result<Vec<VariantValue>> {
    let Some(typed_value) = group.typed_value else {
        // `ConvertBinaryEncoding`.
        return rows
            .iter()
            .map(|&row| {
                if group.value.is_null(row.index) {
                    Ok(VariantValue::Missing)
                } else {
                    metadata.decode(group.value, row)
                }
            })
            .collect();
    };
    match typed_value.logical_type().id() {
        LogicalTypeId::Struct => convert_object(metadata, group.value, typed_value, rows),
        LogicalTypeId::List => convert_array(metadata, group.value, typed_value, rows),
        _ => convert_leaf(metadata, group.value, typed_value, rows),
    }
}

/// `ConvertShreddedLeaf`: typed primitives, or binary values where the typed value is NULL.
fn convert_leaf(
    metadata: &Metadata,
    value: &Vector,
    typed_value: &Vector,
    rows: &[Row],
) -> Result<Vec<VariantValue>> {
    use LogicalTypeId as L;
    let ty = typed_value.logical_type();
    let supported = match ty.id() {
        L::Boolean
        | L::TinyInt
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Float
        | L::Double
        | L::Date
        | L::Time
        | L::TimestampTz
        | L::Timestamp
        | L::TimestampNs
        | L::Blob
        | L::Varchar
        | L::Uuid => true,
        L::Decimal => {
            if !matches!(
                ty.physical_type(),
                PhysicalType::Int16 | PhysicalType::Int32 | PhysicalType::Int64 | PhysicalType::Int128
            ) {
                return Err(Error::not_implemented(format!(
                    "Decimal with PhysicalType ({:?}) not implemented for shredded Variant",
                    ty.physical_type()
                )));
            }
            true
        }
        _ => false,
    };
    if !supported {
        return Err(Error::not_implemented(format!(
            "Variant shredding on type: '{ty}' is not implemented"
        )));
    }
    rows.iter()
        .map(|&row| {
            if !typed_value.is_null(row.index) {
                let typed = typed_value.get_value(row.index);
                if ty.id() == L::Varchar
                    && typed
                        .as_bytes()
                        .is_some_and(|bytes| std::str::from_utf8(bytes).is_err())
                {
                    return Err(Error::internal(
                        "Can't decode Variant string, it isn't valid UTF8",
                    ));
                }
                return Ok(VariantValue::Primitive(typed));
            }
            if value.is_null(row.index) {
                return Ok(VariantValue::Missing);
            }
            metadata.decode(value, row)
        })
        .collect()
}

/// `ConvertShreddedObject`: shredded fields merged with the fields left in `value`.
fn convert_object(
    metadata: &Metadata,
    value: &Vector,
    typed_value: &Vector,
    rows: &[Row],
) -> Result<Vec<VariantValue>> {
    let fields = typed_value.logical_type().struct_children().unwrap_or_default();
    let children = typed_value
        .struct_children()
        .ok_or_else(|| Error::internal("shredded object is not a STRUCT"))?;
    let mut shredded = Vec::with_capacity(fields.len());
    for ((name, _), child) in fields.iter().zip(children) {
        let group = group_of(child)?;
        shredded.push((name.as_bytes(), convert(metadata, &group, rows)?));
    }
    let mut result = Vec::with_capacity(rows.len());
    for (position, &row) in rows.iter().enumerate() {
        if typed_value.is_null(row.index) {
            if value.is_null(row.index) {
                result.push(VariantValue::Missing);
                continue;
            }
            let decoded = metadata.decode(value, row)?;
            if matches!(decoded, VariantValue::Object(_)) {
                return Err(Error::invalid_input(
                    "When 'typed_value' for a shredded Object is NULL, 'value' can not contain an Object value",
                ));
            }
            result.push(decoded);
            continue;
        }
        // `ConvertPartiallyShreddedObject`.
        let mut object = BTreeMap::new();
        for (name, values) in &mut shredded {
            let field = std::mem::take(&mut values[position]);
            VariantValue::add_child(&mut object, name, field);
        }
        if !value.is_null(row.index) {
            let VariantValue::Object(unshredded) = metadata.decode(value, row)? else {
                return Err(Error::invalid_input(
                    "Partially shredded objects have to encode Object Variants in the 'value'",
                ));
            };
            for (key, field) in unshredded {
                VariantValue::add_child(&mut object, &key, field);
            }
        }
        result.push(VariantValue::Object(object));
    }
    Ok(result)
}

/// `ConvertShreddedArray`: shredded elements, or binary values where the list is NULL.
fn convert_array(
    metadata: &Metadata,
    value: &Vector,
    typed_value: &Vector,
    rows: &[Row],
) -> Result<Vec<VariantValue>> {
    let max_row = rows.iter().map(|row| row.index + 1).max().unwrap_or(0);
    let list = typed_value.flattened(max_row);
    let (element, _) = list
        .list_child()
        .ok_or_else(|| Error::internal("shredded array is not a LIST"))?;
    let element_group = group_of(element)?;
    let entries = list.values::<ListEntry>();
    let mut result = Vec::with_capacity(rows.len());
    for &row in rows {
        if list.is_null(row.index) {
            if value.is_null(row.index) {
                result.push(VariantValue::Missing);
            } else {
                result.push(metadata.decode(value, row)?);
            }
            continue;
        }
        let entry = entries[row.index];
        let element_rows: Vec<Row> = (entry.offset..entry.offset + entry.length)
            .map(|index| Row {
                index: index as usize,
                metadata: row.metadata,
            })
            .collect();
        result.push(VariantValue::array(convert(
            metadata,
            &element_group,
            &element_rows,
        )?));
    }
    Ok(result)
}

/// `VariantColumnReader::Read` after the children are read: the VARIANT values of `count`
/// rows of `metadata`, `value` and `typed_value`.
pub fn convert_column(
    metadata: &Vector,
    value: &Vector,
    typed_value: Option<&Vector>,
    count: usize,
    result: &mut Vector,
) -> Result<()> {
    let entries = (0..count)
        .map(|row| {
            if metadata.is_null(row) {
                Ok(None)
            } else {
                VariantMetadata::new(metadata.string_bytes(row)).map(Some)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let metadata = Metadata { entries };
    let rows: Vec<Row> = (0..count)
        .map(|index| Row {
            index,
            metadata: index,
        })
        .collect();
    let values = convert(&metadata, &Group { value, typed_value }, &rows)?;
    let variants = to_variant(&values)?;
    if result.logical_type() != &LogicalType::variant() || result.capacity() < count {
        *result = Vector::flat(LogicalType::variant(), count.max(1));
    }
    for (row, variant) in variants.into_iter().enumerate() {
        match variant {
            Some(variant) => result.set_value(row, &variant)?,
            None => result.set_value(row, &Value::null(LogicalType::variant()))?,
        }
    }
    Ok(())
}
