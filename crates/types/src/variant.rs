//! The VARIANT type (DuckDB's `common/types/variant.hpp`, `variant_value_convert.hpp`,
//! the `to_variant` casts and `variant_normalize`).
//!
//! A VARIANT value is stored as
//! `STRUCT(keys VARCHAR[], children STRUCT(keys_index UINTEGER, values_index UINTEGER)[],
//! values STRUCT(type_id UTINYINT, byte_offset UINTEGER)[], data BLOB)`: `values` lists
//! every (nested) value of the row with its [`VariantLogicalType`] and the offset of its
//! payload in `data`; value 0 is the root. OBJECT and ARRAY payloads are a child count
//! and the index of their first entry in `children`, whose entries point at a key (OBJECT
//! only) and a value. Integers in `data` are LEB128 varints.
//!
//! [`encode`] converts a batch of [`Value`]s column-wise exactly like DuckDB's
//! `ConvertToVariant`, so that the layout (and therefore hashing and grouping) matches.

use rustdb_common::ByteString;
use std::collections::BTreeMap;

use rustdb_common::{Error, Result};

use crate::{
    Date, Interval, ListEntry, LogicalType, LogicalTypeId, PhysicalType, StringT, Time, TimeNs, TimeTz,
    Timestamp, Uuid, Value, Vector,
};

/// The type tag of a value inside a VARIANT (DuckDB's `VariantLogicalType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VariantLogicalType {
    VariantNull = 0,
    BoolTrue = 1,
    BoolFalse = 2,
    Int8 = 3,
    Int16 = 4,
    Int32 = 5,
    Int64 = 6,
    Int128 = 7,
    Uint8 = 8,
    Uint16 = 9,
    Uint32 = 10,
    Uint64 = 11,
    Uint128 = 12,
    Float = 13,
    Double = 14,
    Decimal = 15,
    Varchar = 16,
    Blob = 17,
    Uuid = 18,
    Date = 19,
    TimeMicros = 20,
    TimeNanos = 21,
    TimestampSec = 22,
    TimestampMilis = 23,
    TimestampMicros = 24,
    TimestampNanos = 25,
    TimeMicrosTz = 26,
    TimestampMicrosTz = 27,
    Interval = 28,
    Object = 29,
    Array = 30,
    Bignum = 31,
    Bitstring = 32,
    Geometry = 33,
}

impl VariantLogicalType {
    const ALL: [Self; 34] = [
        Self::VariantNull,
        Self::BoolTrue,
        Self::BoolFalse,
        Self::Int8,
        Self::Int16,
        Self::Int32,
        Self::Int64,
        Self::Int128,
        Self::Uint8,
        Self::Uint16,
        Self::Uint32,
        Self::Uint64,
        Self::Uint128,
        Self::Float,
        Self::Double,
        Self::Decimal,
        Self::Varchar,
        Self::Blob,
        Self::Uuid,
        Self::Date,
        Self::TimeMicros,
        Self::TimeNanos,
        Self::TimestampSec,
        Self::TimestampMilis,
        Self::TimestampMicros,
        Self::TimestampNanos,
        Self::TimeMicrosTz,
        Self::TimestampMicrosTz,
        Self::Interval,
        Self::Object,
        Self::Array,
        Self::Bignum,
        Self::Bitstring,
        Self::Geometry,
    ];

    /// The tag stored in `values.type_id`.
    pub fn from_u8(tag: u8) -> Result<Self> {
        Self::ALL
            .get(usize::from(tag))
            .copied()
            .ok_or_else(|| Error::internal(format!("Unrecognized VariantLogicalType: {tag}")))
    }

    /// DuckDB's `EnumUtil::ToString(VariantLogicalType)`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::VariantNull => "VARIANT_NULL",
            Self::BoolTrue => "BOOL_TRUE",
            Self::BoolFalse => "BOOL_FALSE",
            Self::Int8 => "INT8",
            Self::Int16 => "INT16",
            Self::Int32 => "INT32",
            Self::Int64 => "INT64",
            Self::Int128 => "INT128",
            Self::Uint8 => "UINT8",
            Self::Uint16 => "UINT16",
            Self::Uint32 => "UINT32",
            Self::Uint64 => "UINT64",
            Self::Uint128 => "UINT128",
            Self::Float => "FLOAT",
            Self::Double => "DOUBLE",
            Self::Decimal => "DECIMAL",
            Self::Varchar => "VARCHAR",
            Self::Blob => "BLOB",
            Self::Uuid => "UUID",
            Self::Date => "DATE",
            Self::TimeMicros => "TIME_MICROS",
            Self::TimeNanos => "TIME_NANOS",
            Self::TimestampSec => "TIMESTAMP_SEC",
            Self::TimestampMilis => "TIMESTAMP_MILIS",
            Self::TimestampMicros => "TIMESTAMP_MICROS",
            Self::TimestampNanos => "TIMESTAMP_NANOS",
            Self::TimeMicrosTz => "TIME_MICROS_TZ",
            Self::TimestampMicrosTz => "TIMESTAMP_MICROS_TZ",
            Self::Interval => "INTERVAL",
            Self::Object => "OBJECT",
            Self::Array => "ARRAY",
            Self::Bignum => "BIGNUM",
            Self::Bitstring => "BITSTRING",
            Self::Geometry => "GEOMETRY",
        }
    }

    /// Whether the value is an OBJECT or ARRAY (`VariantUtils::IsNestedType`).
    pub const fn is_nested(self) -> bool {
        matches!(self, Self::Object | Self::Array)
    }

    /// The size of a fixed-width payload.
    const fn fixed_size(self) -> Option<usize> {
        Some(match self {
            Self::VariantNull | Self::BoolTrue | Self::BoolFalse => 0,
            Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float | Self::Date => 4,
            Self::Int64
            | Self::Uint64
            | Self::Double
            | Self::TimeMicros
            | Self::TimeNanos
            | Self::TimeMicrosTz
            | Self::TimestampSec
            | Self::TimestampMilis
            | Self::TimestampMicros
            | Self::TimestampNanos
            | Self::TimestampMicrosTz => 8,
            Self::Int128 | Self::Uint128 | Self::Uuid | Self::Interval => 16,
            _ => return None,
        })
    }
}

/// `GetVarintSize`: the number of bytes of the LEB128 encoding of `value`.
pub fn varint_size(mut value: u64) -> usize {
    let mut size = 1;
    while value >= 0x80 {
        value >>= 7;
        size += 1;
    }
    size
}

/// `VarintEncode`: appends the LEB128 encoding of `value`.
pub fn varint_encode(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// `VarintDecode`: reads a LEB128 integer at `*pos`, advancing it.
pub fn varint_decode(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let byte = *data
            .get(*pos)
            .ok_or_else(|| Error::internal("VARIANT data ends inside a varint"))?;
        *pos += 1;
        if shift < 64 {
            result |= u64::from(byte & 0x7f) << shift;
        }
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

/// An entry of `children`: the key (OBJECT children only) and the value of a child.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VariantChild {
    pub keys_index: Option<u32>,
    pub values_index: u32,
}

/// An entry of `values`: the type of a value and the offset of its payload in `data`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariantEntry {
    pub type_id: u8,
    pub byte_offset: u32,
}

/// The `count` and first `children` index of an OBJECT or ARRAY (`VariantNestedData`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VariantNested {
    pub child_count: u32,
    pub children_idx: u32,
}

/// One non-NULL VARIANT value, unpacked from its STRUCT representation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VariantData {
    pub keys: Vec<ByteString>,
    pub children: Vec<VariantChild>,
    pub values: Vec<VariantEntry>,
    pub data: Vec<u8>,
}

/// The types of the STRUCT representation's lists.
struct Layout {
    variant: LogicalType,
    child: LogicalType,
    entry: LogicalType,
}

impl Layout {
    fn new() -> Self {
        let variant = LogicalType::variant();
        let list_child = |index: usize| {
            variant
                .struct_children()
                .and_then(|fields| fields.get(index))
                .and_then(|(_, ty)| ty.child_type())
                .cloned()
                .unwrap_or(LogicalType::INVALID)
        };
        let (child, entry) = (list_child(1), list_child(2));
        Self {
            variant,
            child,
            entry,
        }
    }
}

fn u32_of(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::internal("VARIANT exceeds 4 GiB"))
}

impl VariantData {
    /// Unpacks a VARIANT value; `None` for NULL.
    pub fn from_value(value: &Value) -> Result<Option<Self>> {
        if value.is_null() {
            return Ok(None);
        }
        let malformed = || Error::internal("malformed VARIANT value");
        let [keys, children, values, data] = value.children().ok_or_else(malformed)? else {
            return Err(malformed());
        };
        let list = |list: &Value| list.children().map(<[Value]>::to_vec).unwrap_or_default();
        let field =
            |entry: &Value, index: usize| entry.children().and_then(|fields| fields.get(index)).cloned();
        let as_u32 = |value: Option<Value>| -> Option<u32> { value.and_then(|v| v.as_u32()) };
        let keys = list(keys)
            .iter()
            .map(|key| ByteString::from(key.as_bytes().unwrap_or_default()))
            .collect();
        let children = list(children)
            .iter()
            .map(|child| {
                Ok(VariantChild {
                    keys_index: as_u32(field(child, 0)),
                    values_index: as_u32(field(child, 1)).ok_or_else(malformed)?,
                })
            })
            .collect::<Result<_>>()?;
        let values = list(values)
            .iter()
            .map(|entry| {
                Ok(VariantEntry {
                    type_id: field(entry, 0).and_then(|v| v.as_u8()).ok_or_else(malformed)?,
                    byte_offset: as_u32(field(entry, 1)).ok_or_else(malformed)?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Some(Self {
            keys,
            children,
            values,
            data: data.as_bytes().unwrap_or_default().to_vec(),
        }))
    }

    /// Unpacks row `row` of a vector with the VARIANT layout (a VARIANT or its unshredded
    /// STRUCT) without building a `Value`; `None` for NULL.
    pub fn from_vector(vector: &Vector, row: usize) -> Result<Option<Self>> {
        if vector.is_null(row) {
            return Ok(None);
        }
        let slot = vector.sel().slot(row);
        let [keys, children, values, data] = vector.struct_children().ok_or_else(malformed_vector)? else {
            return Err(malformed_vector());
        };
        let (key_rows, key_vector) = list_elements(keys, slot)?;
        let keys = key_rows
            .map(|index| ByteString::from(key_vector.string_bytes(index)))
            .collect();
        let (child_rows, child_vector) = list_elements(children, slot)?;
        let (keys_index, values_index) = (struct_field(child_vector, 0)?, struct_field(child_vector, 1)?);
        let children = child_rows
            .map(|index| {
                Ok(VariantChild {
                    keys_index: u32_at(keys_index, index),
                    values_index: u32_at(values_index, index).ok_or_else(malformed_vector)?,
                })
            })
            .collect::<Result<_>>()?;
        let (value_rows, value_vector) = list_elements(values, slot)?;
        let (type_id, byte_offset) = (struct_field(value_vector, 0)?, struct_field(value_vector, 1)?);
        let values = value_rows
            .map(|index| {
                if type_id.is_null(index) {
                    return Err(malformed_vector());
                }
                Ok(VariantEntry {
                    type_id: type_id.values::<u8>()[type_id.sel().slot(index)],
                    byte_offset: u32_at(byte_offset, index).ok_or_else(malformed_vector)?,
                })
            })
            .collect::<Result<_>>()?;
        let data = if data.is_null(slot) {
            Vec::new()
        } else {
            data.string_bytes(slot).to_vec()
        };
        Ok(Some(Self {
            keys,
            children,
            values,
            data,
        }))
    }

    /// A flat vector of type `ty` (a VARIANT or its unshredded STRUCT) holding `rows`
    /// (`None` for NULL), built without `Value`s.
    pub fn write_vector(rows: &[Option<Self>], ty: &LogicalType) -> Result<Vector> {
        let count = rows.len();
        let present = || rows.iter().flatten();
        let list_type = |index: usize| -> LogicalType {
            ty.struct_children()
                .and_then(|fields| fields.get(index))
                .map_or(LogicalType::INVALID, |(_, field)| field.clone())
        };
        // A LIST vector whose row `i` has the `lengths[i]` elements that follow, with
        // `child` as its elements.
        let list_vector =
            |list_type: LogicalType, lengths: &mut dyn Iterator<Item = usize>, child: Vector| {
                let mut list = Vector::flat(list_type, count.max(1));
                let mut offset = 0;
                {
                    let entries = list.values_mut::<ListEntry>();
                    for (entry, length) in entries.iter_mut().zip(lengths) {
                        *entry = ListEntry::new(offset, length);
                        offset += length;
                    }
                }
                list.set_list_child(child, offset);
                list
            };
        let struct_vector = |struct_type: LogicalType, fields: Vec<Vector>, rows: usize| {
            let mut vector = Vector::flat(struct_type, rows.max(1));
            if let Some(children) = vector.struct_children_mut() {
                for (child, field) in children.iter_mut().zip(fields) {
                    *child = field;
                }
            }
            vector
        };
        let lengths = |length: fn(&Self) -> usize| rows.iter().map(move |row| row.as_ref().map_or(0, length));

        let total_keys: usize = present().map(|row| row.keys.len()).sum();
        let mut key_vector = Vector::flat(LogicalType::VARCHAR, total_keys.max(1));
        for (index, key) in present().flat_map(|row| &row.keys).enumerate() {
            let string = key_vector.add_string(key.as_bytes());
            key_vector.values_mut::<StringT>()[index] = string;
        }
        let keys = list_vector(list_type(0), &mut lengths(|row| row.keys.len()), key_vector);

        let child_type = list_type(1).child_type().cloned().unwrap_or_default();
        let total_children: usize = present().map(|row| row.children.len()).sum();
        let mut keys_index = Vector::flat(LogicalType::UINTEGER, total_children.max(1));
        let mut values_index = Vector::flat(LogicalType::UINTEGER, total_children.max(1));
        for (index, child) in present().flat_map(|row| &row.children).enumerate() {
            match child.keys_index {
                Some(key) => keys_index.values_mut::<u32>()[index] = key,
                None => keys_index.set_null(index),
            }
            values_index.values_mut::<u32>()[index] = child.values_index;
        }
        let children = list_vector(
            list_type(1),
            &mut lengths(|row| row.children.len()),
            struct_vector(child_type, vec![keys_index, values_index], total_children),
        );

        let entry_type = list_type(2).child_type().cloned().unwrap_or_default();
        let total_values: usize = present().map(|row| row.values.len()).sum();
        let mut type_ids = Vector::flat(LogicalType::UTINYINT, total_values.max(1));
        let mut byte_offsets = Vector::flat(LogicalType::UINTEGER, total_values.max(1));
        for (index, entry) in present().flat_map(|row| &row.values).enumerate() {
            type_ids.values_mut::<u8>()[index] = entry.type_id;
            byte_offsets.values_mut::<u32>()[index] = entry.byte_offset;
        }
        let values = list_vector(
            list_type(2),
            &mut lengths(|row| row.values.len()),
            struct_vector(entry_type, vec![type_ids, byte_offsets], total_values),
        );

        let mut data = Vector::flat(LogicalType::BLOB, count.max(1));
        for (index, row) in rows.iter().enumerate() {
            if let Some(row) = row {
                let string = data.add_string(&row.data);
                data.values_mut::<StringT>()[index] = string;
            }
        }
        let mut result = struct_vector(ty.clone(), vec![keys, children, values, data], count);
        for (index, row) in rows.iter().enumerate() {
            if row.is_none() {
                result.set_null(index);
            }
        }
        Ok(result)
    }

    /// Packs this value into its STRUCT representation.
    pub fn to_value(&self) -> Value {
        self.to_value_with(&Layout::new())
    }

    fn to_value_with(&self, layout: &Layout) -> Value {
        let keys = self
            .keys
            .iter()
            .map(|key| Value::bytes(LogicalType::VARCHAR, key.as_bytes()))
            .collect();
        let children = self
            .children
            .iter()
            .map(|child| {
                let keys_index = child
                    .keys_index
                    .map_or_else(|| Value::null(LogicalType::UINTEGER), Value::uinteger);
                Value::from_children(
                    layout.child.clone(),
                    vec![keys_index, Value::uinteger(child.values_index)],
                )
            })
            .collect();
        let values = self
            .values
            .iter()
            .map(|entry| {
                Value::from_children(
                    layout.entry.clone(),
                    vec![Value::utinyint(entry.type_id), Value::uinteger(entry.byte_offset)],
                )
            })
            .collect();
        Value::from_children(
            layout.variant.clone(),
            vec![
                Value::list(LogicalType::VARCHAR, keys),
                Value::list(layout.child.clone(), children),
                Value::list(layout.entry.clone(), values),
                Value::blob(&self.data),
            ],
        )
    }

    fn entry(&self, value_index: u32) -> Result<VariantEntry> {
        self.values
            .get(value_index as usize)
            .copied()
            .ok_or_else(|| Error::internal("VARIANT value index out of range"))
    }

    /// The type of value `value_index`.
    pub fn type_id(&self, value_index: u32) -> Result<VariantLogicalType> {
        VariantLogicalType::from_u8(self.entry(value_index)?.type_id)
    }

    fn payload(&self, value_index: u32) -> Result<&[u8]> {
        let offset = self.entry(value_index)?.byte_offset as usize;
        self.data
            .get(offset..)
            .ok_or_else(|| Error::internal("VARIANT byte offset out of range"))
    }

    fn fixed<const N: usize>(&self, value_index: u32) -> Result<[u8; N]> {
        self.payload(value_index)?
            .get(..N)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| Error::internal("VARIANT data too short"))
    }

    /// The count and first child of an OBJECT or ARRAY (`DecodeNestedData`).
    pub fn nested(&self, value_index: u32) -> Result<VariantNested> {
        let data = self.payload(value_index)?;
        let mut pos = 0;
        let child_count = u32_of(varint_decode(data, &mut pos)? as usize)?;
        let children_idx = if child_count == 0 {
            0
        } else {
            u32_of(varint_decode(data, &mut pos)? as usize)?
        };
        Ok(VariantNested {
            child_count,
            children_idx,
        })
    }

    /// The bytes of a VARCHAR/BLOB/BIT/BIGNUM/GEOMETRY value (`DecodeStringData`).
    pub fn string(&self, value_index: u32) -> Result<&[u8]> {
        let data = self.payload(value_index)?;
        let mut pos = 0;
        let length = varint_decode(data, &mut pos)? as usize;
        data.get(pos..pos + length)
            .ok_or_else(|| Error::internal("VARIANT string exceeds its data"))
    }

    /// `(width, scale, unscaled value)` of a DECIMAL (`DecodeDecimalData`).
    pub fn decimal(&self, value_index: u32) -> Result<(u32, u32, i128)> {
        let data = self.payload(value_index)?;
        let mut pos = 0;
        let width = u32_of(varint_decode(data, &mut pos)? as usize)?;
        let scale = u32_of(varint_decode(data, &mut pos)? as usize)?;
        let size = decimal_size(width)?;
        let bytes = data
            .get(pos..pos + size)
            .ok_or_else(|| Error::internal("VARIANT decimal exceeds its data"))?;
        let mut buffer = [0u8; 16];
        buffer[..size].copy_from_slice(bytes);
        if bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
            buffer[size..].fill(0xff);
        }
        Ok((width, scale, i128::from_le_bytes(buffer)))
    }

    /// The child entry `children_idx + index` of a nested value.
    pub fn child(&self, nested: VariantNested, index: u32) -> Result<VariantChild> {
        self.children
            .get((nested.children_idx + index) as usize)
            .copied()
            .ok_or_else(|| Error::internal("VARIANT child index out of range"))
    }

    /// The key of an OBJECT child.
    pub fn child_key(&self, child: VariantChild) -> Result<&ByteString> {
        child
            .keys_index
            .and_then(|index| self.keys.get(index as usize))
            .ok_or_else(|| Error::internal("VARIANT key index out of range"))
    }

    /// The keys of an OBJECT (`VariantUtils::GetObjectKeys`).
    pub fn object_keys(&self, nested: VariantNested) -> Result<Vec<ByteString>> {
        (0..nested.child_count)
            .map(|index| self.child_key(self.child(nested, index)?).cloned())
            .collect()
    }

    /// Finds the child value of an OBJECT by key, or of an ARRAY by 0-based index
    /// (`VariantUtils::FindChildValues`).
    pub fn find_child(&self, nested: VariantNested, component: &VariantPathComponent) -> Result<Option<u32>> {
        match component {
            VariantPathComponent::Index(index) => {
                if *index >= nested.child_count {
                    return Ok(None);
                }
                Ok(Some(self.child(nested, *index)?.values_index))
            }
            VariantPathComponent::Key(key) => {
                for index in 0..nested.child_count {
                    let child = self.child(nested, index)?;
                    if self.child_key(child)? == key {
                        return Ok(Some(child.values_index));
                    }
                }
                Ok(None)
            }
        }
    }

    /// The SQL value of value `value_index` (`VariantUtils::ConvertVariantToValue`):
    /// OBJECTs become STRUCTs, ARRAYs LISTs of their common type or else of VARIANT.
    pub fn to_sql_value(&self, value_index: u32) -> Result<Value> {
        use VariantLogicalType as V;
        let type_id = self.type_id(value_index)?;
        Ok(match type_id {
            V::VariantNull => Value::null(LogicalType::SQLNULL),
            V::BoolTrue => Value::boolean(true),
            V::BoolFalse => Value::boolean(false),
            V::Int8 => Value::tinyint(i8::from_le_bytes(self.fixed(value_index)?)),
            V::Int16 => Value::smallint(i16::from_le_bytes(self.fixed(value_index)?)),
            V::Int32 => Value::integer(i32::from_le_bytes(self.fixed(value_index)?)),
            V::Int64 => Value::bigint(i64::from_le_bytes(self.fixed(value_index)?)),
            V::Int128 => Value::hugeint(i128::from_le_bytes(self.fixed(value_index)?)),
            V::Uint8 => Value::utinyint(u8::from_le_bytes(self.fixed(value_index)?)),
            V::Uint16 => Value::usmallint(u16::from_le_bytes(self.fixed(value_index)?)),
            V::Uint32 => Value::uinteger(u32::from_le_bytes(self.fixed(value_index)?)),
            V::Uint64 => Value::ubigint(u64::from_le_bytes(self.fixed(value_index)?)),
            V::Uint128 => Value::uhugeint(u128::from_le_bytes(self.fixed(value_index)?)),
            V::Float => Value::float(f32::from_le_bytes(self.fixed(value_index)?)),
            V::Double => Value::double(f64::from_le_bytes(self.fixed(value_index)?)),
            V::Uuid => Value::uuid(Uuid(i128::from_le_bytes(self.fixed(value_index)?))),
            V::Date => Value::date(Date(i32::from_le_bytes(self.fixed(value_index)?))),
            V::Interval => {
                let bytes: [u8; 16] = self.fixed(value_index)?;
                let (months, rest) = bytes.split_at(4);
                let (days, micros) = rest.split_at(4);
                let read4 = |b: &[u8]| <[u8; 4]>::try_from(b).unwrap_or_default();
                let read8 = |b: &[u8]| <[u8; 8]>::try_from(b).unwrap_or_default();
                Value::interval(Interval {
                    months: i32::from_le_bytes(read4(months)),
                    days: i32::from_le_bytes(read4(days)),
                    micros: i64::from_le_bytes(read8(micros)),
                })
            }
            V::TimeMicros => Value::time(Time(i64::from_le_bytes(self.fixed(value_index)?))),
            V::TimeNanos => Value::time_ns(TimeNs(i64::from_le_bytes(self.fixed(value_index)?))),
            V::TimeMicrosTz => Value::time_tz(TimeTz(u64::from_le_bytes(self.fixed(value_index)?))),
            V::TimestampSec => Value::timestamp_s(i64::from_le_bytes(self.fixed(value_index)?)),
            V::TimestampMilis => Value::timestamp_ms(i64::from_le_bytes(self.fixed(value_index)?)),
            V::TimestampMicros => Value::timestamp(Timestamp(i64::from_le_bytes(self.fixed(value_index)?))),
            V::TimestampNanos => Value::timestamp_ns(i64::from_le_bytes(self.fixed(value_index)?)),
            V::TimestampMicrosTz => {
                Value::timestamp_tz(Timestamp(i64::from_le_bytes(self.fixed(value_index)?)))
            }
            V::Varchar => Value::bytes(LogicalType::VARCHAR, self.string(value_index)?),
            V::Blob => Value::blob(self.string(value_index)?),
            V::Bignum => Value::bytes(LogicalType::BIGNUM, self.string(value_index)?),
            V::Geometry => Value::bytes(LogicalType::GEOMETRY, self.string(value_index)?),
            V::Bitstring => Value::bytes(LogicalType::BIT, self.string(value_index)?),
            V::Decimal => {
                let (width, scale, unscaled) = self.decimal(value_index)?;
                let (width, scale) = (
                    u8::try_from(width).map_err(|_| Error::internal("VARIANT decimal width"))?,
                    u8::try_from(scale).map_err(|_| Error::internal("VARIANT decimal scale"))?,
                );
                Value::decimal(unscaled, width, scale)
            }
            V::Array => {
                let nested = self.nested(value_index)?;
                let items = (0..nested.child_count)
                    .map(|index| self.to_sql_value(self.child(nested, index)?.values_index))
                    .collect::<Result<Vec<_>>>()?;
                let common = items.first().map(|first| first.logical_type().clone());
                match common {
                    Some(ty) if items.iter().all(|item| item.logical_type() == &ty) => Value::list(ty, items),
                    _ => {
                        // `Value::LIST(VARIANT, items)` casts every item to VARIANT.
                        let encoded =
                            items
                                .iter()
                                .map(|item| {
                                    Ok(encode_value(item)?
                                        .unwrap_or_else(|| Value::null(LogicalType::variant())))
                                })
                                .collect::<Result<Vec<_>>>()?;
                        Value::list(LogicalType::variant(), encoded)
                    }
                }
            }
            V::Object => {
                let nested = self.nested(value_index)?;
                let fields = (0..nested.child_count)
                    .map(|index| {
                        let child = self.child(nested, index)?;
                        Ok((
                            self.child_key(child)?.to_owned(),
                            self.to_sql_value(child.values_index)?,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Value::struct_value(fields)
            }
        })
    }

    /// `variant_typeof` of value `value_index`.
    pub fn type_name(&self, value_index: u32) -> Result<ByteString> {
        let type_id = self.type_id(value_index)?;
        Ok(match type_id {
            VariantLogicalType::Decimal => {
                let (width, scale, _) = self.decimal(value_index)?;
                rustdb_common::byte_format!("DECIMAL({width}, {scale})")
            }
            VariantLogicalType::Object => {
                let nested = self.nested(value_index)?;
                rustdb_common::byte_format!("OBJECT({})", ByteString::join(&self.object_keys(nested)?, ", "))
            }
            VariantLogicalType::Array => {
                rustdb_common::byte_format!("ARRAY({})", self.nested(value_index)?.child_count)
            }
            other => other.name().into(),
        })
    }

    /// The same VARIANT rooted at value `value_index` (`VariantUtils::VariantExtract`
    /// remaps value 0 and shares everything else).
    #[must_use]
    pub fn rooted_at(&self, value_index: u32) -> Self {
        let mut result = self.clone();
        if let (Some(&entry), Some(root)) = (self.values.get(value_index as usize), result.values.first_mut())
        {
            *root = entry;
        }
        result
    }

    /// The length in bytes of the payload of value `value_index`, for copying it verbatim.
    fn payload_size(&self, value_index: u32) -> Result<usize> {
        let type_id = self.type_id(value_index)?;
        if let Some(size) = type_id.fixed_size() {
            return Ok(size);
        }
        let data = self.payload(value_index)?;
        let mut pos = 0;
        if type_id == VariantLogicalType::Decimal {
            let width = varint_decode(data, &mut pos)?;
            varint_decode(data, &mut pos)?;
            return Ok(pos + decimal_size(u32_of(width as usize)?)?);
        }
        let length = varint_decode(data, &mut pos)? as usize;
        Ok(pos + length)
    }

    /// The payload bytes of a non-nested value.
    fn raw_payload(&self, value_index: u32) -> Result<&[u8]> {
        let size = self.payload_size(value_index)?;
        self.payload(value_index)?
            .get(..size)
            .ok_or_else(|| Error::internal("VARIANT payload exceeds its data"))
    }

    /// The canonical form of this value (`VariantNormalizer::Normalize`): values in visit
    /// order, OBJECT fields sorted by key, no unused data.
    pub fn normalize(&self) -> Result<Self> {
        let mut state = VariantNormalizer::default();
        state.visit(self, 0)?;
        Ok(state.finish())
    }
}

/// The error for a vector without the VARIANT layout.
fn malformed_vector() -> Error {
    Error::internal("malformed VARIANT vector")
}

/// The element rows of row `slot` of a LIST vector, and its element vector.
fn list_elements(list: &Vector, slot: usize) -> Result<(std::ops::Range<usize>, &Vector)> {
    let (child, _) = list.list_child().ok_or_else(malformed_vector)?;
    if list.is_null(slot) {
        return Ok((0..0, child));
    }
    Ok((list.values::<ListEntry>()[list.sel().slot(slot)].range(), child))
}

/// Field `index` of a STRUCT vector.
fn struct_field(vector: &Vector, index: usize) -> Result<&Vector> {
    vector
        .struct_children()
        .and_then(|fields| fields.get(index))
        .ok_or_else(malformed_vector)
}

/// Row `index` of a UINTEGER vector; `None` for NULL.
fn u32_at(vector: &Vector, index: usize) -> Option<u32> {
    (!vector.is_null(index)).then(|| vector.values::<u32>()[vector.sel().slot(index)])
}

/// The size of a DECIMAL payload of `width` digits.
fn decimal_size(width: u32) -> Result<usize> {
    Ok(match width {
        0..=4 => 2,
        5..=9 => 4,
        10..=18 => 8,
        19..=38 => 16,
        _ => return Err(Error::internal(format!("Can't handle decimal of width: {width}"))),
    })
}

/// The STRUCT with the layout of a VARIANT, which stores the values a shredded VARIANT
/// column does not shred (`VariantShredding::GetUnshreddedType`).
pub fn unshredded_type() -> LogicalType {
    LogicalType::struct_type(
        LogicalType::variant()
            .struct_children()
            .unwrap_or_default()
            .to_vec(),
    )
}

/// One step of a VARIANT path (DuckDB's `VariantPathComponent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VariantPathComponent {
    /// An OBJECT field.
    Key(ByteString),
    /// A 0-based ARRAY index.
    Index(u32),
}

/// Writes values of VARIANTs into one new VARIANT in canonical form (DuckDB's
/// `VariantNormalizer` with its `VariantNormalizerState`): values in visit order, OBJECT
/// fields sorted by key, no unused data.
#[derive(Debug, Default)]
pub struct VariantNormalizer {
    keys: Vec<ByteString>,
    children: Vec<VariantChild>,
    values: Vec<VariantEntry>,
    data: Vec<u8>,
}

impl VariantNormalizer {
    /// The number of values written so far (`values_size`).
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    /// The VARIANT written.
    pub fn finish(self) -> VariantData {
        VariantData {
            keys: self.keys,
            children: self.children,
            values: self.values,
            data: self.data,
        }
    }

    /// Writes value `value_index` of `source` and its children (`VariantVisitor<VariantNormalizer>::Visit`).
    pub fn visit(&mut self, source: &VariantData, value_index: u32) -> Result<()> {
        let type_id = source.type_id(value_index)?;
        self.push_entry(type_id)?;
        match type_id {
            VariantLogicalType::Array => {
                let nested = source.nested(value_index)?;
                varint_encode(nested.child_count.into(), &mut self.data);
                if nested.child_count == 0 {
                    return Ok(());
                }
                let first = self.children.len();
                varint_encode(first as u64, &mut self.data);
                self.children
                    .resize(first + nested.child_count as usize, VariantChild::default());
                for index in 0..nested.child_count {
                    let child = source.child(nested, index)?;
                    self.children[first + index as usize] = VariantChild {
                        keys_index: None,
                        values_index: u32_of(self.values.len())?,
                    };
                    self.visit(source, child.values_index)?;
                }
            }
            VariantLogicalType::Object => {
                let nested = source.nested(value_index)?;
                varint_encode(nested.child_count.into(), &mut self.data);
                if nested.child_count == 0 {
                    return Ok(());
                }
                let sorted = Self::sorted_fields(source, nested, 0..nested.child_count)?;
                self.write_fields(source, sorted, nested.child_count as usize)?;
            }
            _ => self.data.extend_from_slice(source.raw_payload(value_index)?),
        }
        Ok(())
    }

    /// Writes the OBJECT value `value_index` of `source` with only its children at
    /// `positions` (the `VisitObject` of `ShredVariantData`, for the fields of a
    /// partially shredded OBJECT that are not shredded).
    pub fn visit_object_fields(
        &mut self,
        source: &VariantData,
        value_index: u32,
        positions: &[u32],
    ) -> Result<()> {
        self.push_entry(VariantLogicalType::Object)?;
        let nested = source.nested(value_index)?;
        let sorted = Self::sorted_fields(source, nested, positions.iter().copied())?;
        varint_encode(sorted.len() as u64, &mut self.data);
        let count = sorted.len();
        self.write_fields(source, sorted, count)
    }

    fn push_entry(&mut self, type_id: VariantLogicalType) -> Result<()> {
        self.values.push(VariantEntry {
            type_id: type_id as u8,
            byte_offset: u32_of(self.data.len())?,
        });
        Ok(())
    }

    /// The children at `positions` of an OBJECT by key; the first of equal keys wins.
    fn sorted_fields(
        source: &VariantData,
        nested: VariantNested,
        positions: impl Iterator<Item = u32>,
    ) -> Result<BTreeMap<&[u8], VariantChild>> {
        let mut sorted = BTreeMap::new();
        for index in positions {
            let child = source.child(nested, index)?;
            sorted.entry(source.child_key(child)?.as_bytes()).or_insert(child);
        }
        Ok(sorted)
    }

    /// Writes the first child index of an OBJECT, then its `fields` in order, with
    /// `reserved` children and keys.
    fn write_fields(
        &mut self,
        source: &VariantData,
        fields: BTreeMap<&[u8], VariantChild>,
        reserved: usize,
    ) -> Result<()> {
        let first_child = self.children.len();
        let first_key = self.keys.len();
        varint_encode(first_child as u64, &mut self.data);
        self.children
            .resize(first_child + reserved, VariantChild::default());
        self.keys.resize(first_key + reserved, ByteString::new());
        for (slot, (key, child)) in fields.into_iter().enumerate() {
            self.keys[first_key + slot] = ByteString::from(key);
            self.children[first_child + slot] = VariantChild {
                keys_index: Some(u32_of(first_key + slot)?),
                values_index: u32_of(self.values.len())?,
            };
            self.visit(source, child.values_index)?;
        }
        Ok(())
    }
}

/// The VARIANT of one value (a batch of one row); `None` if the value is NULL.
pub fn encode_value(value: &Value) -> Result<Option<Value>> {
    let mut encoded = encode(std::slice::from_ref(value), value.logical_type())?;
    Ok(encoded.pop().flatten())
}

/// Converts a batch of values of type `ty` to VARIANT values (DuckDB's `CastToVARIANT`);
/// NULL values give `None`.
pub fn encode(values: &[Value], ty: &LogicalType) -> Result<Vec<Option<Value>>> {
    let mut rows: Vec<RowBuilder> = (0..values.len()).map(|_| RowBuilder::default()).collect();
    let items: Vec<Item<'_>> = values
        .iter()
        .enumerate()
        .map(|(row, value)| Item {
            row,
            value,
            slot: None,
        })
        .collect();
    let mut encoder = Encoder { rows: &mut rows };
    encoder.convert(&items, ty, true, false)?;
    let layout = Layout::new();
    Ok(rows
        .into_iter()
        .map(|mut row| {
            if row.null {
                return None;
            }
            row.data.resize(row.blob_offset, 0);
            Some(
                VariantData {
                    keys: row.keys,
                    children: row.children,
                    values: row.values,
                    data: row.data,
                }
                .to_value_with(&layout),
            )
        })
        .collect())
}

/// The VARIANT under construction for one row of the batch (the `offsets` chunk of
/// `CastToVARIANT` plus the row's lists).
#[derive(Default)]
struct RowBuilder {
    null: bool,
    keys: Vec<ByteString>,
    children: Vec<VariantChild>,
    values: Vec<VariantEntry>,
    data: Vec<u8>,
    blob_offset: usize,
}

impl RowBuilder {
    fn write(&mut self, offset: usize, bytes: &[u8]) {
        let end = offset + bytes.len();
        if self.data.len() < end {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(bytes);
    }
}

/// A value to convert: its row, and the `children` slot of its parent, if any.
#[derive(Clone, Copy)]
struct Item<'a> {
    row: usize,
    value: &'a Value,
    slot: Option<usize>,
}

struct Encoder<'r> {
    rows: &'r mut [RowBuilder],
}

impl Encoder<'_> {
    /// `WriteVariantMetadata`.
    fn metadata(&mut self, item: Item<'_>, type_id: VariantLogicalType) -> Result<()> {
        let row = &mut self.rows[item.row];
        let index = u32_of(row.values.len())?;
        row.values.push(VariantEntry {
            type_id: type_id as u8,
            byte_offset: u32_of(row.blob_offset)?,
        });
        if let Some(slot) = item.slot {
            row.children[slot].values_index = index;
        }
        Ok(())
    }

    /// `HandleVariantNull`.
    fn null(&mut self, item: Item<'_>, is_root: bool) -> Result<()> {
        if is_root {
            self.rows[item.row].null = true;
            return Ok(());
        }
        self.metadata(item, VariantLogicalType::VariantNull)
    }

    /// `WriteContainerData`.
    fn container(&mut self, row: usize, length: usize) {
        let row = &mut self.rows[row];
        let children_offset = row.children.len();
        let mut bytes = Vec::new();
        varint_encode(length as u64, &mut bytes);
        if length > 0 {
            varint_encode(children_offset as u64, &mut bytes);
        }
        let offset = row.blob_offset;
        row.write(offset, &bytes);
        row.blob_offset += varint_size(length as u64) + varint_size(children_offset as u64);
    }

    /// `ConvertToVariant`.
    fn convert(
        &mut self,
        items: &[Item<'_>],
        ty: &LogicalType,
        is_root: bool,
        ignore_nulls: bool,
    ) -> Result<()> {
        use LogicalTypeId as L;
        match ty.id() {
            L::List | L::Map => {
                let child = ty
                    .child_type()
                    .ok_or_else(|| Error::internal("LIST without a child type"))?;
                self.convert_list(items, child, is_root, ignore_nulls)
            }
            L::Array => {
                let (child, _) = ty
                    .array_child()
                    .ok_or_else(|| Error::internal("ARRAY without a child type"))?;
                self.convert_list(items, child, is_root, ignore_nulls)
            }
            L::Struct => self.convert_struct(items, ty, is_root, ignore_nulls),
            L::Union => self.convert_union(items, ty, is_root, ignore_nulls),
            L::Variant => self.convert_variant(items, is_root, ignore_nulls),
            _ => self.convert_primitive(items, ty, is_root, ignore_nulls),
        }
    }

    /// `ConvertStructToVariant`.
    fn convert_struct(
        &mut self,
        items: &[Item<'_>],
        ty: &LogicalType,
        is_root: bool,
        ignore_nulls: bool,
    ) -> Result<()> {
        let fields = ty.struct_children().unwrap_or_default();
        let mut child_items: Vec<Vec<Item<'_>>> = vec![Vec::new(); fields.len()];
        let mut checked_names = false;
        for &item in items {
            if item.value.is_null() {
                if !ignore_nulls {
                    self.null(item, is_root)?;
                }
                continue;
            }
            self.metadata(item, VariantLogicalType::Object)?;
            self.container(item.row, fields.len());
            if !checked_names {
                if ty.is_unnamed_struct() {
                    return Err(Error::conversion("Can't cast unnamed struct to VARIANT"));
                }
                checked_names = true;
            }
            let row = &mut self.rows[item.row];
            let base = row.children.len();
            for (index, (name, _)) in fields.iter().enumerate() {
                row.children.push(VariantChild {
                    keys_index: Some(u32_of(row.keys.len())?),
                    values_index: 0,
                });
                row.keys.push(name.clone());
                let value = item
                    .value
                    .children()
                    .and_then(|children| children.get(index))
                    .ok_or_else(|| Error::internal("STRUCT value without its fields"))?;
                child_items[index].push(Item {
                    row: item.row,
                    value,
                    slot: Some(base + index),
                });
            }
        }
        for ((_, field_type), child_items) in fields.iter().zip(&child_items) {
            self.convert(child_items, field_type, false, false)?;
        }
        Ok(())
    }

    /// `ConvertListToVariant` / `ConvertArrayToVariant`.
    fn convert_list(
        &mut self,
        items: &[Item<'_>],
        child: &LogicalType,
        is_root: bool,
        ignore_nulls: bool,
    ) -> Result<()> {
        let mut element_items = Vec::new();
        for &item in items {
            if item.value.is_null() {
                if !ignore_nulls {
                    self.null(item, is_root)?;
                }
                continue;
            }
            let elements = item.value.children().unwrap_or_default();
            self.metadata(item, VariantLogicalType::Array)?;
            self.container(item.row, elements.len());
            let row = &mut self.rows[item.row];
            for value in elements {
                element_items.push(Item {
                    row: item.row,
                    value,
                    slot: Some(row.children.len()),
                });
                row.children.push(VariantChild::default());
            }
        }
        self.convert(&element_items, child, false, false)
    }

    /// `ConvertUnionToVariant`: each member converts its non-NULL rows.
    fn convert_union(
        &mut self,
        items: &[Item<'_>],
        ty: &LogicalType,
        is_root: bool,
        ignore_nulls: bool,
    ) -> Result<()> {
        let members = ty.union_members().unwrap_or_default();
        let nulls: Vec<Value> = members
            .iter()
            .map(|(_, member)| Value::null(member.clone()))
            .collect();
        for (index, (_, member)) in members.iter().enumerate() {
            let member_items: Vec<Item<'_>> = items
                .iter()
                .map(|item| Item {
                    value: member_value(item, &nulls, index),
                    ..*item
                })
                .collect();
            self.convert(&member_items, member, is_root, true)?;
        }
        if ignore_nulls {
            return Ok(());
        }
        for item in items {
            let all_null = members
                .iter()
                .enumerate()
                .filter(|(_, (_, member))| member.id() != LogicalTypeId::SqlNull)
                .all(|(index, _)| member_value(item, &nulls, index).is_null());
            if all_null {
                self.null(*item, is_root)?;
            }
        }
        Ok(())
    }

    /// `ConvertVariantToVariant`: copies the values, re-basing the children indices.
    fn convert_variant(&mut self, items: &[Item<'_>], is_root: bool, ignore_nulls: bool) -> Result<()> {
        for &item in items {
            let Some(source) = VariantData::from_value(item.value)? else {
                if !ignore_nulls {
                    self.null(item, is_root)?;
                }
                continue;
            };
            let row = &mut self.rows[item.row];
            let values_offset = row.values.len();
            if let Some(slot) = item.slot {
                row.children[slot].values_index = u32_of(values_offset)?;
            }
            let children_offset = row.children.len();
            for child in &source.children {
                let keys_index = match child.keys_index {
                    Some(key) => {
                        let key = source
                            .keys
                            .get(key as usize)
                            .ok_or_else(|| Error::internal("VARIANT key index out of range"))?;
                        row.keys.push(key.clone());
                        Some(u32_of(row.keys.len() - 1)?)
                    }
                    None => None,
                };
                row.children.push(VariantChild {
                    keys_index,
                    values_index: u32_of(values_offset + child.values_index as usize)?,
                });
            }
            let mut blob_size = 0;
            for index in 0..u32_of(source.values.len())? {
                let type_id = source.type_id(index)?;
                row.values.push(VariantEntry {
                    type_id: type_id as u8,
                    byte_offset: u32_of(row.blob_offset + blob_size)?,
                });
                let mut bytes = Vec::new();
                if type_id.is_nested() {
                    let nested = source.nested(index)?;
                    varint_encode(nested.child_count.into(), &mut bytes);
                    if nested.child_count > 0 {
                        varint_encode(
                            (nested.children_idx as usize + children_offset) as u64,
                            &mut bytes,
                        );
                    }
                } else {
                    bytes.extend_from_slice(source.raw_payload(index)?);
                }
                let offset = row.blob_offset + blob_size;
                row.write(offset, &bytes);
                blob_size += bytes.len();
            }
            row.blob_offset += blob_size;
        }
        Ok(())
    }

    /// `ConvertPrimitiveToVariant`. Like DuckDB, the payload size of the last non-NULL
    /// value also advances the data offset of the NULL rows that follow it.
    fn convert_primitive(
        &mut self,
        items: &[Item<'_>],
        ty: &LogicalType,
        is_root: bool,
        ignore_nulls: bool,
    ) -> Result<()> {
        let mut last_size = 0;
        for &item in items {
            if ty.id() != LogicalTypeId::SqlNull && !item.value.is_null() {
                let (type_id, bytes) = primitive_payload(item.value, ty)?;
                last_size = bytes.len();
                self.metadata(item, type_id)?;
                let row = &mut self.rows[item.row];
                let offset = row.blob_offset;
                row.write(offset, &bytes);
            } else if !ignore_nulls {
                self.null(item, is_root)?;
            }
            self.rows[item.row].blob_offset += last_size;
        }
        Ok(())
    }
}

/// The value of UNION member `index` of `item` (NULL if the union is NULL).
fn member_value<'v>(item: &Item<'v>, nulls: &'v [Value], index: usize) -> &'v Value {
    item.value
        .children()
        .and_then(|fields| fields.get(index + 1))
        .unwrap_or(&nulls[index])
}

/// The type tag and payload bytes of a non-NULL primitive value.
fn primitive_payload(value: &Value, ty: &LogicalType) -> Result<(VariantLogicalType, Vec<u8>)> {
    use LogicalTypeId as L;
    use VariantLogicalType as V;
    let missing = || Error::internal(format!("{ty} value without its payload"));
    let int = |value: &Value| value.as_i128().ok_or_else(missing);
    let string = |type_id: VariantLogicalType, bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 5);
        varint_encode(bytes.len() as u64, &mut out);
        out.extend_from_slice(bytes);
        (type_id, out)
    };
    let i64_of = |value: &Value| value.as_i64().ok_or_else(missing);
    Ok(match ty.id() {
        L::Boolean => {
            let truth = value.as_bool().ok_or_else(missing)?;
            (if truth { V::BoolTrue } else { V::BoolFalse }, Vec::new())
        }
        L::TinyInt => (V::Int8, (int(value)? as i8).to_le_bytes().to_vec()),
        L::SmallInt => (V::Int16, (int(value)? as i16).to_le_bytes().to_vec()),
        L::Integer => (V::Int32, (int(value)? as i32).to_le_bytes().to_vec()),
        L::BigInt => (V::Int64, (int(value)? as i64).to_le_bytes().to_vec()),
        L::HugeInt => (V::Int128, int(value)?.to_le_bytes().to_vec()),
        L::UTinyInt => (V::Uint8, (int(value)? as u8).to_le_bytes().to_vec()),
        L::USmallInt => (V::Uint16, (int(value)? as u16).to_le_bytes().to_vec()),
        L::UInteger => (V::Uint32, (int(value)? as u32).to_le_bytes().to_vec()),
        L::UBigInt => (V::Uint64, (int(value)? as u64).to_le_bytes().to_vec()),
        L::UHugeInt => (
            V::Uint128,
            value.as_u128().ok_or_else(missing)?.to_le_bytes().to_vec(),
        ),
        L::Date => (
            V::Date,
            value.as_i32().ok_or_else(missing)?.to_le_bytes().to_vec(),
        ),
        L::Time => (V::TimeMicros, i64_of(value)?.to_le_bytes().to_vec()),
        L::TimeNs => (V::TimeNanos, i64_of(value)?.to_le_bytes().to_vec()),
        L::TimeTz => (V::TimeMicrosTz, i64_of(value)?.to_le_bytes().to_vec()),
        L::Timestamp => (V::TimestampMicros, i64_of(value)?.to_le_bytes().to_vec()),
        L::TimestampSec => (V::TimestampSec, i64_of(value)?.to_le_bytes().to_vec()),
        L::TimestampNs => (V::TimestampNanos, i64_of(value)?.to_le_bytes().to_vec()),
        L::TimestampMs => (V::TimestampMilis, i64_of(value)?.to_le_bytes().to_vec()),
        L::TimestampTz => (V::TimestampMicrosTz, i64_of(value)?.to_le_bytes().to_vec()),
        L::Uuid => (
            V::Uuid,
            value.as_uuid().ok_or_else(missing)?.0.to_le_bytes().to_vec(),
        ),
        L::Float => (
            V::Float,
            value.as_f32().ok_or_else(missing)?.to_le_bytes().to_vec(),
        ),
        L::Double => (
            V::Double,
            value.as_f64().ok_or_else(missing)?.to_le_bytes().to_vec(),
        ),
        L::Decimal => {
            let (width, scale) = ty.decimal_properties().ok_or_else(missing)?;
            let unscaled = int(value)?;
            let mut out = Vec::with_capacity(18);
            varint_encode(width.into(), &mut out);
            varint_encode(scale.into(), &mut out);
            match ty.physical_type() {
                PhysicalType::Int16 => out.extend_from_slice(&(unscaled as i16).to_le_bytes()),
                PhysicalType::Int32 => out.extend_from_slice(&(unscaled as i32).to_le_bytes()),
                PhysicalType::Int64 => out.extend_from_slice(&(unscaled as i64).to_le_bytes()),
                _ => out.extend_from_slice(&unscaled.to_le_bytes()),
            }
            (V::Decimal, out)
        }
        L::Varchar | L::Char => string(V::Varchar, value.as_bytes().ok_or_else(missing)?),
        L::Geometry => string(V::Geometry, value.as_bytes().ok_or_else(missing)?),
        L::Blob => string(V::Blob, value.as_bytes().ok_or_else(missing)?),
        L::Bignum => string(V::Bignum, value.as_bytes().ok_or_else(missing)?),
        L::Bit => string(V::Bitstring, value.as_bytes().ok_or_else(missing)?),
        L::Interval => {
            let interval = value.as_interval().ok_or_else(missing)?;
            let mut out = Vec::with_capacity(16);
            out.extend_from_slice(&interval.months.to_le_bytes());
            out.extend_from_slice(&interval.days.to_le_bytes());
            out.extend_from_slice(&interval.micros.to_le_bytes());
            (V::Interval, out)
        }
        L::Enum => {
            let index = usize::try_from(int(value)?).map_err(|_| missing())?;
            let text = ty
                .enum_values()
                .and_then(|values| values.get(index))
                .ok_or_else(missing)?;
            string(V::Varchar, text.as_bytes())
        }
        L::Type => return Err(Error::not_implemented("Cannot convert TYPE to VARIANT")),
        other => {
            return Err(Error::not_implemented(format!(
                "Invalid LogicalType ({}) for ConvertToVariant",
                other.name()
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: &Value) -> Value {
        let encoded = encode_value(value).unwrap().unwrap();
        VariantData::from_value(&encoded)
            .unwrap()
            .unwrap()
            .to_sql_value(0)
            .unwrap()
    }

    #[test]
    fn varints_roundtrip() {
        for value in [0u64, 1, 127, 128, 300, u64::from(u32::MAX)] {
            let mut out = Vec::new();
            varint_encode(value, &mut out);
            assert_eq!(out.len(), varint_size(value));
            let mut pos = 0;
            assert_eq!(varint_decode(&out, &mut pos).unwrap(), value);
        }
    }

    #[test]
    fn primitives_roundtrip() {
        for value in [
            Value::integer(42),
            Value::varchar("hello"),
            Value::decimal(15, 3, 1),
            Value::boolean(false),
            Value::double(1.5),
        ] {
            assert_eq!(roundtrip(&value), value);
        }
    }

    #[test]
    fn struct_becomes_object() {
        let value = Value::struct_value(vec![
            ("a".into(), Value::integer(42)),
            (
                "b".into(),
                Value::list(
                    LogicalType::INTEGER,
                    vec![Value::integer(1), Value::null(LogicalType::INTEGER)],
                ),
            ),
        ]);
        let encoded = encode_value(&value).unwrap().unwrap();
        let data = VariantData::from_value(&encoded).unwrap().unwrap();
        assert_eq!(data.type_name(0).unwrap(), "OBJECT(a, b)");
        assert_eq!(data.keys, ["a", "b"]);
        // [1, NULL] has items of different types: a LIST of VARIANT.
        let decoded = data.to_sql_value(0).unwrap();
        assert_eq!(decoded.to_varchar().unwrap(), "{'a': 42, 'b': [1, NULL]}");
    }

    #[test]
    fn normalize_sorts_keys() {
        let value = Value::struct_value(vec![
            ("b".into(), Value::integer(1)),
            ("a".into(), Value::integer(2)),
        ]);
        let encoded = encode_value(&value).unwrap().unwrap();
        let data = VariantData::from_value(&encoded).unwrap().unwrap();
        let normalized = data.normalize().unwrap();
        assert_eq!(normalized.keys, ["a", "b"]);
        assert_eq!(normalized.type_name(0).unwrap(), "OBJECT(a, b)");
    }

    #[test]
    fn vectors_roundtrip() {
        let object = Value::struct_value(vec![
            ("k".into(), Value::varchar("text")),
            (
                "l".into(),
                Value::list(LogicalType::INTEGER, vec![Value::integer(1), Value::integer(2)]),
            ),
        ]);
        let rows: Vec<Option<VariantData>> = [Some(object), None, Some(Value::bigint(7))]
            .iter()
            .map(|value| {
                value
                    .as_ref()
                    .and_then(|value| encode_value(value).unwrap())
                    .map(|encoded| VariantData::from_value(&encoded).unwrap().unwrap())
            })
            .collect();
        for ty in [LogicalType::variant(), unshredded_type()] {
            let vector = VariantData::write_vector(&rows, &ty).unwrap();
            for (row, expected) in rows.iter().enumerate() {
                assert_eq!(&VariantData::from_vector(&vector, row).unwrap(), expected);
                let value = VariantData::from_value(&vector.get_value(row)).unwrap();
                assert_eq!(&value, expected);
            }
        }
    }
}
