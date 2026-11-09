//! The casts of the JSON type (`RegisterSimpleCastFunctions` in `json_functions.cpp`,
//! `RegisterJSONCreateCastFunctions` and `RegisterJSONTransformCastFunctions`): VARCHAR
//! to JSON validates, JSON to VARCHAR is free, anything converts to JSON like `to_json`,
//! and JSON converts to anything like `json_transform`.

use std::sync::Arc;

use roastedduck_common::Result;
use roastedduck_function::cast::{BindCastFn, BoundCastInfo, CastFunctionSet, CastParameters};
use roastedduck_types::{ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::json_common::format_parse_error;
use crate::json_executors::{ColumnBuilder, ListBuilder, StrBuilder, StrInput, addressable};
use crate::json_functions::json_create::any_to_json_cast;
use crate::json_functions::json_transform::json_to_any_cast;
use crate::json_type;
use crate::yyjson::{JsonArena, write_val};

/// The number of rows a cast computes: one for constant inputs.
fn cast_rows(source: &Vector, count: usize) -> usize {
    if source.is_constant() { count.min(1) } else { count }
}

/// `CastVarcharToJSON`: keeps the text, NULL (or an error) where it is not valid JSON.
fn varchar_to_json(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let rows = cast_rows(source, count);
    let input_vector = addressable(source, rows);
    let input = StrInput::new(&input_vector);
    let mut arena = JsonArena::default();
    let mut failed = Vec::new();
    let mut success = true;
    for row in 0..rows {
        let Some(json) = input.get(row) else {
            continue;
        };
        arena.reset();
        if let Err(error) = arena.read(json, false) {
            failed.push(row);
            if success {
                params.assign_error(format_parse_error(json, &error, ""))?;
                success = false;
            }
        }
    }
    let mut out = source.reinterpret(result.logical_type().clone())?;
    for row in failed {
        out.set_null(row);
    }
    *result = out;
    Ok(success)
}

/// `CastJSONListToVarchar`: `[a, b, NULL]` from the elements' JSON text.
fn json_list_to_varchar(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    _: &mut CastParameters,
) -> Result<bool> {
    let rows = cast_rows(source, count);
    let source = addressable(source, rows);
    let entries = source.unified::<ListEntry>();
    let (child, _) = source
        .list_child()
        .ok_or_else(|| roastedduck_common::Error::internal("JSON[] cast of a vector without a list child"))?;
    let child_input = StrInput::new(child);
    let mut builder = StrBuilder::default();
    for row in 0..rows {
        let Some(entry) = entries.get(row) else {
            builder.push_null();
            continue;
        };
        builder.push_with(|buf| {
            buf.push(b'[');
            for (i, child_row) in entry.range().enumerate() {
                if i > 0 {
                    buf.extend_from_slice(b", ");
                }
                match child_input.get(child_row) {
                    Some(json) => buf.extend_from_slice(json),
                    None => buf.extend_from_slice(b"NULL"),
                }
            }
            buf.push(b']');
        });
    }
    let ty = result.logical_type().clone();
    *result = builder.finish(&ty)?;
    if source.is_constant() {
        result.make_constant();
    }
    Ok(true)
}

/// `CastVarcharToJSONList`: the elements of a JSON array.
fn varchar_to_json_list(
    source: &Vector,
    result: &mut Vector,
    count: usize,
    params: &mut CastParameters,
) -> Result<bool> {
    let rows = cast_rows(source, count);
    let input_vector = addressable(source, rows);
    let input = StrInput::new(&input_vector);
    let mut arena = JsonArena::default();
    let mut builder = ListBuilder::<StrBuilder>::default();
    let mut success = true;
    for row in 0..rows {
        let Some(json) = input.get(row) else {
            builder.push_null();
            continue;
        };
        arena.reset();
        let parsed = arena.read(json, false);
        let doc = arena.doc();
        match parsed {
            Ok(read) if doc.is_arr(read.root) => {
                builder.push_list(|elements| {
                    for element in doc.arr_iter(read.root) {
                        elements.push_with(|buf| write_val(buf, &doc, element));
                    }
                    Ok(())
                })?;
            }
            parsed => {
                builder.push_null();
                if success {
                    let message = if let Err(error) = parsed {
                        format_parse_error(json, &error, "")
                    } else {
                        let truncated = if json.len() > 50 {
                            format!("{}...", String::from_utf8_lossy(&json[..47]))
                        } else {
                            String::from_utf8_lossy(json).into_owned()
                        };
                        format!("Cannot cast to list of JSON. Input \"{truncated}\"")
                    };
                    params.assign_error(message)?;
                    success = false;
                }
            }
        }
    }
    let ty = result.logical_type().clone();
    *result = builder.finish(&ty)?;
    if source.is_constant() {
        result.make_constant();
    }
    Ok(success)
}

/// The type `LogicalType::AllTypes` entry stands for in a cast registration: nested types
/// with `ANY` children.
fn cast_pattern(id: LogicalTypeId) -> LogicalType {
    match id {
        LogicalTypeId::Struct => LogicalType::struct_type(vec![("any".to_owned().into(), LogicalType::ANY)]),
        LogicalTypeId::List => LogicalType::list(LogicalType::ANY),
        LogicalTypeId::Map => LogicalType::map(LogicalType::ANY, LogicalType::ANY),
        LogicalTypeId::Union => LogicalType::union_type(vec![("any".to_owned().into(), LogicalType::ANY)]),
        LogicalTypeId::Array => LogicalType::array(LogicalType::ANY, 0),
        id => LogicalType::simple(id),
    }
}

/// `LogicalType::AllTypes`.
const ALL_TYPES: [LogicalTypeId; 38] = {
    use LogicalTypeId as L;
    [
        L::Boolean,
        L::TinyInt,
        L::SmallInt,
        L::Integer,
        L::BigInt,
        L::Date,
        L::Time,
        L::TimestampSec,
        L::TimestampMs,
        L::Timestamp,
        L::TimestampNs,
        L::Decimal,
        L::Float,
        L::Double,
        L::Char,
        L::Varchar,
        L::Blob,
        L::Interval,
        L::UTinyInt,
        L::USmallInt,
        L::UInteger,
        L::UBigInt,
        L::TimestampTz,
        L::TimeTz,
        L::TimeNs,
        L::Bit,
        L::Bignum,
        L::UHugeInt,
        L::HugeInt,
        L::Uuid,
        L::Geometry,
        L::Struct,
        L::List,
        L::Map,
        L::Enum,
        L::Union,
        L::Array,
        L::Variant,
    ]
};

/// Whether `ty` is or contains the JSON type.
fn contains_json(ty: &LogicalType) -> bool {
    match ty.id() {
        LogicalTypeId::Varchar => ty
            .alias()
            .is_some_and(|alias| alias.as_bytes() == crate::JSON_TYPE_NAME.as_bytes()),
        LogicalTypeId::List => ty.child_type().is_some_and(contains_json),
        LogicalTypeId::Array => ty.array_child().is_some_and(|(child, _)| contains_json(child)),
        LogicalTypeId::Map => ty
            .map_key_value()
            .is_some_and(|(key, value)| contains_json(key) || contains_json(value)),
        LogicalTypeId::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| contains_json(child))),
        LogicalTypeId::Union => ty
            .union_members()
            .is_some_and(|members| members.iter().any(|(_, member)| contains_json(member))),
        _ => false,
    }
}

/// `ty` with every JSON replaced by VARIANT.
fn json_as_variant(ty: &LogicalType) -> LogicalType {
    match ty.id() {
        LogicalTypeId::Varchar
            if ty
                .alias()
                .is_some_and(|alias| alias.as_bytes() == crate::JSON_TYPE_NAME.as_bytes()) =>
        {
            LogicalType::variant()
        }
        LogicalTypeId::List => {
            LogicalType::list(json_as_variant(ty.child_type().unwrap_or(&LogicalType::SQLNULL)))
        }
        LogicalTypeId::Array => match ty.array_child() {
            Some((child, size)) => LogicalType::array(json_as_variant(child), size),
            None => ty.clone(),
        },
        LogicalTypeId::Map => match ty.map_key_value() {
            Some((key, value)) => LogicalType::map(json_as_variant(key), json_as_variant(value)),
            None => ty.clone(),
        },
        LogicalTypeId::Struct => LogicalType::struct_type(
            ty.struct_children()
                .unwrap_or_default()
                .iter()
                .map(|(name, child)| (name.clone(), json_as_variant(child)))
                .collect(),
        ),
        LogicalTypeId::Union => LogicalType::union_type(
            ty.union_members()
                .unwrap_or_default()
                .iter()
                .map(|(name, member)| (name.clone(), json_as_variant(member)))
                .collect(),
        ),
        _ => ty.clone(),
    }
}

/// Registers the JSON casts (in DuckDB's order: simple, create, transform).
pub fn register_json_casts(casts: &mut CastFunctionSet) {
    let json = json_type();
    // JSON to VARCHAR is basically free.
    casts.register_cast_function(
        json.clone(),
        LogicalType::VARCHAR,
        BoundCastInfo::reinterpret(),
        1,
    );
    // VARCHAR to JSON requires a parse: one more than a cast to STRUCT.
    let varchar_to_json_cost =
        casts.implicit_cast_cost(&LogicalType::SQLNULL, &LogicalType::simple(LogicalTypeId::Struct)) + 1;
    casts.register_cast_function(
        LogicalType::VARCHAR,
        json.clone(),
        BoundCastInfo::new(varchar_to_json),
        varchar_to_json_cost,
    );
    // NULL to JSON with a different cost than NULL to VARCHAR, so the binder can choose.
    let null_to_json_cost = casts.implicit_cast_cost(&LogicalType::SQLNULL, &LogicalType::VARCHAR) + 1;
    casts.register_cast_function(
        LogicalType::SQLNULL,
        json.clone(),
        BoundCastInfo::null_source(),
        null_to_json_cost,
    );
    // JSON[] to VARCHAR (without quoting the elements).
    let json_list = LogicalType::list(json.clone());
    let json_list_to_varchar_cost = casts.implicit_cast_cost(&json_list, &LogicalType::VARCHAR) - 1;
    casts.register_cast_function(
        json_list.clone(),
        LogicalType::VARCHAR,
        BoundCastInfo::new(json_list_to_varchar),
        json_list_to_varchar_cost,
    );
    // JSON[] to JSON is allowed implicitly.
    casts.register_cast_function(
        json_list.clone(),
        json.clone(),
        BoundCastInfo::new(json_list_to_varchar),
        100,
    );
    // VARCHAR to JSON[] (instead of a VARCHAR to VARCHAR[] cast).
    let varchar_to_json_list_cost = casts.implicit_cast_cost(&LogicalType::VARCHAR, &json_list) - 1;
    casts.register_cast_function(
        LogicalType::VARCHAR,
        json_list,
        BoundCastInfo::new(varchar_to_json_list),
        varchar_to_json_list_cost,
    );

    // VARIANT to JSON (DuckDB's core `CastVariantToJSON`).
    casts.register_cast_function(
        LogicalType::variant(),
        json.clone(),
        crate::variant_to_json::variant_to_json_cast(),
        (casts.implicit_cast_cost(&LogicalType::variant(), &LogicalType::VARCHAR) - 1).max(0),
    );

    // JSON to VARIANT (DuckDB's core `ConvertJSONToVariant`).
    casts.register_cast_function(
        json.clone(),
        LogicalType::variant(),
        crate::json_to_variant::json_to_variant_cast(),
        casts.implicit_cast_cost(&LogicalType::VARCHAR, &LogicalType::variant()),
    );

    // Nested types holding JSON convert their JSON parts to VARIANT first (DuckDB's
    // `ToVariant` parses JSON-typed children).
    let nested_to_variant: BindCastFn = Arc::new(
        |set: &CastFunctionSet, source: &LogicalType, target: &LogicalType| {
            if !contains_json(source) {
                return Ok(None);
            }
            let intermediate = json_as_variant(source);
            let first = set.get_cast_function(source, &intermediate)?;
            let second = set.get_cast_function(&intermediate, target)?;
            Ok(Some(BoundCastInfo::new(move |input, result, count, params| {
                let mut converted = Vector::flat(intermediate.clone(), count.max(1));
                let first_ok = first.execute(input, &mut converted, count, params)?;
                let second_ok = second.execute(&converted, result, count, params)?;
                Ok(first_ok && second_ok)
            })))
        },
    );
    for id in [
        LogicalTypeId::Struct,
        LogicalTypeId::List,
        LogicalTypeId::Map,
        LogicalTypeId::Array,
        LogicalTypeId::Union,
    ] {
        let source = cast_pattern(id);
        let cost = casts.implicit_cast_cost(&source, &LogicalType::variant());
        casts.register_bind_function(
            source,
            LogicalType::variant(),
            Arc::clone(&nested_to_variant),
            cost,
        );
    }

    // Anything can be cast to JSON.
    let to_json: BindCastFn = Arc::new(|set: &CastFunctionSet, source: &LogicalType, _: &LogicalType| {
        crate::json_functions::json_create::get_json_type(source)?;
        Ok(Some(any_to_json_cast(set)))
    });
    for id in ALL_TYPES {
        if id == LogicalTypeId::Varchar {
            continue;
        }
        let source = cast_pattern(id);
        let cost = (casts.implicit_cast_cost(&source, &LogicalType::VARCHAR) - 1).max(0);
        casts.register_bind_function(source, json.clone(), Arc::clone(&to_json), cost);
    }

    // JSON can be cast to anything.
    let from_json: BindCastFn =
        Arc::new(|set: &CastFunctionSet, _: &LogicalType, _: &LogicalType| Ok(Some(json_to_any_cast(set))));
    for id in ALL_TYPES {
        if id == LogicalTypeId::Varchar {
            continue;
        }
        let target = cast_pattern(id);
        let cost = casts.implicit_cast_cost(&LogicalType::VARCHAR, &target);
        casts.register_bind_function(json.clone(), target, Arc::clone(&from_json), cost);
    }
}
