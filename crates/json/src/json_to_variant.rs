//! JSON to VARIANT (DuckDB's `json_to_variant.hpp`): objects become OBJECTs, arrays
//! ARRAYs, strings VARCHAR, unsigned integers UINT64, signed ones INT64, and other numbers
//! (including bignums) DOUBLE. A JSON `null` at the top is a NULL VARIANT.

use rustdb_common::{Error, Result};
use rustdb_function::cast::text_parse::TextFloat;
use rustdb_function::cast::{BoundCastInfo, CastParameters};
use rustdb_types::{LogicalType, Value, Vector};

use crate::json_common::read_document;
use crate::json_executors::{StrInput, addressable};
use crate::yyjson::{Doc, JsonArena, ValId, ValType, tag};

/// The SQL value whose VARIANT encoding is that of the JSON value (`ConvertJSON`).
fn json_to_value(doc: &Doc<'_>, val: ValId) -> Result<Value> {
    Ok(match doc.get_type(val) {
        ValType::Null | ValType::None => Value::null(LogicalType::variant()),
        ValType::Obj => Value::struct_value(
            doc.obj_iter(val)
                .map(|(key, value)| {
                    Ok((
                        String::from_utf8_lossy(doc.str(key)).into_owned(),
                        json_to_value(doc, value)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        ),
        ValType::Arr => {
            let items = doc
                .arr_iter(val)
                .map(|element| {
                    let value = json_to_value(doc, element)?;
                    Ok(rustdb_types::variant::encode_value(&value)?
                        .unwrap_or_else(|| Value::null(LogicalType::variant())))
                })
                .collect::<Result<Vec<_>>>()?;
            Value::list(LogicalType::variant(), items)
        }
        ValType::Str => Value::bytes(LogicalType::VARCHAR, doc.str(val)),
        ValType::Bool => Value::boolean(doc.get_bool(val)),
        ValType::Num => match doc.tag(val) {
            tag::UINT => Value::ubigint(doc.get_uint(val)),
            tag::SINT => Value::bigint(doc.get_sint(val)),
            _ => Value::double(doc.get_real(val)),
        },
        ValType::Raw => {
            let Some(value) = f64::parse_float_with(doc.str(val), b'.', true) else {
                return Err(Error::conversion(format!(
                    "Could not convert JSON value {} to DOUBLE",
                    String::from_utf8_lossy(doc.str(val))
                )));
            };
            Value::double(value)
        }
    })
}

/// The cast from JSON to VARIANT.
pub(crate) fn json_to_variant_cast() -> BoundCastInfo {
    BoundCastInfo::new(|source: &Vector, result: &mut Vector, count: usize, _: &mut CastParameters| {
        let rows = if source.is_constant() { count.min(1) } else { count };
        let column = addressable(source, rows);
        let input = StrInput::new(&column);
        let target = result.logical_type().clone();
        let mut out = Vector::flat(target.clone(), rows.max(1));
        let mut arena = JsonArena::default();
        for row in 0..rows {
            let Some(json) = input.get(row) else {
                out.set_null(row);
                continue;
            };
            arena.reset();
            let doc = read_document(&mut arena, json)?;
            let value = json_to_value(&arena.doc(), doc.root)?;
            match rustdb_types::variant::encode_value(&value)? {
                Some(encoded) => out.set_value(row, &encoded)?,
                None => out.set_null(row),
            }
        }
        if source.is_constant() {
            out.make_constant();
        }
        *result = out;
        Ok(true)
    })
}
