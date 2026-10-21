//! Variant values as a tree (DuckDB's `VariantValue`, `common/types/variant_value.cpp`):
//! what unshredding and the Parquet reader assemble before converting a batch of them
//! into the VARIANT layout (`VariantValue::ToVARIANT`).

use std::collections::BTreeMap;

use rustdb_common::{Error, Result};

use crate::variant::{
    VariantChild, VariantData, VariantEntry, VariantLogicalType, varint_encode, varint_size,
};
use crate::{LogicalTypeId, PhysicalType, Value};

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

    pub fn is_null(&self) -> bool {
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
