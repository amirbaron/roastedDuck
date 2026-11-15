//! The pinned file writer's type whitelist and byte representation.
use roastedduck_common::{Error, Result};
use roastedduck_types::{Date, LogicalType, LogicalTypeId as L, Timestamp, TypeInfo, Value};

pub fn encode_row(values: &[Value], replacement: Option<&[u8]>) -> Result<Vec<u8>> {
    let count = i16::try_from(values.len())
        .map_err(|_| Error::invalid_input("Too many columns for Postgres COPY"))?;
    let mut output = count.to_be_bytes().to_vec();
    for value in values {
        field(value, replacement, &mut output)?;
    }
    Ok(output)
}
fn field(value: &Value, replacement: Option<&[u8]>, output: &mut Vec<u8>) -> Result<()> {
    if value.is_null() {
        output.extend_from_slice(&(-1i32).to_be_bytes());
        return Ok(());
    }
    let bytes = encode(value, replacement)?;
    let length =
        i32::try_from(bytes.len()).map_err(|_| Error::invalid_input("Postgres COPY field is too large"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(&bytes);
    Ok(())
}
fn text(bytes: &[u8], replacement: Option<&[u8]>) -> Result<Vec<u8>> {
    if !bytes.contains(&0) {
        return Ok(bytes.to_vec());
    }
    let replacement = replacement.ok_or_else(|| Error::invalid_input("Attempting to write a VARCHAR value with a NULL-byte. Postgres does not support NULL-bytes in VARCHAR values.\n* SET pg_null_byte_replacement='' to remove NULL bytes or replace them with another character"))?;
    let mut out = Vec::new();
    for byte in bytes {
        if *byte == 0 {
            out.extend_from_slice(replacement);
        } else {
            out.push(*byte);
        }
    }
    Ok(out)
}
fn encode(value: &Value, replacement: Option<&[u8]>) -> Result<Vec<u8>> {
    let ty = value.logical_type();
    Ok(match ty.id() {
        L::Boolean
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Float
        | L::Double
        | L::Blob
        | L::Time
        | L::TimeTz
        | L::Interval
        | L::Uuid => crate::codec::encode_parameter(value)?
            .0
            .ok_or_else(|| Error::internal("Unexpected NULL binary field"))?,
        L::Date => {
            let date = value
                .as_date()
                .ok_or_else(|| Error::internal("Invalid DATE payload"))?;
            if date != Date::INFINITY
                && date != Date::NEG_INFINITY
                && (date.0 <= -2_440_589 || date.0 >= 2_145_042_906)
            {
                return Err(Error::invalid_input(format!(
                    "DATE \"{}\" is out of range for Postgres' DATE field",
                    value.to_varchar()?
                )));
            }
            crate::codec::encode_parameter(value)?
                .0
                .ok_or_else(|| Error::internal("Unexpected NULL DATE"))?
        }
        L::Timestamp | L::TimestampTz => {
            let raw = value
                .as_i64()
                .ok_or_else(|| Error::internal("Invalid TIMESTAMP payload"))?;
            let wire = if raw == Timestamp::INFINITY.0 {
                i64::MAX
            } else if raw == Timestamp::NEG_INFINITY.0 {
                i64::MIN
            } else {
                raw.wrapping_sub(946_684_800_000_000)
            };
            wire.to_be_bytes().to_vec()
        }
        L::Decimal => decimal(
            value
                .as_i128()
                .ok_or_else(|| Error::internal("Invalid DECIMAL payload"))?,
            ty.decimal_properties()
                .ok_or_else(|| Error::internal("Invalid DECIMAL type"))?
                .1,
        ),
        L::Varchar => text(
            value
                .as_bytes()
                .ok_or_else(|| Error::internal("Invalid VARCHAR payload"))?,
            replacement,
        )?,
        L::Enum => text(value.to_varchar_bytes()?.as_bytes(), replacement)?,
        L::Geometry => geometry(value)?,
        L::List => array(value, replacement)?,
        L::Struct => {
            let children = value
                .children()
                .ok_or_else(|| Error::internal("Invalid STRUCT payload"))?;
            let mut out = (children.len() as u32).to_be_bytes().to_vec();
            for child in children {
                out.extend_from_slice(&oid(child.logical_type())?.to_be_bytes());
                field(child, replacement, &mut out)?;
            }
            out
        }
        _ => {
            return Err(Error::not_implemented(format!(
                "Type \"{ty}\" is not supported for Postgres binary copy"
            )));
        }
    })
}
fn oid(ty: &LogicalType) -> Result<u32> {
    Ok(match ty.id() {
        L::Boolean => 16,
        L::SmallInt => 21,
        L::Integer => 23,
        L::BigInt => 20,
        L::Float => 700,
        L::Double => 701,
        L::Varchar => 1043,
        L::Blob => 17,
        L::Date => 1082,
        L::Time => 1083,
        L::Timestamp => 1114,
        L::TimestampTz => 1184,
        L::Interval => 1186,
        L::TimeTz => 1266,
        L::Bit => 1560,
        L::Uuid => 2950,
        L::List => {
            return oid(ty
                .child_type()
                .ok_or_else(|| Error::internal("LIST without a child type"))?);
        }
        _ => {
            return Err(Error::not_implemented(format!(
                "Unsupported type for Postgres array copy: {ty}"
            )));
        }
    })
}
fn decimal(value: i128, scale: u8) -> Vec<u8> {
    let mut digits = value.unsigned_abs().to_string();
    let scale = usize::from(scale);
    if digits.len() <= scale {
        digits.insert_str(0, &"0".repeat(scale + 1 - digits.len()));
    }
    let split = digits.len() - scale;
    let integer = digits[..split].trim_start_matches('0');
    let mut integral = "0".repeat((4 - integer.len() % 4) % 4);
    integral.push_str(integer);
    let mut fraction = digits[split..].to_owned();
    fraction.push_str(&"0".repeat((4 - scale % 4) % 4));
    let count = (integral.len() + fraction.len()) / 4;
    let weight = (integral.len() / 4) as i16 - 1;
    let mut out = (count as u16).to_be_bytes().to_vec();
    out.extend_from_slice(&weight.to_be_bytes());
    out.extend_from_slice(&(if value < 0 { 0x4000u16 } else { 0 }).to_be_bytes());
    out.extend_from_slice(&(scale as u16).to_be_bytes());
    for part in [integral, fraction] {
        for group in part.as_bytes().as_chunks::<4>().0 {
            let n = group.iter().fold(0u16, |n, b| n * 10 + u16::from(b - b'0'));
            out.extend_from_slice(&n.to_be_bytes());
        }
    }
    out
}
fn array(value: &Value, replacement: Option<&[u8]>) -> Result<Vec<u8>> {
    let ty = value.logical_type();
    let value_oid = oid(ty
        .child_type()
        .ok_or_else(|| Error::internal("LIST without child type"))?)?;
    let children = value
        .children()
        .ok_or_else(|| Error::internal("Invalid LIST payload"))?;
    if children.is_empty() {
        let mut out = vec![0; 8];
        out.extend_from_slice(&value_oid.to_be_bytes());
        return Ok(out);
    }
    let mut shape = Vec::new();
    let mut current = Some(value);
    let mut current_type = ty;
    while current_type.id() == L::List {
        let children = current.and_then(Value::children).unwrap_or_default();
        shape.push(children.len());
        current = children.first();
        current_type = current_type
            .child_type()
            .ok_or_else(|| Error::internal("LIST without a child type"))?;
    }
    let mut out = (shape.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&value_oid.to_be_bytes());
    for length in &shape {
        out.extend_from_slice(&(*length as u32).to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
    }
    write_array(value, &shape, 0, replacement, &mut out)?;
    Ok(out)
}
fn write_array(
    value: &Value,
    shape: &[usize],
    depth: usize,
    replacement: Option<&[u8]>,
    out: &mut Vec<u8>,
) -> Result<()> {
    let children = value.children().unwrap_or_default();
    if children.len() != shape[depth] {
        return Err(Error::invalid_input(format!(
            "Postgres multidimensional arrays must all have matching dimensions - found a length mismatch (found {} entries, expected {})",
            children.len(),
            shape[depth]
        )));
    }
    for child in children {
        if depth + 1 < shape.len() {
            write_array(child, shape, depth + 1, replacement, out)?;
        } else {
            field(child, replacement, out)?;
        }
    }
    Ok(())
}
fn geometry(value: &Value) -> Result<Vec<u8>> {
    let bytes = value
        .as_bytes()
        .ok_or_else(|| Error::internal("Invalid GEOMETRY payload"))?;
    let Some(TypeInfo::Geo(info)) = value.logical_type().info() else {
        return Ok(bytes.to_vec());
    };
    let crs = roastedduck_types::geometry::Crs::parse(&info.crs.definition)?;
    let Some((_, suffix)) = crs.identifier().split_once(':') else {
        return Ok(bytes.to_vec());
    };
    let suffix = suffix.trim_start_matches(|c| matches!(c, ' ' | '\t'..='\r'));
    let length = suffix
        .as_bytes()
        .iter()
        .enumerate()
        .take_while(|(i, b)| b.is_ascii_digit() || *i == 0 && matches!(**b, b'+' | b'-'))
        .count();
    let Ok(srid) = suffix[..length].parse::<i32>() else {
        return Ok(bytes.to_vec());
    };
    if bytes.len() < 5 {
        return Err(Error::invalid_input("Invalid GEOMETRY payload"));
    }
    let mut out = vec![bytes[0]];
    let kind = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]) | 0x2000_0000;
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&srid.to_le_bytes());
    out.extend_from_slice(&bytes[5..]);
    Ok(out)
}
