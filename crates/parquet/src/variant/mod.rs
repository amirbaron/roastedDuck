//! VARIANT columns: the Parquet variant binary encoding (`variant_binary_decoder.cpp`),
//! shredded variants (`variant_shredded_conversion.cpp`) and the conversion of decoded
//! values into DuckDB's VARIANT layout (`VariantValue::ToVARIANT`).

pub mod read;
pub mod write;

use rustdb_function::{NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{LogicalType, Vector};

use std::collections::BTreeMap;

use rustdb_common::{Error, ErrorKind, Result};
use rustdb_types::variant::{
    VariantChild, VariantData, VariantEntry, VariantLogicalType, varint_encode, varint_size,
};
use rustdb_types::{LogicalTypeId, PhysicalType, Value};

/// A decoded variant value (DuckDB's `VariantValue`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum VariantValue {
    /// No value (a field absent from a shredded object).
    #[default]
    Missing,
    /// A scalar; a NULL `Value` of type `"NULL"` is the variant null.
    Primitive(Value),
    /// Fields by name, in byte order of the names.
    Object(BTreeMap<String, VariantValue>),
    Array(Vec<VariantValue>),
}

impl VariantValue {
    /// The variant null (`VariantValue::NullValue`).
    pub fn null() -> Self {
        Self::Primitive(Value::default())
    }

    fn is_null(&self) -> bool {
        matches!(self, Self::Primitive(value) if value.is_null())
    }

    /// `AddChild`: missing values are left out; the first value of a key wins.
    pub fn add_child(fields: &mut BTreeMap<String, VariantValue>, key: &str, value: VariantValue) {
        if matches!(value, Self::Missing) {
            return;
        }
        fields.entry(key.to_owned()).or_insert(value);
    }

    /// `SetItems`: missing array items are variant nulls.
    pub fn array(items: Vec<VariantValue>) -> Self {
        Self::Array(
            items
                .into_iter()
                .map(|item| {
                    if matches!(item, Self::Missing) {
                        Self::null()
                    } else {
                        item
                    }
                })
                .collect(),
        )
    }
}

fn io_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, message.into())
}

fn corrupt_value() -> Error {
    io_error("Corrupted VARIANT 'value' buffer")
}

/// `ReadVariableLengthLittleEndian`.
fn read_le(length_in_bytes: usize, data: &[u8], offset: &mut usize) -> Result<usize> {
    if length_in_bytes > 8 {
        return Err(Error::not_implemented(format!(
            "Can't read little-endian value of {length_in_bytes} bytes"
        )));
    }
    let end = offset
        .checked_add(length_in_bytes)
        .filter(|&end| end <= data.len());
    let Some(end) = end else {
        return Err(io_error(format!(
            "Data corruption detected, read of length_in_bytes ({length_in_bytes}) would exceed buffer capacity"
        )));
    };
    let mut buffer = [0u8; 8];
    buffer[..length_in_bytes].copy_from_slice(&data[*offset..end]);
    *offset = end;
    Ok(u64::from_le_bytes(buffer) as usize)
}

/// The dictionary of a variant `metadata` buffer (`VariantMetadata`).
#[derive(Debug)]
pub struct VariantMetadata {
    pub strings: Vec<String>,
    /// The length of the metadata in its buffer (`total_size`).
    pub total_size: usize,
}

impl VariantMetadata {
    /// Parses a `metadata` buffer.
    pub fn new(metadata: &[u8]) -> Result<Self> {
        let Some(&header) = metadata.first() else {
            return Err(io_error("Corrupted VARIANT 'metadata' buffer, empty or nullptr"));
        };
        let version = header & 0xF;
        if version != 1 {
            return Err(Error::not_implemented(format!(
                "Only version 1 of the Variant encoding scheme is supported, found version: {version}"
            )));
        }
        let offset_size = usize::from((header >> 6) & 0x3) + 1;
        let mut position = 1;
        let dictionary_size = read_le(offset_size, metadata, &mut position)?;
        let data_start = dictionary_size
            .checked_add(1)
            .and_then(|count| count.checked_mul(offset_size))
            .and_then(|size| size.checked_add(position))
            .ok_or_else(|| io_error("Corrupted VARIANT 'metadata' buffer"))?;
        let mut last_offset = read_le(offset_size, metadata, &mut position)?;
        let mut strings = Vec::with_capacity(dictionary_size.min(metadata.len()));
        for _ in 0..dictionary_size {
            let next_offset = read_le(offset_size, metadata, &mut position)?;
            let string_size = next_offset.wrapping_sub(last_offset);
            let start = data_start.checked_add(last_offset);
            let end = start.and_then(|start| start.checked_add(string_size));
            let Some(bytes) = start.zip(end).and_then(|(start, end)| metadata.get(start..end)) else {
                return Err(io_error("Corrupted VARIANT 'metadata' buffer"));
            };
            strings.push(String::from_utf8_lossy(bytes).into_owned());
            last_offset = next_offset;
        }
        Ok(Self {
            strings,
            total_size: position + last_offset,
        })
    }
}

/// The value of `bytes[offset..offset + N]` as an array.
fn fixed<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    offset
        .checked_add(N)
        .and_then(|end| data.get(offset..end))
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(corrupt_value)
}

/// The number of decimal digits of `value` (`floor(log10(|value|)) + 1`), 0 for 0.
fn decimal_digits(value: i64) -> u8 {
    let mut magnitude = value.unsigned_abs();
    let mut digits = 0;
    while magnitude > 0 {
        magnitude /= 10;
        digits += 1;
    }
    digits
}

fn checked_string(bytes: &[u8]) -> Result<Value> {
    if std::str::from_utf8(bytes).is_err() {
        return Err(io_error(
            "Can't decode Variant short-string, string isn't valid UTF8",
        ));
    }
    Ok(Value::bytes(rustdb_types::LogicalType::VARCHAR, bytes))
}

/// `VariantBinaryDecoder::PrimitiveTypeDecode`.
fn decode_primitive(primitive_type: u8, data: &[u8], offset: usize) -> Result<VariantValue> {
    use rustdb_types::{Date, Time, Timestamp, Uuid};
    let value = match primitive_type {
        0 => return Ok(VariantValue::null()),
        1 => Value::boolean(true),
        2 => Value::boolean(false),
        3 => Value::tinyint(i8::from_le_bytes(fixed(data, offset)?)),
        4 => Value::smallint(i16::from_le_bytes(fixed(data, offset)?)),
        5 => Value::integer(i32::from_le_bytes(fixed(data, offset)?)),
        6 => Value::bigint(i64::from_le_bytes(fixed(data, offset)?)),
        7 => Value::double(f64::from_le_bytes(fixed(data, offset)?)),
        8 => {
            if offset + 5 > data.len() {
                return Err(corrupt_value());
            }
            let scale = data[offset];
            let unscaled = i32::from_le_bytes(fixed(data, offset + 1)?);
            Value::decimal(i128::from(unscaled), decimal_digits(i64::from(unscaled)), scale)
        }
        9 => {
            if offset + 9 > data.len() {
                return Err(corrupt_value());
            }
            let scale = data[offset];
            let unscaled = i64::from_le_bytes(fixed(data, offset + 1)?);
            Value::decimal(i128::from(unscaled), decimal_digits(unscaled), scale)
        }
        10 => {
            if offset + 17 > data.len() {
                return Err(corrupt_value());
            }
            let scale = data[offset];
            let unscaled = i128::from_le_bytes(fixed(data, offset + 1)?);
            Value::decimal(unscaled, 38, scale)
        }
        11 => Value::date(Date(i32::from_le_bytes(fixed(data, offset)?))),
        12 => Value::timestamp_tz(Timestamp(i64::from_le_bytes(fixed(data, offset)?))),
        13 => Value::timestamp(Timestamp(i64::from_le_bytes(fixed(data, offset)?))),
        14 => Value::float(f32::from_le_bytes(fixed(data, offset)?)),
        15 | 16 => {
            let size = u32::from_le_bytes(fixed(data, offset)?) as usize;
            let start = offset + 4;
            let bytes = start
                .checked_add(size)
                .and_then(|end| data.get(start..end))
                .ok_or_else(corrupt_value)?;
            if primitive_type == 15 {
                Value::blob(bytes)
            } else {
                checked_string(bytes)?
            }
        }
        17 => Value::time(Time(i64::from_le_bytes(fixed(data, offset)?))),
        18 => {
            // Nanoseconds are cut to microseconds (`Timestamp::FromEpochNanoSeconds`).
            let nanos = i64::from_le_bytes(fixed(data, offset)?);
            Value::timestamp_tz(Timestamp(nanos / 1000))
        }
        19 => Value::timestamp_ns(i64::from_le_bytes(fixed(data, offset)?)),
        20 => {
            let bytes: [u8; 16] = fixed(data, offset)?;
            // `ReadParquetUUID`: big-endian bytes with the sign bit flipped.
            Value::uuid(Uuid((u128::from_be_bytes(bytes) ^ (1u128 << 127)) as i128))
        }
        other => {
            return Err(Error::not_implemented(format!(
                "Variant PrimitiveType ({other}) is not supported"
            )));
        }
    };
    Ok(VariantValue::Primitive(value))
}

/// `VariantBinaryDecoder::Decode`: the value at `offset` of `data`.
pub fn decode(metadata: &VariantMetadata, data: &[u8], offset: usize) -> Result<VariantValue> {
    let Some(&header) = data.get(offset) else {
        return Err(corrupt_value());
    };
    let offset = offset + 1;
    let value_header = header >> 2;
    match header & 0x3 {
        0 => decode_primitive(value_header, data, offset),
        1 => {
            let size = usize::from(value_header);
            let bytes = data.get(offset..offset + size).ok_or_else(corrupt_value)?;
            Ok(VariantValue::Primitive(checked_string(bytes)?))
        }
        2 => {
            let field_offset_size = usize::from(value_header & 0x3) + 1;
            let field_id_size = usize::from((value_header >> 2) & 0x3) + 1;
            let is_large = (value_header >> 4) & 0x1 != 0;
            let (count, offset) = element_count(data, offset, is_large)?;
            let mut field_ids_offset = offset;
            let mut field_offsets_offset = count
                .checked_mul(field_id_size)
                .and_then(|size| size.checked_add(offset))
                .ok_or_else(corrupt_value)?;
            let values_offset = count
                .checked_add(1)
                .and_then(|count| count.checked_mul(field_offset_size))
                .and_then(|size| size.checked_add(field_offsets_offset))
                .ok_or_else(corrupt_value)?;
            let mut last_offset = read_le(field_offset_size, data, &mut field_offsets_offset)?;
            let mut fields = BTreeMap::new();
            for _ in 0..count {
                let field_id = read_le(field_id_size, data, &mut field_ids_offset)?;
                let next_offset = read_le(field_offset_size, data, &mut field_offsets_offset)?;
                let value = decode(metadata, data, values_offset.saturating_add(last_offset))?;
                let key = metadata.strings.get(field_id).ok_or_else(corrupt_value)?;
                VariantValue::add_child(&mut fields, key, value);
                last_offset = next_offset;
            }
            Ok(VariantValue::Object(fields))
        }
        _ => {
            let field_offset_size = usize::from(value_header & 0x3) + 1;
            let is_large = (value_header >> 2) & 0x1 != 0;
            let (count, offset) = element_count(data, offset, is_large)?;
            let mut field_offsets_offset = offset;
            let values_offset = count
                .checked_add(1)
                .and_then(|count| count.checked_mul(field_offset_size))
                .and_then(|size| size.checked_add(offset))
                .ok_or_else(corrupt_value)?;
            let mut last_offset = read_le(field_offset_size, data, &mut field_offsets_offset)?;
            let mut items = Vec::with_capacity(count.min(data.len()));
            for _ in 0..count {
                let next_offset = read_le(field_offset_size, data, &mut field_offsets_offset)?;
                items.push(decode(metadata, data, values_offset.saturating_add(last_offset))?);
                last_offset = next_offset;
            }
            Ok(VariantValue::array(items))
        }
    }
}

/// The element count of an OBJECT or ARRAY: 1 or 4 bytes.
fn element_count(data: &[u8], offset: usize, is_large: bool) -> Result<(usize, usize)> {
    if is_large {
        Ok((u32::from_le_bytes(fixed(data, offset)?) as usize, offset + 4))
    } else {
        let count = *data.get(offset).ok_or_else(corrupt_value)?;
        Ok((usize::from(count), offset + 1))
    }
}

/// `VariantValue::ToVARIANT`: the VARIANT value of each decoded value; `None` for NULL
/// and missing values.
pub fn to_variant(values: &[VariantValue]) -> Result<Vec<Option<Value>>> {
    values
        .iter()
        .map(|value| {
            if matches!(value, VariantValue::Missing) || value.is_null() {
                return Ok(None);
            }
            let mut data = VariantData::default();
            convert_value(value, &mut data)?;
            Ok(Some(data.to_value()))
        })
        .collect()
}

fn u32_of(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::internal("VARIANT exceeds 4 GiB"))
}

/// `ConvertValue`: appends `value` (and its children) to the row's lists.
fn convert_value(value: &VariantValue, data: &mut VariantData) -> Result<()> {
    let entry = |data: &VariantData, type_id: VariantLogicalType| -> Result<VariantEntry> {
        Ok(VariantEntry {
            type_id: type_id as u8,
            byte_offset: u32_of(data.data.len())?,
        })
    };
    match value {
        VariantValue::Missing => Err(Error::internal("VariantValueType not handled")),
        VariantValue::Object(fields) => {
            let object = entry(data, VariantLogicalType::Object)?;
            data.values.push(object);
            varint_encode(fields.len() as u64, &mut data.data);
            if !fields.is_empty() {
                let start = data.children.len();
                varint_encode(start as u64, &mut data.data);
                data.children
                    .resize(start + fields.len(), VariantChild::default());
                for (index, (key, child)) in fields.iter().enumerate() {
                    data.children[start + index] = VariantChild {
                        keys_index: Some(u32_of(data.keys.len())?),
                        values_index: u32_of(data.values.len())?,
                    };
                    data.keys.push(key.clone());
                    convert_value(child, data)?;
                }
            }
            Ok(())
        }
        VariantValue::Array(items) => {
            let array = entry(data, VariantLogicalType::Array)?;
            data.values.push(array);
            varint_encode(items.len() as u64, &mut data.data);
            if !items.is_empty() {
                let start = data.children.len();
                varint_encode(start as u64, &mut data.data);
                data.children.resize(start + items.len(), VariantChild::default());
                for (index, item) in items.iter().enumerate() {
                    data.children[start + index] = VariantChild {
                        keys_index: None,
                        values_index: u32_of(data.values.len())?,
                    };
                    convert_value(item, data)?;
                }
            }
            Ok(())
        }
        VariantValue::Primitive(primitive) => convert_primitive(primitive, data),
    }
}

/// The primitive case of `ConvertValue`: the type tag and the payload.
fn convert_primitive(value: &Value, data: &mut VariantData) -> Result<()> {
    use LogicalTypeId as L;
    use VariantLogicalType as V;
    let ty = value.logical_type();
    let missing = || Error::internal(format!("VARIANT primitive of type {ty} without a value"));
    let byte_offset = u32_of(data.data.len())?;
    let mut push = |type_id: V, payload: &[u8]| {
        data.values.push(VariantEntry {
            type_id: type_id as u8,
            byte_offset,
        });
        data.data.extend_from_slice(payload);
    };
    if value.is_null() {
        push(V::VariantNull, &[]);
        return Ok(());
    }
    let int = || value.as_i128().ok_or_else(missing);
    match ty.id() {
        L::Boolean => {
            let flag = value.as_bool().ok_or_else(missing)?;
            push(if flag { V::BoolTrue } else { V::BoolFalse }, &[]);
        }
        L::SqlNull => push(V::VariantNull, &[]),
        L::TinyInt => push(V::Int8, &(int()? as i8).to_le_bytes()),
        L::SmallInt => push(V::Int16, &(int()? as i16).to_le_bytes()),
        L::Integer => push(V::Int32, &(int()? as i32).to_le_bytes()),
        L::BigInt => push(V::Int64, &(int()? as i64).to_le_bytes()),
        L::HugeInt => push(V::Int128, &int()?.to_le_bytes()),
        L::UTinyInt => push(V::Uint8, &(int()? as u8).to_le_bytes()),
        L::USmallInt => push(V::Uint16, &(int()? as u16).to_le_bytes()),
        L::UInteger => push(V::Uint32, &(int()? as u32).to_le_bytes()),
        L::UBigInt => push(V::Uint64, &(int()? as u64).to_le_bytes()),
        L::UHugeInt => push(V::Uint128, &value.as_u128().ok_or_else(missing)?.to_le_bytes()),
        L::Double => push(V::Double, &value.as_f64().ok_or_else(missing)?.to_le_bytes()),
        L::Float => push(V::Float, &value.as_f32().ok_or_else(missing)?.to_le_bytes()),
        L::Date => push(V::Date, &value.as_date().ok_or_else(missing)?.0.to_le_bytes()),
        L::TimestampTz => push(
            V::TimestampMicrosTz,
            &value.as_timestamp().ok_or_else(missing)?.0.to_le_bytes(),
        ),
        L::Timestamp => push(
            V::TimestampMicros,
            &value.as_timestamp().ok_or_else(missing)?.0.to_le_bytes(),
        ),
        L::TimestampSec => push(V::TimestampSec, &(int()? as i64).to_le_bytes()),
        L::TimestampMs => push(V::TimestampMilis, &(int()? as i64).to_le_bytes()),
        L::TimestampNs => push(V::TimestampNanos, &(int()? as i64).to_le_bytes()),
        L::Time => push(
            V::TimeMicros,
            &value.as_time().ok_or_else(missing)?.0.to_le_bytes(),
        ),
        L::TimeNs => push(V::TimeNanos, &(int()? as i64).to_le_bytes()),
        L::TimeTz => push(V::TimeMicrosTz, &(int()? as i64).to_le_bytes()),
        L::Interval => {
            let interval = value.as_interval().ok_or_else(missing)?;
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(&interval.months.to_le_bytes());
            bytes.extend_from_slice(&interval.days.to_le_bytes());
            bytes.extend_from_slice(&interval.micros.to_le_bytes());
            push(V::Interval, &bytes);
        }
        L::Uuid => push(V::Uuid, &value.as_uuid().ok_or_else(missing)?.0.to_le_bytes()),
        L::Decimal => {
            let (width, scale) = ty.decimal_properties().ok_or_else(missing)?;
            let unscaled = int()?;
            let mut bytes =
                Vec::with_capacity(varint_size(u64::from(width)) + varint_size(u64::from(scale)) + 16);
            varint_encode(u64::from(width), &mut bytes);
            varint_encode(u64::from(scale), &mut bytes);
            match ty.physical_type() {
                PhysicalType::Int16 => bytes.extend_from_slice(&(unscaled as i16).to_le_bytes()),
                PhysicalType::Int32 => bytes.extend_from_slice(&(unscaled as i32).to_le_bytes()),
                PhysicalType::Int64 => bytes.extend_from_slice(&(unscaled as i64).to_le_bytes()),
                _ => bytes.extend_from_slice(&unscaled.to_le_bytes()),
            }
            push(V::Decimal, &bytes);
        }
        L::Blob | L::Bignum | L::Bit | L::Geometry | L::Varchar => {
            let type_id = match ty.id() {
                L::Blob => V::Blob,
                L::Bignum => V::Bignum,
                L::Bit => V::Bitstring,
                L::Geometry => V::Geometry,
                _ => V::Varchar,
            };
            let bytes = value.as_bytes().ok_or_else(missing)?;
            let mut payload = Vec::with_capacity(bytes.len() + 5);
            varint_encode(bytes.len() as u64, &mut payload);
            payload.extend_from_slice(bytes);
            push(type_id, &payload);
        }
        _ => {
            return Err(Error::internal(format!(
                "Encountered unrecognized LogicalType in VariantValue::ConvertValue: {ty}"
            )));
        }
    }
    Ok(())
}

/// `GetParquetVariantType`: the Parquet layout of an unshredded VARIANT.
fn parquet_variant_type() -> LogicalType {
    LogicalType::struct_type(vec![
        ("metadata".to_owned(), LogicalType::BLOB),
        ("value".to_owned(), LogicalType::BLOB),
    ])
    .with_alias("PARQUET_VARIANT")
}

/// `variant_to_parquet_variant(VARIANT)` and `variant_bytes_to_variant(BLOB)`.
pub fn functions() -> Vec<ScalarFunctionSet> {
    let mut to_parquet = ScalarFunctionSet::new("variant_to_parquet_variant");
    let mut function = ScalarFunction::new(
        "variant_to_parquet_variant",
        vec![LogicalType::variant()],
        parquet_variant_type(),
        |args, _, result| {
            let input = args.column(0);
            let count = args.len();
            let rows = (0..count)
                .map(|row| VariantData::from_value(&input.get_value(row)))
                .collect::<Result<Vec<_>>>()?;
            let ty = parquet_variant_type();
            let values = write::to_parquet_variant(&rows, None, &ty)?;
            let mut output = Vector::flat(ty, count.max(1));
            for (row, value) in values.iter().enumerate() {
                output.set_value(row, value)?;
            }
            *result = output;
            Ok(())
        },
    );
    function.null_handling = NullHandling::Special;
    to_parquet.add(function);

    let mut from_bytes = ScalarFunctionSet::new("variant_bytes_to_variant");
    let mut function = ScalarFunction::new(
        "variant_bytes_to_variant",
        vec![LogicalType::BLOB],
        LogicalType::variant(),
        |args, _, result| {
            // `ConvertBinaryToVariant`: the value follows the metadata in each blob.
            let input = args.column(0);
            let count = args.len();
            let mut values = Vec::with_capacity(count);
            for row in 0..count {
                if input.is_null(row) {
                    values.push(VariantValue::Missing);
                    continue;
                }
                let bytes = input.string_bytes(row);
                let metadata = VariantMetadata::new(bytes)?;
                values.push(decode(&metadata, bytes, metadata.total_size)?);
            }
            let mut output = Vector::flat(LogicalType::variant(), count.max(1));
            for (row, variant) in to_variant(&values)?.into_iter().enumerate() {
                let value = variant.unwrap_or_else(|| Value::null(LogicalType::variant()));
                output.set_value(row, &value)?;
            }
            *result = output;
            Ok(())
        },
    );
    function.null_handling = NullHandling::Special;
    from_bytes.add(function);
    vec![to_parquet, from_bytes]
}
