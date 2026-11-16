//! Conversion of PostgreSQL text-format fields, following `PostgresTextReader`.
use roastedduck_common::{Error, Result};
use roastedduck_function::cast::ValueCast;
use roastedduck_types::{LogicalType, LogicalTypeId as L, Value};

fn nested(bytes: &[u8], start: u8, end: u8, list: bool) -> Result<Vec<Option<Vec<u8>>>> {
    if bytes.len() < 2 || bytes[0] != start || bytes[bytes.len() - 1] != end {
        return Err(Error::invalid_input(format!(
            "Invalid Postgres list - expected {}...{} - got {}",
            char::from(start),
            char::from(end),
            String::from_utf8_lossy(bytes)
        )));
    }
    let mut values = Vec::new();
    let mut current = Vec::new();
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut was_quoted = false;
    let mut i = 1;
    let add = |values: &mut Vec<Option<Vec<u8>>>, current: &mut Vec<u8>, was_quoted: &mut bool| {
        if !current.is_empty() || *was_quoted {
            let value = std::mem::take(current);
            values.push(if !*was_quoted && value == b"NULL" {
                None
            } else {
                Some(value)
            });
            if list {
                *was_quoted = false;
            }
        }
    };
    while i + 1 < bytes.len() {
        let byte = bytes[i];
        if quoted {
            match byte {
                b'"' => quoted = false,
                b'\\' => {
                    i += 1;
                    if let Some(byte) = bytes.get(i) {
                        current.push(*byte);
                    }
                }
                _ => current.push(byte),
            }
        } else {
            match byte {
                b'{' | b'(' => {
                    stack.push(if byte == b'{' { b'}' } else { b')' });
                    current.push(byte);
                }
                b'}' | b')' => {
                    if stack.pop() != Some(byte) {
                        return Err(Error::invalid_input(
                            "Failed to convert list - mismatch in brackets",
                        ));
                    }
                    current.push(byte);
                }
                b'"' => {
                    quoted = true;
                    was_quoted = true;
                }
                b',' if stack.is_empty() => add(&mut values, &mut current, &mut was_quoted),
                _ => current.push(byte),
            }
        }
        i += 1;
    }
    add(&mut values, &mut current, &mut was_quoted);
    Ok(values)
}

fn hex(bytes: &[u8]) -> Result<Vec<u8>> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::invalid_input("Blob size must be modulo 2 (\\xAA)"));
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(Error::invalid_input("Invalid hex value in Postgres field")),
    };
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}

pub(super) fn decode(ty: &LogicalType, bytes: Option<&[u8]>) -> Result<Value> {
    let Some(bytes) = bytes else {
        return Ok(Value::null(ty.clone()));
    };
    match ty.id() {
        L::List => {
            let child = ty
                .child_type()
                .ok_or_else(|| Error::internal("Postgres list without child"))?;
            let values = nested(bytes, b'{', b'}', true)?
                .into_iter()
                .map(|v| decode(child, v.as_deref()))
                .collect::<Result<_>>()?;
            Ok(Value::from_children(ty.clone(), values))
        }
        L::Struct => {
            let fields = ty.struct_children().unwrap_or_default();
            let values = nested(bytes, b'(', b')', false)?;
            if values.len() != fields.len() {
                return Err(Error::invalid_input(format!(
                    "Missing columns in data for parsing struct - expected {} but got {}",
                    fields.len(),
                    values.len()
                )));
            }
            Ok(Value::from_children(
                ty.clone(),
                fields
                    .iter()
                    .zip(values)
                    .map(|((_, ty), v)| decode(ty, v.as_deref()))
                    .collect::<Result<_>>()?,
            ))
        }
        L::Blob => {
            let data = bytes
                .strip_prefix(b"\\x")
                .ok_or_else(|| Error::invalid_input("Incorrect blob format - expected \\x... for blob"))?;
            Ok(Value::blob(&hex(data)?))
        }
        L::Geometry => {
            let data = roastedduck_types::geometry::from_binary(&hex(bytes)?, true)?
                .ok_or_else(|| Error::invalid_input("Failed to parse Postgres geometry data"))?;
            Ok(Value::bytes(ty.clone(), &data))
        }
        _ => Value::bytes(LogicalType::VARCHAR, bytes).default_cast_as(ty),
    }
}
pub(super) fn ctid(bytes: Option<&[u8]>) -> Result<Value> {
    let Some(bytes) = bytes else {
        return Ok(Value::null(LogicalType::BIGINT));
    };
    let fields = nested(bytes, b'(', b')', false)?;
    if fields.len() != 2 {
        return Err(Error::invalid_input(
            "CTID mismatch - expected (page_index, row_in_page)",
        ));
    }
    let number = |bytes: &Option<Vec<u8>>| -> Result<u64> {
        std::str::from_utf8(bytes.as_deref().unwrap_or_default())
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::invalid_input("Invalid Postgres CTID"))
    };
    let value = (number(&fields[0])? << 16) + number(&fields[1])?;
    Ok(Value::bigint(
        i64::try_from(value).map_err(|_| Error::invalid_input("Postgres CTID overflow"))?,
    ))
}
