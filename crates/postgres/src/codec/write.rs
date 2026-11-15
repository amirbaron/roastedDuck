use super::{DATE_EPOCH, TIMESTAMP_EPOCH, invalid, numeric};
use crate::BinaryParameter;
use roastedduck_common::{Error, ErrorKind, Result};
use roastedduck_types::{Date, LogicalType, LogicalTypeId as L, TimeTz, Timestamp, Value};

/// Encode using DuckDB's input type, not the server's inferred parameter type.
/// This preserves PostgreSQL's binary-size errors for mismatched bindings.
pub fn encode_parameter(value: &Value) -> Result<BinaryParameter> {
    if value.is_null() {
        return Ok(BinaryParameter(None));
    }
    Ok(BinaryParameter(Some(encode(value)?)))
}

fn required<T>(value: Option<T>) -> Result<T> {
    value.ok_or_else(|| Error::internal("PostgreSQL parameter has invalid payload"))
}

fn encode(value: &Value) -> Result<Vec<u8>> {
    let ty = value.logical_type();
    Ok(match ty.id() {
        L::Boolean => vec![u8::from(required(value.as_bool())?)],
        L::TinyInt | L::SmallInt => (required(value.as_i128())? as i16).to_be_bytes().to_vec(),
        L::Integer => required(value.as_i32())?.to_be_bytes().to_vec(),
        L::BigInt | L::Time => required(value.as_i64())?.to_be_bytes().to_vec(),
        L::UTinyInt | L::USmallInt | L::UInteger => {
            (required(value.as_i128())? as i64).to_be_bytes().to_vec()
        }
        L::UBigInt => numeric::encode(i128::from(required(value.as_u64())?), 0),
        L::HugeInt => (required(value.as_i128())? as f64).to_be_bytes().to_vec(),
        L::Float => required(value.as_f32())?.to_be_bytes().to_vec(),
        L::Double => required(value.as_f64())?.to_be_bytes().to_vec(),
        L::Decimal => numeric::encode(required(value.as_i128())?, required(ty.decimal_properties())?.1),
        L::Varchar | L::Blob => required(value.as_bytes())?.to_vec(),
        L::Date => {
            let date = required(value.as_date())?;
            let wire = if date == Date::INFINITY {
                i32::MAX
            } else if date == Date::NEG_INFINITY {
                i32::MIN
            } else {
                date.0
                    .checked_sub(DATE_EPOCH)
                    .ok_or_else(|| invalid("PostgreSQL date overflow"))?
            };
            wire.to_be_bytes().to_vec()
        }
        L::TimeTz => {
            let time = TimeTz(required(value.as_i64())? as u64);
            let mut bytes = time.time().0.to_be_bytes().to_vec();
            bytes.extend_from_slice(&(-time.offset()).to_be_bytes());
            bytes
        }
        L::Timestamp | L::TimestampTz | L::TimestampSec | L::TimestampMs | L::TimestampNs => {
            let raw = required(value.as_i64())?;
            let wire = if raw == Timestamp::INFINITY.0 {
                i64::MAX
            } else if raw == Timestamp::NEG_INFINITY.0 {
                i64::MIN
            } else {
                let micros = match ty.id() {
                    L::TimestampSec => raw.checked_mul(1_000_000),
                    L::TimestampMs => raw.checked_mul(1_000),
                    L::TimestampNs => Some(raw / 1_000),
                    _ => Some(raw),
                }
                .ok_or_else(|| invalid("PostgreSQL timestamp overflow"))?;
                micros
                    .checked_sub(TIMESTAMP_EPOCH)
                    .ok_or_else(|| invalid("PostgreSQL timestamp overflow"))?
            };
            wire.to_be_bytes().to_vec()
        }
        L::Interval => {
            let interval = required(value.as_interval())?;
            let mut bytes = interval.micros.to_be_bytes().to_vec();
            bytes.extend_from_slice(&interval.days.to_be_bytes());
            bytes.extend_from_slice(&interval.months.to_be_bytes());
            bytes
        }
        L::Uuid => required(value.as_uuid())?.to_u128().to_be_bytes().to_vec(),
        L::List => encode_array(value)?,
        L::Struct => {
            let fields = required(value.children())?;
            let mut bytes = (fields.len() as i32).to_be_bytes().to_vec();
            for field in fields {
                bytes.extend_from_slice(&oid(field.logical_type())?.to_be_bytes());
                write_field(field, &mut bytes)?;
            }
            bytes
        }
        _ => value.to_varchar()?.into_bytes(),
    })
}

fn write_field(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    if value.is_null() {
        out.extend_from_slice(&(-1i32).to_be_bytes());
    } else {
        let bytes = encode(value)?;
        let length =
            i32::try_from(bytes.len()).map_err(|_| invalid("PostgreSQL field exceeds wire length limit"))?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&bytes);
    }
    Ok(())
}

fn oid(ty: &LogicalType) -> Result<u32> {
    Ok(match ty.id() {
        L::Boolean => 16,
        L::Blob => 17,
        L::BigInt | L::UTinyInt | L::USmallInt | L::UInteger => 20,
        L::TinyInt | L::SmallInt => 21,
        L::Integer => 23,
        L::Varchar => 1043,
        L::Float => 700,
        L::Double | L::HugeInt => 701,
        L::Decimal | L::UBigInt => 1700,
        L::Date => 1082,
        L::Time => 1083,
        L::Timestamp | L::TimestampSec | L::TimestampMs | L::TimestampNs => 1114,
        L::TimestampTz => 1184,
        L::Interval => 1186,
        L::TimeTz => 1266,
        L::Uuid => 2950,
        _ => {
            return Err(Error::new(
                ErrorKind::NotImplemented,
                format!("PostgreSQL type OID is required for {ty}"),
            ));
        }
    })
}

fn encode_array(value: &Value) -> Result<Vec<u8>> {
    let mut element_type = value.logical_type();
    let mut depth = 0;
    while element_type.id() == L::List {
        element_type = required(element_type.child_type())?;
        depth += 1;
    }
    if depth > 6 {
        return Err(invalid("PostgreSQL supports at most six array dimensions"));
    }
    let mut dimensions = vec![None; depth];
    let mut leaves = Vec::new();
    walk(value, 0, &mut dimensions, &mut leaves)?;
    let empty = leaves.is_empty();
    let mut bytes = (if empty { 0 } else { depth as i32 }).to_be_bytes().to_vec();
    bytes.extend_from_slice(&i32::from(leaves.iter().any(|v| v.is_null())).to_be_bytes());
    bytes.extend_from_slice(&oid(element_type)?.to_be_bytes());
    if !empty {
        for dimension in dimensions {
            let length = i32::try_from(dimension.unwrap_or(0))
                .map_err(|_| invalid("PostgreSQL array dimension too large"))?;
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes.extend_from_slice(&1i32.to_be_bytes());
        }
        for leaf in leaves {
            write_field(leaf, &mut bytes)?;
        }
    }
    Ok(bytes)
}

fn walk<'a>(
    v: &'a Value,
    level: usize,
    shape: &mut [Option<usize>],
    leaves: &mut Vec<&'a Value>,
) -> Result<()> {
    if level == shape.len() {
        leaves.push(v);
        return Ok(());
    }
    if v.is_null() {
        return Err(invalid(
            "Postgres multidimensional arrays cannot contain NULL sub-arrays",
        ));
    }
    let values = required(v.children())?;
    if shape[level].is_some_and(|n| n != values.len()) {
        return Err(invalid(
            "Postgres multidimensional arrays must have matching dimensions",
        ));
    }
    shape[level] = Some(values.len());
    for value in values {
        walk(value, level + 1, shape, leaves)?;
    }
    Ok(())
}
