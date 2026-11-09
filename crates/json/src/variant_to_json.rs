//! VARIANT to JSON (DuckDB's `src/function/cast/variant/to_json.cpp`, the `JSONConverter`
//! visitor): objects and arrays nest, numbers stay numbers (wide integers, decimals and
//! bignums as raw numbers), and every other value is written as its text.

use roastedduck_common::Result;
use roastedduck_function::cast::{BoundCastInfo, CastParameters};
use roastedduck_types::variant::{VariantData, VariantLogicalType as V};
use roastedduck_types::{Value, Vector};

use crate::json_executors::{ColumnBuilder, StrBuilder};
use crate::yyjson::{write_f64, write_i64, write_raw, write_string, write_u64};

/// Appends value `index` of `data` as JSON (`VariantVisitor<JSONConverter>::Visit`).
pub(crate) fn write_variant(data: &VariantData, index: u32, out: &mut Vec<u8>) -> Result<()> {
    let type_id = data.type_id(index)?;
    match type_id {
        V::VariantNull => out.extend_from_slice(b"null"),
        V::BoolTrue => out.extend_from_slice(b"true"),
        V::BoolFalse => out.extend_from_slice(b"false"),
        V::Array => {
            let nested = data.nested(index)?;
            out.push(b'[');
            for i in 0..nested.child_count {
                if i > 0 {
                    out.push(b',');
                }
                write_variant(data, data.child(nested, i)?.values_index, out)?;
            }
            out.push(b']');
        }
        V::Object => {
            let nested = data.nested(index)?;
            out.push(b'{');
            for i in 0..nested.child_count {
                if i > 0 {
                    out.push(b',');
                }
                let child = data.child(nested, i)?;
                write_string(out, data.child_key(child)?.as_bytes());
                out.push(b':');
                write_variant(data, child.values_index, out)?;
            }
            out.push(b'}');
        }
        V::Varchar => write_string(out, data.string(index)?),
        _ => {
            let value = data.to_sql_value(index)?;
            write_scalar(type_id, &value, out)?;
        }
    }
    Ok(())
}

/// The scalar cases of `JSONConverter`.
fn write_scalar(type_id: V, value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match type_id {
        V::Int8 | V::Int16 | V::Int32 | V::Int64 | V::Uint8 | V::Uint16 | V::Uint32 => {
            write_i64(out, value.as_i128().unwrap_or_default() as i64);
        }
        V::Uint64 => write_u64(out, value.as_i128().unwrap_or_default() as u64),
        V::Float | V::Double => write_f64(out, value.as_f64().unwrap_or_default()),
        V::Int128 | V::Uint128 | V::Decimal | V::Bignum => write_raw(out, value.to_varchar()?.as_bytes()),
        _ => write_string(out, value.to_varchar()?.as_bytes()),
    }
    Ok(())
}

/// The JSON text of every row of a VARIANT vector (NULL rows stay NULL).
pub(crate) fn variant_column_to_json(source: &Vector, rows: usize) -> Result<StrBuilder> {
    let mut builder = StrBuilder::default();
    let mut buf = Vec::new();
    for row in 0..rows {
        match VariantData::from_value(&source.get_value(row))? {
            Some(data) => {
                buf.clear();
                write_variant(&data, 0, &mut buf)?;
                builder.push(&buf);
            }
            None => builder.push_null(),
        }
    }
    Ok(builder)
}

/// The cast from VARIANT to JSON (`CastVariantToJSON`).
pub(crate) fn variant_to_json_cast() -> BoundCastInfo {
    BoundCastInfo::new(
        |source: &Vector, result: &mut Vector, count: usize, _: &mut CastParameters| {
            let rows = if source.is_constant() { count.min(1) } else { count };
            let builder = variant_column_to_json(source, rows)?;
            let ty = result.logical_type().clone();
            *result = builder.finish(&ty)?;
            if source.is_constant() {
                result.make_constant();
            }
            Ok(true)
        },
    )
}
