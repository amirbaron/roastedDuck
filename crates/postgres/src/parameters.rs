//! SQL parameter behavior from the pinned `postgres_parameters.cpp`.
use crate::QueryParameter;
use roastedduck_common::{Error, Result};
use roastedduck_types::{LogicalTypeId as L, Value};

pub fn encode(value: &Value) -> Result<QueryParameter> {
    if value.is_null() {
        return Ok(QueryParameter {
            bytes: None,
            text: false,
        });
    }
    let bytes = match value.logical_type().id() {
        L::Varchar => {
            let bytes = value
                .as_bytes()
                .ok_or_else(|| Error::internal("Invalid string parameter"))?;
            // libpq's text parameters are C strings; lengths are ignored.
            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            return Ok(QueryParameter {
                bytes: Some(bytes[..end].to_vec()),
                text: true,
            });
        }
        L::TinyInt | L::UTinyInt | L::SmallInt => (value
            .as_i128()
            .ok_or_else(|| Error::internal("Invalid integer parameter"))?
            as i16)
            .to_be_bytes()
            .to_vec(),
        L::USmallInt | L::Integer => (value
            .as_i128()
            .ok_or_else(|| Error::internal("Invalid integer parameter"))?
            as i32)
            .to_be_bytes()
            .to_vec(),
        L::UInteger | L::BigInt => (value
            .as_i128()
            .ok_or_else(|| Error::internal("Invalid integer parameter"))?
            as i64)
            .to_be_bytes()
            .to_vec(),
        L::Float => value
            .as_f32()
            .ok_or_else(|| Error::internal("Invalid float parameter"))?
            .to_be_bytes()
            .to_vec(),
        L::Double => value
            .as_f64()
            .ok_or_else(|| Error::internal("Invalid double parameter"))?
            .to_be_bytes()
            .to_vec(),
        _ => {
            return Err(Error::binder(format!(
                "Unsupported parameter type: {}",
                value.logical_type()
            )));
        }
    };
    Ok(QueryParameter {
        bytes: Some(bytes),
        text: false,
    })
}
