//! Writing VARIANT columns (`VariantColumnWriter`, `convert_variant.cpp`,
//! `analyze_variant.cpp` and the core `VariantShredding`): each DuckDB VARIANT value
//! becomes a Parquet variant `metadata` dictionary and binary `value`, optionally with
//! (part of) it shredded into a `typed_value` column.

use std::collections::{BTreeMap, HashSet};

use rustdb_common::libcxx::{UnorderedOrder, ci_hash};
use rustdb_common::{Error, Result};
use rustdb_types::variant::{VariantData, VariantLogicalType as V};
use rustdb_types::{LogicalType, LogicalTypeId, PhysicalType, Value};

/// `CalculateByteLength`: the bytes needed for `value` (at least 1).
fn byte_length(value: u64) -> usize {
    if value == 0 {
        return 1;
    }
    8 - (value.leading_zeros() as usize / 8)
}

fn push_le(out: &mut Vec<u8>, value: u64, bytes: usize) {
    out.extend_from_slice(&value.to_le_bytes()[..bytes]);
}

/// `CreateMetadata`: the dictionary of a row's keys (unsorted).
pub fn metadata_blob(keys: &[rustdb_common::ByteString]) -> Result<Vec<u8>> {
    let dictionary_size: u64 = keys.iter().map(|key| key.len() as u64).sum();
    if dictionary_size >= u64::from(u32::MAX) {
        return Err(Error::invalid_input(
            "The total length of the dictionary exceeds a 4 byte value (uint32_t), failed to export VARIANT to Parquet",
        ));
    }
    let width = byte_length(dictionary_size);
    let mut out = Vec::with_capacity(1 + width * (keys.len() + 2) + dictionary_size as usize);
    // Version 1, `sorted_strings` unset.
    out.push(1 | ((width as u8 - 1) << 6));
    push_le(&mut out, keys.len() as u64, width);
    let mut offset = 0u64;
    for key in keys {
        push_le(&mut out, offset, width);
        offset += key.len() as u64;
    }
    push_le(&mut out, offset, width);
    for key in keys {
        out.extend_from_slice(key.as_bytes());
    }
    Ok(out)
}

fn unsupported(type_id: V) -> Error {
    Error::invalid_input(format!(
        "Can't convert VARIANT of type '{}' to Parquet VARIANT",
        type_id.name()
    ))
}

fn primitive_header(primitive_type: u8) -> u8 {
    primitive_type << 2
}

/// The value of `index` as an `i64` for the unsigned and 128-bit integer conversions
/// (`Cast::Operation<SRC, int64_t>`).
fn int64_of(data: &VariantData, index: u32, type_id: V) -> Result<i64> {
    let value = data.to_sql_value(index)?;
    let wide = match type_id {
        V::Uint128 => value.as_u128().and_then(|value| i128::try_from(value).ok()),
        _ => value.as_i128(),
    };
    wide.and_then(|wide| i64::try_from(wide).ok()).ok_or_else(|| {
        Error::conversion(format!(
            "Type {} with value {} can't be cast because the value is out of range for the destination type INT64",
            value.logical_type(),
            value.to_varchar().unwrap_or_default()
        ))
    })
}

/// `WritePrimitiveValueData`: a scalar in the Parquet variant encoding.
fn encode_primitive(data: &VariantData, index: u32, type_id: V, out: &mut Vec<u8>) -> Result<()> {
    let sql = || data.to_sql_value(index);
    let missing = || Error::internal("VARIANT value without its payload");
    match type_id {
        V::Blob | V::Varchar => {
            let bytes = data.string(index)?;
            if type_id == V::Blob || bytes.len() > 64 {
                out.push(primitive_header(if type_id == V::Blob { 15 } else { 16 }));
                out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            } else {
                // A short string; like DuckDB the length is stored in 6 bits.
                out.push(1 | ((bytes.len() as u8) << 2));
            }
            out.extend_from_slice(bytes);
        }
        V::VariantNull => out.push(primitive_header(0)),
        V::BoolTrue => out.push(primitive_header(1)),
        V::BoolFalse => out.push(primitive_header(2)),
        V::Int8 => {
            out.push(primitive_header(3));
            out.extend_from_slice(&(sql()?.as_i128().ok_or_else(missing)? as i8).to_le_bytes());
        }
        V::Int16 => {
            out.push(primitive_header(4));
            out.extend_from_slice(&(sql()?.as_i128().ok_or_else(missing)? as i16).to_le_bytes());
        }
        V::Int32 => {
            out.push(primitive_header(5));
            out.extend_from_slice(&(sql()?.as_i128().ok_or_else(missing)? as i32).to_le_bytes());
        }
        V::Int64 => {
            out.push(primitive_header(6));
            out.extend_from_slice(&(sql()?.as_i128().ok_or_else(missing)? as i64).to_le_bytes());
        }
        V::Float => {
            out.push(primitive_header(14));
            out.extend_from_slice(&sql()?.as_f32().ok_or_else(missing)?.to_le_bytes());
        }
        V::Double => {
            out.push(primitive_header(7));
            out.extend_from_slice(&sql()?.as_f64().ok_or_else(missing)?.to_le_bytes());
        }
        V::Uuid => {
            out.push(primitive_header(20));
            let uuid = sql()?.as_uuid().ok_or_else(missing)?;
            // `BaseUUID::ToBlob`: big-endian with the sign bit flipped.
            out.extend_from_slice(&((uuid.0 as u128) ^ (1u128 << 127)).to_be_bytes());
        }
        V::Date => {
            out.push(primitive_header(11));
            out.extend_from_slice(&sql()?.as_date().ok_or_else(missing)?.0.to_le_bytes());
        }
        V::TimeMicros | V::TimestampMicros | V::TimestampNanos | V::TimestampMicrosTz => {
            let header = match type_id {
                V::TimeMicros => 17,
                V::TimestampMicros => 13,
                V::TimestampNanos => 19,
                _ => 12,
            };
            out.push(primitive_header(header));
            out.extend_from_slice(&(sql()?.as_i128().ok_or_else(missing)? as i64).to_le_bytes());
        }
        V::Decimal => {
            let (width, scale, unscaled) = data.decimal(index)?;
            if width > 38 {
                return Err(Error::invalid_input(format!(
                    "Can't convert VARIANT DECIMAL({width}, {scale}) to Parquet VARIANT"
                )));
            }
            let (header, size) = match width {
                0..=9 => (8, 4),
                10..=18 => (9, 8),
                _ => (10, 16),
            };
            out.push(primitive_header(header));
            out.push(scale as u8);
            out.extend_from_slice(&unscaled.to_le_bytes()[..size]);
        }
        V::Uint8 => {
            out.push(primitive_header(4));
            out.extend_from_slice(&i16::from(sql()?.as_u8().ok_or_else(missing)?).to_le_bytes());
        }
        V::Uint16 => {
            out.push(primitive_header(5));
            out.extend_from_slice(&i32::from(sql()?.as_u16().ok_or_else(missing)?).to_le_bytes());
        }
        V::Uint32 => {
            out.push(primitive_header(6));
            out.extend_from_slice(&i64::from(sql()?.as_u32().ok_or_else(missing)?).to_le_bytes());
        }
        V::Uint64 | V::Uint128 | V::Int128 => {
            let value = int64_of(data, index, type_id)?;
            out.push(primitive_header(6));
            out.extend_from_slice(&value.to_le_bytes());
        }
        _ => return Err(unsupported(type_id)),
    }
    Ok(())
}

/// The header and offsets of an OBJECT or ARRAY around its children's encodings.
fn encode_nested(
    out: &mut Vec<u8>,
    basic_type: u8,
    field_ids: Option<(&[u32], usize)>,
    children: &[Vec<u8>],
) {
    let total: usize = children.iter().map(Vec::len).sum();
    let offset_size = byte_length(total as u64);
    let count = children.len();
    let is_large = count > usize::from(u8::MAX);
    let header = match field_ids {
        Some((_, id_size)) => {
            basic_type
                | (u8::from(is_large) << 6)
                | ((id_size as u8 - 1) << 4)
                | ((offset_size as u8 - 1) << 2)
        }
        None => basic_type | (u8::from(is_large) << 4) | ((offset_size as u8 - 1) << 2),
    };
    out.push(header);
    if is_large {
        out.extend_from_slice(&(count as u32).to_le_bytes());
    } else {
        out.push(count as u8);
    }
    if let Some((ids, id_size)) = field_ids {
        for &id in ids {
            push_le(out, u64::from(id), id_size);
        }
    }
    let mut offset = 0;
    for child in children {
        push_le(out, offset as u64, offset_size);
        offset += child.len();
    }
    push_le(out, offset as u64, offset_size);
    for child in children {
        out.extend_from_slice(child);
    }
}

/// `AnalyzeValueData` + `WriteValueData`: the binary encoding of value `index` of a row
/// (`None` for a NULL row's variant null), leaving out the OBJECT fields in `excluded`;
/// `None` when every field of a non-empty OBJECT is left out.
#[allow(clippy::implicit_hasher, reason = "the excluded fields are built here")]
pub fn encode_value(
    data: Option<&VariantData>,
    index: u32,
    excluded: Option<&HashSet<rustdb_common::ByteString>>,
) -> Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let Some(data) = data else {
        out.push(primitive_header(0));
        return Ok(Some(out));
    };
    let type_id = data.type_id(index)?;
    match type_id {
        V::Object => {
            let nested = data.nested(index)?;
            let mut ids = Vec::new();
            let mut children = Vec::new();
            for position in 0..nested.child_count {
                let child = data.child(nested, position)?;
                if let Some(excluded) = excluded
                    && excluded.contains(&data.child_key(child)?.to_ascii_lowercase())
                {
                    continue;
                }
                let keys_index = child
                    .keys_index
                    .ok_or_else(|| Error::internal("VARIANT object child without a key"))?;
                ids.push(keys_index);
                let encoded = encode_value(Some(data), child.values_index, None)?
                    .ok_or_else(|| Error::internal("VARIANT child without an encoding"))?;
                children.push(encoded);
            }
            if nested.child_count > 0 && children.is_empty() {
                return Ok(None);
            }
            let highest = ids.iter().copied().max().unwrap_or(0);
            encode_nested(
                &mut out,
                2,
                Some((&ids, byte_length(u64::from(highest)))),
                &children,
            );
        }
        V::Array => {
            let nested = data.nested(index)?;
            let mut children = Vec::with_capacity(nested.child_count as usize);
            for position in 0..nested.child_count {
                let child = data.child(nested, position)?;
                let encoded = encode_value(Some(data), child.values_index, None)?
                    .ok_or_else(|| Error::internal("VARIANT child without an encoding"))?;
                children.push(encoded);
            }
            encode_nested(&mut out, 3, None, &children);
        }
        _ => encode_primitive(data, index, type_id, &mut out)?,
    }
    Ok(Some(out))
}

/// `TransformTypedValueRecursive`: the `typed_value` column type of a shredding type.
pub fn transform_typed_value(ty: &LogicalType) -> Result<LogicalType> {
    let group = |child: &LogicalType| -> Result<LogicalType> {
        let mut fields = vec![("value".into(), LogicalType::BLOB)];
        if child.id() != LogicalTypeId::Variant {
            fields.push(("typed_value".into(), transform_typed_value(child)?));
        }
        Ok(LogicalType::struct_type(fields))
    };
    match ty.id() {
        LogicalTypeId::Struct => {
            let fields = ty
                .struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, child)| Ok((name.clone(), group(child)?)))
                .collect::<Result<Vec<_>>>()?;
            Ok(LogicalType::struct_type(fields))
        }
        LogicalTypeId::List => {
            let child = ty.child_type().cloned().unwrap_or(LogicalType::INVALID);
            Ok(LogicalType::list(group(&child)?))
        }
        LogicalTypeId::Union | LogicalTypeId::Map | LogicalTypeId::Variant | LogicalTypeId::Array => {
            Err(Error::binder(format!(
                "'{ty}' can't appear inside a 'typed_value' shredded type!"
            )))
        }
        _ => Ok(ty.clone()),
    }
}

/// `GetVariantType`: the VARIANT types a shredding type takes.
fn variant_types(ty: &LogicalType) -> Result<Vec<V>> {
    use LogicalTypeId as L;
    Ok(match ty.id() {
        L::Any => Vec::new(),
        L::Struct => vec![V::Object],
        L::List => vec![V::Array],
        L::Boolean => vec![V::BoolTrue, V::BoolFalse],
        L::TinyInt => vec![V::Int8],
        L::SmallInt => vec![V::Int16],
        L::Integer => vec![V::Int32],
        L::BigInt => vec![V::Int64],
        L::Float => vec![V::Float],
        L::Double => vec![V::Double],
        L::Decimal => vec![V::Decimal],
        L::Date => vec![V::Date],
        L::Time => vec![V::TimeMicros],
        L::TimestampTz => vec![V::TimestampMicrosTz],
        L::Timestamp => vec![V::TimestampMicros],
        L::TimestampNs => vec![V::TimestampNanos],
        L::Blob => vec![V::Blob],
        L::Varchar => vec![V::Varchar],
        L::Uuid => vec![V::Uuid],
        _ => {
            return Err(Error::binder(format!(
                "Type '{ty}' can't be translated to a VARIANT type"
            )));
        }
    })
}

/// `VariantShreddingState::ValueIsShredded`.
fn is_shredded(data: &VariantData, index: u32, shredding: &LogicalType) -> Result<bool> {
    let type_id = data.type_id(index)?;
    if !variant_types(shredding)?.contains(&type_id) {
        return Ok(false);
    }
    if type_id == V::Decimal {
        let (width, scale, _) = data.decimal(index)?;
        let physical = LogicalType::decimal(width as u8, scale as u8).physical_type();
        return Ok(physical == shredding.physical_type());
    }
    Ok(true)
}

/// A value to write into a variant group: its row's data and value index, or a missing
/// field.
#[derive(Clone, Copy)]
enum Item<'a> {
    /// A NULL VARIANT row.
    Null,
    Value(&'a VariantData, u32),
    Missing,
}

/// `WriteVariantValues` for one value: the `{value, typed_value}` group of a value
/// shredded on `shredding` (whose group type is `group_type`).
fn write_group(item: Item<'_>, shredding: &LogicalType, group_type: &LogicalType) -> Result<Value> {
    let fields = group_type.struct_children().unwrap_or_default();
    let typed_type = fields
        .iter()
        .find(|(name, _)| name == "typed_value")
        .map(|(_, ty)| ty.clone());
    let null_typed = || typed_type.as_ref().map(|ty| Value::null(ty.clone()));
    let (value, typed) = match item {
        Item::Missing => (Value::null(LogicalType::BLOB), null_typed()),
        Item::Null => (blob(encode_value(None, 0, None)?), null_typed()),
        Item::Value(data, index) => match &typed_type {
            None => (blob(encode_value(Some(data), index, None)?), None),
            Some(typed_type) => {
                if is_shredded(data, index, shredding)? {
                    write_shredded(data, index, shredding, typed_type)?
                } else {
                    (
                        blob(encode_value(Some(data), index, None)?),
                        Some(Value::null(typed_type.clone())),
                    )
                }
            }
        },
    };
    let mut values = vec![value];
    if let Some(typed) = typed {
        values.push(typed);
    }
    Ok(Value::from_children(group_type.clone(), values))
}

fn blob(bytes: Option<Vec<u8>>) -> Value {
    bytes.map_or_else(|| Value::null(LogicalType::BLOB), |bytes| Value::blob(&bytes))
}

/// The `value` and `typed_value` of a value that matches its shredding type
/// (`WriteTypedValues`).
fn write_shredded(
    data: &VariantData,
    index: u32,
    shredding: &LogicalType,
    typed_type: &LogicalType,
) -> Result<(Value, Option<Value>)> {
    match shredding.id() {
        LogicalTypeId::Struct => {
            // Shredded fields leave the `value`; the rest stays there.
            let shredded_fields = shredding.struct_children().unwrap_or_default();
            let excluded: HashSet<rustdb_common::ByteString> = shredded_fields
                .iter()
                .map(|(name, _)| name.to_ascii_lowercase())
                .collect();
            let value = blob(encode_value(Some(data), index, Some(&excluded))?);
            let nested = data.nested(index)?;
            let group_types = typed_type.struct_children().unwrap_or_default();
            let mut typed_fields = Vec::with_capacity(shredded_fields.len());
            for ((name, field_shredding), (_, field_group)) in shredded_fields.iter().zip(group_types) {
                // `FindChildValues` by key.
                let mut item = Item::Missing;
                for position in 0..nested.child_count {
                    let child = data.child(nested, position)?;
                    if data.child_key(child)? == name {
                        item = Item::Value(data, child.values_index);
                        break;
                    }
                }
                let field_shredding = if field_shredding.id() == LogicalTypeId::Variant {
                    LogicalType::ANY
                } else {
                    field_shredding.clone()
                };
                typed_fields.push(write_group(item, &field_shredding, field_group)?);
            }
            Ok((
                value,
                Some(Value::from_children(typed_type.clone(), typed_fields)),
            ))
        }
        LogicalTypeId::List => {
            let element_shredding = shredding.child_type().cloned().unwrap_or(LogicalType::ANY);
            let element_shredding = if element_shredding.id() == LogicalTypeId::Variant {
                LogicalType::ANY
            } else {
                element_shredding
            };
            let element_group = typed_type.child_type().cloned().unwrap_or(LogicalType::INVALID);
            let nested = data.nested(index)?;
            let mut items = Vec::with_capacity(nested.child_count as usize);
            for position in 0..nested.child_count {
                let child = data.child(nested, position)?;
                items.push(write_group(
                    Item::Value(data, child.values_index),
                    &element_shredding,
                    &element_group,
                )?);
            }
            Ok((
                Value::null(LogicalType::BLOB),
                Some(Value::from_children(typed_type.clone(), items)),
            ))
        }
        _ => {
            // `WriteTypedPrimitiveValues`: the payload as the shredded type.
            let type_id = data.type_id(index)?;
            let typed = match type_id {
                V::BoolTrue => Value::boolean(true),
                V::BoolFalse => Value::boolean(false),
                V::Decimal => {
                    let (_, _, unscaled) = data.decimal(index)?;
                    let (width, scale) = shredding.decimal_properties().unwrap_or((18, 3));
                    Value::decimal(unscaled, width, scale)
                }
                _ => data.to_sql_value(index)?.reinterpret(shredding.clone()),
            };
            Ok((Value::null(LogicalType::BLOB), Some(typed)))
        }
    }
}

/// `ToParquetVariant`: the `{metadata, value[, typed_value]}` values of VARIANT rows
/// (`None` for NULL rows), shredded on `shredding` (`None` or ANY for no shredding).
pub fn to_parquet_variant(
    rows: &[Option<VariantData>],
    shredding: Option<&LogicalType>,
    struct_type: &LogicalType,
) -> Result<Vec<Value>> {
    let shredding = shredding.cloned().unwrap_or(LogicalType::ANY);
    let fields = struct_type.struct_children().unwrap_or_default();
    // The group type of `value` and `typed_value` (without `metadata`).
    let group_type = LogicalType::struct_type(fields.iter().skip(1).cloned().collect());
    // Reuse the batch schema; values carry no per-row field names. The public
    // conversion has historically returned an unaliased outer struct.
    let output_type = LogicalType::struct_type(fields.to_vec());
    rows.iter()
        .map(|row| {
            let keys: &[rustdb_common::ByteString] = row.as_ref().map_or(&[], |data| &data.keys);
            let metadata = Value::blob(&metadata_blob(keys)?);
            let item = row.as_ref().map_or(Item::Null, |data| Item::Value(data, 0));
            let group = write_group(item, &shredding, &group_type)?;
            let mut values = Vec::with_capacity(fields.len());
            values.push(metadata);
            values.extend_from_slice(group.children().unwrap_or_default());
            Ok(Value::from_children(output_type.clone(), values))
        })
        .collect()
}

/// The OBJECT fields of a VARIANT analysis (`case_insensitive_map_t<VariantAnalyzeData>`):
/// a case-insensitive map whose first spelling of a key names the field, iterated in
/// the order of the C++ standard library's `unordered_map` (libc++, which DuckDB's
/// builds on this platform use), since that order picks the shredded field order.
#[derive(Debug, Default)]
struct ObjectFields {
    /// The fields in insertion order, with their hashes.
    entries: Vec<(rustdb_common::ByteString, u64, AnalyzeData)>,
    /// The position in `entries` of each lowercased key.
    positions: std::collections::HashMap<rustdb_common::ByteString, usize>,
}

impl ObjectFields {
    /// `operator[]`: the field of `key`, inserted if new.
    fn entry(&mut self, key: &rustdb_common::ByteString) -> &mut AnalyzeData {
        let lower = key.to_ascii_lowercase();
        let position = if let Some(&position) = self.positions.get(&lower) {
            position
        } else {
            self.entries
                .push((key.to_owned(), ci_hash(key), AnalyzeData::default()));
            let position = self.entries.len() - 1;
            self.positions.insert(lower, position);
            position
        };
        &mut self.entries[position].2
    }

    /// The fields in the iteration order of libc++'s `unordered_map` after inserting
    /// them in this order.
    fn iter_in_map_order(&self) -> impl Iterator<Item = (&rustdb_common::ByteString, &AnalyzeData)> {
        let mut table = UnorderedOrder::default();
        for (position, (_, hash, _)) in self.entries.iter().enumerate() {
            table.insert(*hash, position, |a, b| a == b);
        }
        table
            .order()
            .into_iter()
            .map(|position| (&self.entries[position].0, &self.entries[position].2))
    }
}

/// The type statistics of a VARIANT column (`VariantAnalyzeData`).
#[derive(Debug, Default)]
pub struct AnalyzeData {
    type_map: BTreeMap<u8, u64>,
    decimal_width: u32,
    decimal_scale: u32,
    decimal_consistent: bool,
    total_count: u64,
    object: Option<ObjectFields>,
    array: Option<Box<AnalyzeData>>,
}

impl AnalyzeData {
    fn count(&self, type_id: V) -> u64 {
        self.type_map.get(&(type_id as u8)).copied().unwrap_or(0)
    }

    fn bump(&mut self, type_id: V, delta: i64) {
        let entry = self.type_map.entry(type_id as u8).or_insert(0);
        *entry = entry.saturating_add_signed(delta);
    }

    /// `AnalyzeSchemaInternal`.
    pub fn analyze(&mut self, data: Option<&VariantData>, index: u32) -> Result<()> {
        self.total_count += 1;
        let Some(data) = data else {
            self.bump(V::VariantNull, 1);
            return Ok(());
        };
        let type_id = data.type_id(index)?;
        self.bump(type_id, 1);
        match type_id {
            V::Object => {
                let fields = self.object.get_or_insert_with(ObjectFields::default);
                let nested = data.nested(index)?;
                for position in 0..nested.child_count {
                    let child = data.child(nested, position)?;
                    let key = data.child_key(child)?;
                    fields.entry(key).analyze(Some(data), child.values_index)?;
                }
            }
            V::Array => {
                let element = self.array.get_or_insert_with(Box::default);
                let nested = data.nested(index)?;
                for position in 0..nested.child_count {
                    element.analyze(Some(data), data.child(nested, position)?.values_index)?;
                }
            }
            V::Decimal => {
                let (width, scale, _) = data.decimal(index)?;
                if self.count(V::Decimal) == 1 {
                    self.decimal_width = width;
                    self.decimal_scale = scale;
                    self.decimal_consistent = true;
                } else if self.decimal_consistent
                    && (width != self.decimal_width || scale != self.decimal_scale)
                {
                    self.decimal_consistent = false;
                }
            }
            V::BoolFalse => {
                self.bump(V::BoolTrue, 1);
                self.bump(V::BoolFalse, -1);
            }
            _ => {}
        }
        Ok(())
    }

    /// `ConstructShreddedType`: the type to shred on, if any.
    pub fn shredded_type(&self) -> Option<LogicalType> {
        if self.count(V::VariantNull) == self.total_count {
            return Some(LogicalType::INTEGER);
        }
        let candidates = [
            (V::BoolTrue, LogicalType::BOOLEAN),
            (V::Int8, LogicalType::TINYINT),
            (V::Int16, LogicalType::SMALLINT),
            (V::Int32, LogicalType::INTEGER),
            (V::Int64, LogicalType::BIGINT),
            (V::Float, LogicalType::FLOAT),
            (V::Double, LogicalType::DOUBLE),
            (V::Decimal, LogicalType::INVALID),
            (V::Date, LogicalType::DATE),
            (V::TimeMicros, LogicalType::TIME),
            (V::TimestampMicros, LogicalType::TIMESTAMP),
            (V::TimestampNanos, LogicalType::TIMESTAMP_NS),
            (V::TimestampMicrosTz, LogicalType::TIMESTAMP_TZ),
            (V::Blob, LogicalType::BLOB),
            (V::Varchar, LogicalType::VARCHAR),
            (V::Uuid, LogicalType::UUID),
            (V::Uint8, LogicalType::SMALLINT),
            (V::Uint16, LogicalType::INTEGER),
            (V::Uint32, LogicalType::BIGINT),
            (V::Uint64, LogicalType::BIGINT),
            (V::Uint128, LogicalType::BIGINT),
            (V::Int128, LogicalType::BIGINT),
        ];
        let mut highest = 0;
        let mut best = None;
        for (type_id, ty) in candidates {
            let count = self.count(type_id);
            if count <= highest {
                continue;
            }
            if type_id == V::Decimal {
                if !self.decimal_consistent {
                    continue;
                }
                highest = count;
                best = Some(LogicalType::decimal(
                    self.decimal_width as u8,
                    self.decimal_scale as u8,
                ));
            } else {
                highest = count;
                best = Some(ty);
            }
        }
        let arrays = self.count(V::Array);
        let objects = self.count(V::Object);
        if arrays > objects {
            if arrays > highest {
                let element = self.array.as_ref()?.shredded_type()?;
                return Some(LogicalType::list(element));
            }
        } else if objects > highest {
            let fields: Vec<(rustdb_common::ByteString, LogicalType)> = self
                .object
                .as_ref()?
                .iter_in_map_order()
                .filter_map(|(name, field)| field.shredded_type().map(|ty| (name.to_owned(), ty)))
                .collect();
            if fields.is_empty() {
                return None;
            }
            return Some(LogicalType::struct_type(fields));
        }
        best
    }
}

/// `ConvertShreddingTypeRecursive`'s check of an explicit `SHREDDING` type.
pub fn check_shredding_type(ty: &LogicalType) -> Result<()> {
    match ty.id() {
        LogicalTypeId::Variant => Ok(()),
        LogicalTypeId::Struct => ty
            .struct_children()
            .unwrap_or_default()
            .iter()
            .try_for_each(|(_, child)| check_shredding_type(child)),
        LogicalTypeId::List => check_shredding_type(ty.child_type().unwrap_or(&LogicalType::INVALID)),
        _ if !ty.is_nested() => Ok(()),
        _ => Err(Error::binder(format!(
            "VARIANT can only be shredded on LIST/STRUCT/ANY/non-nested type, not {ty}"
        ))),
    }
}

/// Whether a physical type is one shredded decimals use.
pub fn is_decimal_physical(physical: PhysicalType) -> bool {
    matches!(
        physical,
        PhysicalType::Int16 | PhysicalType::Int32 | PhysicalType::Int64 | PhysicalType::Int128
    )
}

#[cfg(test)]
mod schema_reuse_tests {
    use super::*;
    use rustdb_types::variant_value::{VariantValue as VV, to_variant_data};

    #[test]
    fn vector_input_matches_value_input_for_nested_selections_and_constants() {
        use rustdb_types::{SelectionVector, Vector};
        let rows = to_variant_data(&[
            VV::Missing,
            VV::Primitive(Value::integer(7)),
            VV::Object(
                [(
                    "raw".into(),
                    VV::Array(vec![VV::null(), VV::Primitive(Value::blob(&[0, 255]))]),
                )]
                .into_iter()
                .collect(),
            ),
            VV::Array(vec![
                VV::Primitive(Value::varchar("text")),
                VV::Primitive(Value::boolean(false)),
            ]),
        ])
        .unwrap();
        let ty = LogicalType::variant();
        let flat = VariantData::write_vector(&rows, &ty).unwrap();
        let first = flat.slice(&SelectionVector::new(vec![3, 0, 2, 1, 2, 0]));
        let second = first.slice(&SelectionVector::new(vec![5, 4, 0, 2, 3, 1]));
        let constant = Vector::constant(&flat.get_value(2)).unwrap();
        let repeated = flat.reference_row(2, 6).unwrap();
        for (vector, count) in [
            (&flat, 4),
            (&first, 6),
            (&second, 6),
            (&constant, 6),
            (&repeated, 6),
        ] {
            for row in 0..count {
                assert_eq!(
                    VariantData::from_vector(vector, row).unwrap(),
                    VariantData::from_value(&vector.get_value(row)).unwrap()
                );
            }
        }
    }

    #[test]
    fn parquet_variant_preserves_null_encoding_and_outer_alias_behavior() {
        let ty = LogicalType::struct_type(vec![
            ("metadata".into(), LogicalType::BLOB),
            ("value".into(), LogicalType::BLOB),
        ])
        .with_alias("PARQUET_VARIANT");
        let rows = to_variant_data(&[VV::null(), VV::Primitive(Value::boolean(true))]).unwrap();
        let output = to_parquet_variant(&rows, None, &ty).unwrap();
        let expected = vec![
            Value::struct_value(vec![
                ("metadata".into(), Value::blob(&[1, 0, 0])),
                ("value".into(), Value::blob(&[0])),
            ]),
            Value::struct_value(vec![
                ("metadata".into(), Value::blob(&[1, 0, 0])),
                ("value".into(), Value::blob(&[4])),
            ]),
        ];
        assert_eq!(output, expected);
        let shredded = LogicalType::struct_type(vec![
            ("metadata".into(), LogicalType::BLOB),
            ("value".into(), LogicalType::BLOB),
            ("typed_value".into(), LogicalType::BOOLEAN),
        ]);
        let output = to_parquet_variant(&rows, Some(&LogicalType::BOOLEAN), &shredded).unwrap();
        assert_eq!(
            output,
            vec![
                Value::struct_value(vec![
                    ("metadata".into(), Value::blob(&[1, 0, 0])),
                    ("value".into(), Value::blob(&[0])),
                    ("typed_value".into(), Value::null(LogicalType::BOOLEAN))
                ]),
                Value::struct_value(vec![
                    ("metadata".into(), Value::blob(&[1, 0, 0])),
                    ("value".into(), Value::null(LogicalType::BLOB)),
                    ("typed_value".into(), Value::boolean(true))
                ]),
            ]
        );
    }
}
