//! PostgreSQL COPY output, matching the pinned extension's text and binary forms.
use crate::codec::encode_parameter;
use roastedduck_common::{Error, Result};
use roastedduck_types::{LogicalType, LogicalTypeId as L, Value};

pub const BINARY_HEADER: &[u8] = b"PGCOPY\n\xff\r\n\0\0\0\0\0\0\0\0\0";
pub const BINARY_FOOTER: &[u8] = &[0xff, 0xff];
pub const TEXT_FOOTER: &[u8] = b"\\.\n";

pub fn postgres_type(ty: &LogicalType) -> LogicalType {
    match ty.id() {
        L::Boolean
        | L::SmallInt
        | L::Integer
        | L::BigInt
        | L::Float
        | L::Double
        | L::Enum
        | L::Blob
        | L::Date
        | L::Decimal
        | L::Interval
        | L::Time
        | L::TimeTz
        | L::Timestamp
        | L::TimestampTz
        | L::Uuid
        | L::Varchar
        | L::Geometry => ty.clone(),
        L::List => ty.child_type().map_or(LogicalType::VARCHAR, |child| {
            LogicalType::list(postgres_type(child))
        }),
        L::Struct => {
            let mapped = LogicalType::struct_type(
                ty.struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(name, ty)| (name.clone(), postgres_type(ty)))
                    .collect(),
            );
            ty.alias().map_or_else(
                || mapped.clone(),
                |alias| mapped.clone().with_alias(alias.clone()),
            )
        }
        L::TimestampSec | L::TimestampMs | L::TimestampNs => LogicalType::TIMESTAMP,
        L::TinyInt => LogicalType::SMALLINT,
        L::UTinyInt | L::USmallInt | L::UInteger => LogicalType::BIGINT,
        L::UBigInt => LogicalType::decimal(20, 0),
        L::HugeInt => LogicalType::DOUBLE,
        _ => LogicalType::VARCHAR,
    }
}

pub fn type_sql(ty: &LogicalType) -> Result<String> {
    if let Some(alias) = ty.alias() {
        return alias
            .to_str()
            .map(str::to_owned)
            .map_err(|_| Error::invalid_input("Invalid UTF-8 in Postgres type name"));
    }
    Ok(match ty.id() {
        L::Float => "REAL".into(),
        L::Double => "FLOAT".into(),
        L::Blob => "BYTEA".into(),
        L::List => format!(
            "{}[]",
            type_sql(
                ty.child_type()
                    .ok_or_else(|| Error::internal("Postgres list without child type"))?
            )?
        ),
        L::Enum => {
            return Err(Error::not_implemented(
                "Enums in Postgres must be named - unnamed enums are not supported. Use CREATE TYPE to create a named enum.",
            ));
        }
        L::Struct => {
            return Err(Error::not_implemented(
                "Composite types in Postgres must be named - unnamed composite types are not supported. Use CREATE TYPE to create a named composite type.",
            ));
        }
        L::Map => return Err(Error::not_implemented("MAP type not supported in Postgres")),
        L::Union => return Err(Error::not_implemented("UNION type not supported in Postgres")),
        _ => ty.to_string(),
    })
}

fn replace_nulls(bytes: &[u8], replacement: Option<&str>) -> Result<Vec<u8>> {
    if !bytes.contains(&0) {
        return Ok(bytes.to_vec());
    }
    let replacement = replacement.ok_or_else(|| Error::invalid_input("Attempting to write a VARCHAR value with a NULL-byte. Postgres does not support NULL-bytes in VARCHAR values.\n* SET pg_null_byte_replacement='' to remove NULL bytes or replace them with another character"))?;
    if replacement.as_bytes().contains(&0) {
        return Err(Error::invalid_input(
            "NULL byte replacement string cannot contain NULL values",
        ));
    }
    let mut result = Vec::new();
    for byte in bytes {
        if *byte == 0 {
            result.extend_from_slice(replacement.as_bytes());
        } else {
            result.push(*byte);
        }
    }
    Ok(result)
}
fn sanitize(value: &Value, replacement: Option<&str>) -> Result<Value> {
    if value.is_null() {
        return Ok(value.clone());
    }
    if value.logical_type().id() == L::Varchar {
        return Ok(Value::bytes(
            value.logical_type().clone(),
            &replace_nulls(value.as_bytes().unwrap_or_default(), replacement)?,
        ));
    }
    if let Some(children) = value.children() {
        return Ok(Value::from_children(
            value.logical_type().clone(),
            children
                .iter()
                .map(|v| sanitize(v, replacement))
                .collect::<Result<_>>()?,
        ));
    }
    Ok(value.clone())
}

pub fn binary_row(values: &[Value], replacement: Option<&str>) -> Result<Vec<u8>> {
    let count = i16::try_from(values.len())
        .map_err(|_| Error::invalid_input("Too many columns for Postgres COPY"))?;
    let mut result = count.to_be_bytes().to_vec();
    for value in values {
        match encode_parameter(&sanitize(value, replacement)?)?.0 {
            None => result.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(bytes) => {
                let length = i32::try_from(bytes.len())
                    .map_err(|_| Error::invalid_input("Postgres COPY field is too large"))?;
                result.extend_from_slice(&length.to_be_bytes());
                result.extend_from_slice(&bytes);
            }
        }
    }
    Ok(result)
}

fn quote_nested(bytes: &[u8], out: &mut Vec<u8>) {
    let quoted = bytes.is_empty()
        || bytes.eq_ignore_ascii_case(b"NULL")
        || bytes.first().is_some_and(u8::is_ascii_whitespace)
        || bytes.last().is_some_and(u8::is_ascii_whitespace)
        || bytes.iter().any(|c| b"\"\\{}(),".contains(c));
    if quoted {
        out.push(b'"');
    }
    for byte in bytes {
        if quoted && matches!(*byte, b'"' | b'\\') {
            out.push(b'\\');
        }
        out.push(*byte);
    }
    if quoted {
        out.push(b'"');
    }
}

fn text_value(value: &Value) -> Result<Vec<u8>> {
    match value.logical_type().id() {
        L::List | L::Struct => {
            let list = value.logical_type().id() == L::List;
            let mut result = vec![if list { b'{' } else { b'(' }];
            for (index, child) in value.children().unwrap_or_default().iter().enumerate() {
                if index > 0 {
                    result.push(b',');
                }
                if child.is_null() {
                    if list {
                        result.extend_from_slice(b"NULL");
                    }
                } else {
                    let bytes = text_value(child)?;
                    if list && child.logical_type().id() == L::List {
                        result.extend_from_slice(&bytes);
                    } else {
                        quote_nested(&bytes, &mut result);
                    }
                }
            }
            result.push(if list { b'}' } else { b')' });
            Ok(result)
        }
        L::Blob => {
            const HEX: &[u8] = b"0123456789ABCDEF";
            let mut result = b"\\x".to_vec();
            for byte in value.as_bytes().unwrap_or_default() {
                result.push(HEX[usize::from(byte >> 4)]);
                result.push(HEX[usize::from(byte & 15)]);
            }
            Ok(result)
        }
        L::Varchar => Ok(value.as_bytes().unwrap_or_default().to_vec()),
        _ => Ok(value.to_varchar()?.into_bytes()),
    }
}
pub fn text_row(values: &[Value], replacement: Option<&str>) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            result.push(b'\t');
        }
        if value.is_null() {
            result.push(8);
            continue;
        }
        let bytes = replace_nulls(&text_value(value)?, replacement)?;
        for byte in bytes {
            let escaped = match byte {
                b'\n' => Some(b'n'),
                b'\r' => Some(b'r'),
                8 => Some(b'b'),
                12 => Some(b'f'),
                b'\t' => Some(b't'),
                11 => Some(b'v'),
                b'\\' => Some(b'\\'),
                b'"' => Some(b'"'),
                _ => None,
            };
            if let Some(escaped) = escaped {
                result.push(b'\\');
                result.push(escaped);
            } else {
                result.push(byte);
            }
        }
    }
    result.push(b'\n');
    Ok(result)
}
