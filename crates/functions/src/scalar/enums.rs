//! ENUM functions (DuckDB's `core_functions/scalar/enum/enum_functions.cpp`):
//! `enum_first`, `enum_last`, `enum_code`, `enum_range` and `enum_range_boundary`.

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, PhysicalType, Value, Vector};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

fn needs_enum() -> Error {
    Error::binder("This function needs an ENUM as an argument")
}

/// DuckDB's `BindEnumFunction`.
fn bind_enum(_: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    match input.argument_types[0].id() {
        LogicalTypeId::Unknown => Err(crate::parameter_not_resolved()),
        LogicalTypeId::Enum => Ok(None),
        _ => Err(needs_enum()),
    }
}

/// The members of an ENUM type in definition order.
fn members(ty: &LogicalType) -> Result<&[String]> {
    ty.enum_values()
        .ok_or_else(|| Error::internal("ENUM function on a non-ENUM type"))
}

/// Writes a constant result (DuckDB's `result.Reference(value)`).
fn constant(result: &mut Vector, value: &Value) -> Result<()> {
    *result = Vector::constant(value)?;
    Ok(())
}

fn varchar_list(values: &[String]) -> Value {
    Value::list(LogicalType::VARCHAR, values.iter().map(Value::varchar).collect())
}

/// A function over the ENUM type of its single argument whose result is a constant.
fn type_function(name: &str, return_type: LogicalType, value: fn(&[String]) -> Value) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        name,
        vec![LogicalType::ANY],
        return_type,
        move |args: &DataChunk, _, result| {
            let values = members(args.column(0).logical_type())?;
            constant(result, &value(values))
        },
    )
    .with_bind(bind_enum);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new(name);
    set.add(function);
    set
}

/// `enum_first(enum)`: the first member.
pub fn enum_first_functions() -> ScalarFunctionSet {
    type_function("enum_first", LogicalType::VARCHAR, |values| {
        values
            .first()
            .map_or_else(|| Value::null(LogicalType::VARCHAR), Value::varchar)
    })
}

/// `enum_last(enum)`: the last member.
pub fn enum_last_functions() -> ScalarFunctionSet {
    type_function("enum_last", LogicalType::VARCHAR, |values| {
        values
            .last()
            .map_or_else(|| Value::null(LogicalType::VARCHAR), Value::varchar)
    })
}

/// `enum_range(enum)`: every member.
pub fn enum_range_functions() -> ScalarFunctionSet {
    type_function(
        "enum_range",
        LogicalType::list(LogicalType::VARCHAR),
        varchar_list,
    )
}

/// DuckDB's `BindEnumCodeFunction`: the result is the ENUM's index type.
fn bind_enum_code(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    bind_enum(function, input)?;
    function.return_type = match input.argument_types[0].physical_type() {
        PhysicalType::UInt8 => LogicalType::UTINYINT,
        PhysicalType::UInt16 => LogicalType::USMALLINT,
        PhysicalType::UInt32 => LogicalType::UINTEGER,
        PhysicalType::UInt64 => LogicalType::UBIGINT,
        _ => return Err(Error::internal("Unsupported Enum Internal Type")),
    };
    Ok(None)
}

/// `enum_code(enum)`: the member index of each row.
pub fn enum_code_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "enum_code",
        vec![LogicalType::ANY],
        LogicalType::ANY,
        |args, _, result| {
            *result = args.column(0).reinterpret(result.logical_type().clone())?;
            Ok(())
        },
    )
    .with_bind(bind_enum_code);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("enum_code");
    set.add(function);
    set
}

/// DuckDB's `BindEnumRangeBoundaryFunction`.
fn bind_enum_range_boundary(_: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let (first, second) = (&input.argument_types[0], &input.argument_types[1]);
    if first.id() == LogicalTypeId::Unknown || second.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let allowed = |ty: &LogicalType| matches!(ty.id(), LogicalTypeId::Enum | LogicalTypeId::SqlNull);
    if !allowed(first) || !allowed(second) {
        return Err(needs_enum());
    }
    if first.id() == LogicalTypeId::SqlNull && second.id() == LogicalTypeId::SqlNull {
        return Err(needs_enum());
    }
    if first.id() == LogicalTypeId::Enum && second.id() == LogicalTypeId::Enum && first != second {
        return Err(Error::binder(
            "The parameters need to link to ONLY one enum OR be NULL ",
        ));
    }
    Ok(None)
}

/// The member index stored in row 0 of an ENUM vector, or `None` if it is NULL.
fn first_code(vector: &Vector) -> Option<u64> {
    fn get<T: rustdb_types::NativeType + Into<u64>>(vector: &Vector) -> Option<u64> {
        vector.unified::<T>().get(0).map(Into::into)
    }
    if vector.is_null(0) {
        return None;
    }
    match vector.logical_type().physical_type() {
        PhysicalType::UInt8 => get::<u8>(vector),
        PhysicalType::UInt16 => get::<u16>(vector),
        PhysicalType::UInt32 => get::<u32>(vector),
        PhysicalType::UInt64 => get::<u64>(vector),
        _ => None,
    }
}

/// `enum_range_boundary(start, end)`: the members from `start` to `end` (inclusive); a
/// NULL bound means the first or last member. Like DuckDB, only the first row is read.
fn enum_range_boundary(args: &DataChunk, result: &mut Vector) -> Result<()> {
    let (first, second) = (args.column(0), args.column(1));
    let start = first_code(first);
    let source = if start.is_none() { second } else { first };
    let values = members(source.logical_type())?;
    let start = usize::try_from(start.unwrap_or(0)).unwrap_or(usize::MAX);
    let end = match first_code(second) {
        Some(code) => usize::try_from(code).map_or(usize::MAX, |code| code.saturating_add(1)),
        None => members(first.logical_type())?.len(),
    };
    let range = values.get(start..end.min(values.len())).unwrap_or_default();
    constant(result, &varchar_list(range))
}

/// `enum_range_boundary(enum, enum)`.
pub fn enum_range_boundary_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "enum_range_boundary",
        vec![LogicalType::ANY, LogicalType::ANY],
        LogicalType::list(LogicalType::VARCHAR),
        |args, _, result| enum_range_boundary(args, result),
    )
    .with_bind(bind_enum_range_boundary);
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("enum_range_boundary");
    set.add(function);
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mood() -> LogicalType {
        LogicalType::enum_type(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()])
    }

    fn boundary(first: &Value, second: &Value) -> String {
        let args = DataChunk::new(
            vec![
                Vector::constant(first).unwrap(),
                Vector::constant(second).unwrap(),
            ],
            1,
        );
        let mut result = Vector::flat(LogicalType::list(LogicalType::VARCHAR), 1);
        enum_range_boundary(&args, &mut result).unwrap();
        crate::values::as_text(&result.get_value(0)).unwrap()
    }

    #[test]
    fn range_boundary_matches_duckdb() {
        let value = |index| Value::enum_value(mood(), index);
        let null = Value::null(mood());
        assert_eq!(boundary(&value(1), &null), "[b, c]");
        assert_eq!(boundary(&null, &value(1)), "[a, b]");
        assert_eq!(boundary(&value(0), &value(1)), "[a, b]");
        assert_eq!(boundary(&value(2), &value(0)), "[]");
    }
}
