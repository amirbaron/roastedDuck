//! Generic functions (DuckDB's `function/scalar/generic` and
//! `core_functions/scalar/generic`): `typeof`, `least`/`greatest`, `hash`, `error`,
//! `constant_or_null`, `create_sort_key`, ...

use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::cast::{CastFunctionSet, CastParameters, cast_vector};
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet, Stability};
use rustdb_row::hash::hash_columns;
use rustdb_row::sort::{
    NullOrder, OrderModifiers, OrderType, create_sort_keys, decode_sort_key, sort_key_type_for,
};
use rustdb_types::{LogicalType, LogicalTypeId, StringT, Value, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::all_constant;
use crate::signature::normalize_type;

/// A constant result.
fn constant(result: &mut Vector, value: &Value) -> Result<()> {
    *result = Vector::constant(value)?;
    Ok(())
}

fn set(name: &str, functions: Vec<ScalarFunction>) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for function in functions {
        set.add(function);
    }
    set
}

/// `typeof(x) -> VARCHAR`: the name of the argument's type.
pub fn typeof_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "typeof",
        vec![LogicalType::ANY],
        LogicalType::VARCHAR,
        |args, _, result| constant(result, &Value::varchar(args.column(0).logical_type().to_string())),
    );
    function.null_handling = NullHandling::Special;
    set("typeof", vec![function])
}

/// `get_type(x) -> TYPE`.
pub fn get_type_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "get_type",
        vec![LogicalType::ANY],
        LogicalType::simple(LogicalTypeId::Type),
        |args, _, result| constant(result, &Value::type_value(args.column(0).logical_type().clone())),
    )
    .with_bind(|function, input| {
        if input.argument_types[0].id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        function.arguments[0] = input.argument_types[0].clone();
        Ok(None)
    });
    function.null_handling = NullHandling::Special;
    set("get_type", vec![function])
}

/// `alias(x) -> VARCHAR`: the name of the argument expression.
pub fn alias_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "alias",
        vec![LogicalType::ANY],
        LogicalType::VARCHAR,
        |_, state, result| {
            let name = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ConstantValue>())
                .map_or_else(|| Value::varchar(""), |data| data.0.clone());
            constant(result, &name)
        },
    )
    .with_bind(|_, input| Ok(Some(Arc::new(ConstantValue(Value::varchar(&input.names[0]))))));
    function.null_handling = NullHandling::Special;
    set("alias", vec![function])
}

/// A value computed at bind time.
#[derive(Debug, Clone, PartialEq)]
struct ConstantValue(Value);

impl FunctionData for ConstantValue {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `version() -> VARCHAR` (DuckDB's `LibraryVersion`).
pub fn version_functions() -> ScalarFunctionSet {
    set(
        "version",
        vec![ScalarFunction::new(
            "version",
            Vec::new(),
            LogicalType::VARCHAR,
            |_, _, result| constant(result, &Value::varchar("v1.5.6")),
        )],
    )
}

/// `can_cast_implicitly(source, target) -> BOOLEAN`.
pub fn can_cast_implicitly_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "can_cast_implicitly",
        vec![LogicalType::ANY, LogicalType::ANY],
        LogicalType::BOOLEAN,
        |args, _, result| {
            let cost = CastFunctionSet::new()
                .implicit_cast_cost(args.column(0).logical_type(), args.column(1).logical_type());
            constant(result, &Value::boolean(cost >= 0))
        },
    );
    function.null_handling = NullHandling::Special;
    set("can_cast_implicitly", vec![function])
}

/// `cast_to_type(x, type_of)`: `x` cast to the type of the second argument.
pub fn cast_to_type_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "cast_to_type",
        vec![LogicalType::ANY, LogicalType::ANY],
        LogicalType::ANY,
        |args, _, result| {
            let target = result.logical_type().clone();
            *result = cast_vector(
                &CastFunctionSet::new(),
                args.column(0),
                &target,
                args.len(),
                &mut CastParameters::new(),
            )?;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let target = input.argument_types[1].clone();
        match target.id() {
            LogicalTypeId::Unknown => return Err(crate::parameter_not_resolved()),
            LogicalTypeId::SqlNull => {
                return Err(Error::invalid_input(
                    "cast_to_type cannot be used to cast to NULL",
                ));
            }
            _ => {}
        }
        function.arguments = vec![input.argument_types[0].clone(), target.clone()];
        function.return_type = target;
        Ok(None)
    });
    function.null_handling = NullHandling::Special;
    set("cast_to_type", vec![function])
}

/// `error(message)`: raises an Invalid Input Error with the message.
pub fn error_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "error",
        vec![LogicalType::VARCHAR],
        LogicalType::SQLNULL,
        |args, _, result| {
            let input = addressable(args.column(0), args.len());
            let strings = input.unified::<StringT>();
            for row in 0..args.len() {
                if let Some(message) = strings.get(row) {
                    return Err(Error::invalid_input(
                        String::from_utf8_lossy(input.string_value(&message)).into_owned(),
                    ));
                }
            }
            *result = Vector::constant_null(LogicalType::SQLNULL);
            Ok(())
        },
    );
    function.stability = Stability::Volatile;
    set("error", vec![function])
}

/// `constant_or_null(constant, x, ...)`: the constant where every other argument is
/// non-NULL (used by the optimizer).
pub fn constant_or_null_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "constant_or_null",
        vec![LogicalType::ANY, LogicalType::ANY],
        LogicalType::ANY,
        |args, state, result| {
            let value = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<ConstantValue>())
                .ok_or_else(|| Error::internal("constant_or_null without bind data"))?;
            let count = args.len();
            let columns = &args.columns()[1..];
            if columns
                .iter()
                .any(|column| column.is_constant() && column.is_null(0))
            {
                *result = Vector::constant_null(value.0.logical_type().clone());
                return Ok(());
            }
            let mut out = Vector::constant(&value.0)?;
            let nulls: Vec<usize> = (0..count)
                .filter(|&row| columns.iter().any(|column| column.is_null(row)))
                .collect();
            if !nulls.is_empty() {
                out.flatten(count);
                for row in nulls {
                    out.validity_mut().set_invalid(row, count);
                }
            }
            *result = out;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        if input.argument_types[0].id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let Some(value) = &input.constants[0] else {
            return Err(Error::binder("ConstantOrNull requires a constant input"));
        };
        function.return_type = input.argument_types[0].clone();
        Ok(Some(Arc::new(ConstantValue(value.clone()))))
    });
    function.varargs = Some(LogicalType::ANY);
    set("constant_or_null", vec![function])
}

/// `hash(x, ...) -> UBIGINT` (DuckDB's `DataChunk::Hash`).
pub fn hash_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "hash",
        vec![LogicalType::ANY],
        LogicalType::UBIGINT,
        |args, _, result| {
            let constant = all_constant(args.columns());
            let rows = if constant { 1 } else { args.len() };
            let columns: Vec<Vector> = args
                .columns()
                .iter()
                .map(|column| addressable(column, rows).into_owned())
                .collect();
            let refs: Vec<&Vector> = columns.iter().collect();
            let mut hashes = hash_columns(&refs, rows)?;
            if constant {
                hashes.make_constant();
            }
            *result = hashes;
            Ok(())
        },
    );
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    set("hash", vec![function])
}

/// `least`/`greatest`: the smallest/largest non-NULL argument (DuckDB's `least.cpp`), compared
/// through sort keys whose NULL placement makes NULLs lose.
fn least_greatest(name: &'static str, greatest: bool) -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        name,
        vec![LogicalType::ANY],
        LogicalType::ANY,
        move |args, _, result| {
            if args.column_count() == 1 {
                *result = args.column(0).clone();
                return Ok(());
            }
            let constant = all_constant(args.columns());
            let rows = if constant { 1 } else { args.len() };
            // Ascending keys; NULLs last for `least` and first for `greatest`, so they never win.
            let modifiers = OrderModifiers::new(
                OrderType::Ascending,
                if greatest {
                    NullOrder::NullsFirst
                } else {
                    NullOrder::NullsLast
                },
            );
            let columns: Vec<Vector> = args
                .columns()
                .iter()
                .map(|column| addressable(column, rows).into_owned())
                .collect();
            let keys = columns
                .iter()
                .map(|column| create_sort_keys(&[column], &[modifiers], rows))
                .collect::<Result<Vec<_>>>()?;
            let mut out = Vector::flat(result.logical_type().clone(), rows.max(1));
            for row in 0..rows {
                let mut best: Option<usize> = None;
                for (index, column) in columns.iter().enumerate() {
                    if column.is_null(row) {
                        continue;
                    }
                    let better = best.is_none_or(|best| {
                        let ordering = keys[index].key(row).cmp(keys[best].key(row));
                        if greatest {
                            ordering.is_gt()
                        } else {
                            ordering.is_lt()
                        }
                    });
                    if better {
                        best = Some(index);
                    }
                }
                if let Some(index) = best {
                    decode_sort_key(
                        keys[index].key(row),
                        std::slice::from_mut(&mut out),
                        row,
                        &[modifiers],
                    )?;
                } else {
                    let capacity = out.capacity();
                    out.validity_mut().set_invalid(row, capacity);
                }
            }
            if constant {
                out.make_constant();
            }
            *result = out;
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let mut child_type = input.argument_types[0].clone();
        for ty in &input.argument_types[1..] {
            child_type = LogicalType::try_max_logical_type(&child_type, ty).ok_or_else(|| {
                Error::binder(format!(
                    "Cannot combine types of {child_type} and {ty} - an explicit cast is required"
                ))
            })?;
        }
        if child_type.id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let child_type = normalize_type(&child_type);
        function.arguments[0] = child_type.clone();
        function.varargs = Some(child_type.clone());
        function.return_type = child_type;
        Ok(None)
    });
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    set(name, vec![function])
}

/// The `least` set.
pub fn least_functions() -> ScalarFunctionSet {
    least_greatest("least", false)
}

/// The `greatest` set.
pub fn greatest_functions() -> ScalarFunctionSet {
    least_greatest("greatest", true)
}

/// The ORDER BY modifiers of a `create_sort_key` call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SortKeyModifiers(Vec<OrderModifiers>);

impl FunctionData for SortKeyModifiers {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// DuckDB's `CreateSortKeyBind`.
fn bind_create_sort_key(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if !input.argument_types.len().is_multiple_of(2) {
        return Err(Error::binder(
            "Arguments to create_sort_key must be [key1, sort_specifier1, key2, sort_specifier2, ...]",
        ));
    }
    let mut modifiers = Vec::new();
    for index in (1..input.argument_types.len()).step_by(2) {
        let Some(specifier) = &input.constants[index] else {
            return Err(Error::binder(format!(
                "sort_specifier must be a constant value - but got {}",
                input.aliases[index]
            )));
        };
        if specifier.is_null() {
            return Err(Error::binder("sort_specifier cannot be NULL"));
        }
        modifiers.push(OrderModifiers::parse(&specifier.to_varchar()?)?);
    }
    let key_types: Vec<LogicalType> = input.argument_types.iter().step_by(2).cloned().collect();
    function.return_type = sort_key_type_for(&key_types);
    Ok(Some(Arc::new(SortKeyModifiers(modifiers))))
}

/// `create_sort_key(key, 'ASC NULLS LAST', ...)`.
pub fn create_sort_key_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "create_sort_key",
        vec![LogicalType::ANY],
        LogicalType::BLOB,
        |args, state, result| {
            let modifiers = state
                .bind_data
                .as_ref()
                .and_then(|data| data.as_any().downcast_ref::<SortKeyModifiers>())
                .ok_or_else(|| Error::internal("create_sort_key without bind data"))?;
            let count = args.len();
            let columns: Vec<Vector> = args
                .columns()
                .iter()
                .step_by(2)
                .map(|column| addressable(column, count).into_owned())
                .collect();
            let refs: Vec<&Vector> = columns.iter().collect();
            let keys = create_sort_keys(&refs, &modifiers.0, count)?;
            rustdb_row::sort::encode::sort_keys_to_vector(&keys, result)
        },
    )
    .with_bind(bind_create_sort_key);
    function.varargs = Some(LogicalType::ANY);
    function.null_handling = NullHandling::Special;
    set("create_sort_key", vec![function])
}

/// DuckDB's `TypeVisitor::VisitReplace`: `ty` with every occurrence of `from` (also
/// nested) replaced by `to`.
fn replace_type(ty: &LogicalType, from: &LogicalType, to: &LogicalType) -> LogicalType {
    if ty == from {
        return to.clone();
    }
    let replace = |child: &LogicalType| replace_type(child, from, to);
    match ty.id() {
        LogicalTypeId::List => ty
            .child_type()
            .map_or_else(|| ty.clone(), |child| LogicalType::list(replace(child))),
        LogicalTypeId::Array => ty.array_child().map_or_else(
            || ty.clone(),
            |(child, size)| LogicalType::array(replace(child), size),
        ),
        LogicalTypeId::Map => ty.map_key_value().map_or_else(
            || ty.clone(),
            |(key, value)| LogicalType::map(replace(key), replace(value)),
        ),
        LogicalTypeId::Struct => ty.struct_children().map_or_else(
            || ty.clone(),
            |children| {
                LogicalType::struct_type(
                    children
                        .iter()
                        .map(|(name, child)| (name.clone(), replace(child)))
                        .collect(),
                )
            },
        ),
        LogicalTypeId::Union => ty.union_members().map_or_else(
            || ty.clone(),
            |members| {
                LogicalType::union_type(
                    members
                        .iter()
                        .map(|(name, child)| (name.clone(), replace(child)))
                        .collect(),
                )
            },
        ),
        _ => ty.clone(),
    }
}

/// `replace_type(value, from_type_witness, to_type_witness)`: casts `value` to its type
/// with `from` replaced by `to` (DuckDB's `BindReplaceTypeFunction`, which rewrites the
/// call into the cast).
fn replace_type_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "replace_type",
        vec![LogicalType::ANY, LogicalType::ANY, LogicalType::ANY],
        LogicalType::ANY,
        |args, _, result| {
            *result = args.column(0).clone();
            Ok(())
        },
    )
    .with_bind(|function, input| {
        let (from, to) = (&input.argument_types[1], &input.argument_types[2]);
        if from.id() == LogicalTypeId::Unknown || to.id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        if to.id() == LogicalTypeId::SqlNull {
            return Err(Error::invalid_input(
                "replace_type cannot be used to replace type with NULL",
            ));
        }
        let target = replace_type(&input.argument_types[0], from, to);
        function.arguments[0] = target.clone();
        function.return_type = target;
        Ok(None)
    });
    function.null_handling = NullHandling::Special;
    set("replace_type", vec![function])
}

/// `vector_type(value)`: the physical representation of the argument (DuckDB's
/// `VectorType` names).
fn vector_type_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "vector_type",
        vec![LogicalType::ANY],
        LogicalType::VARCHAR,
        |args, _, result| {
            let name = match args.column(0).kind() {
                rustdb_types::VectorKind::Flat => "FLAT_VECTOR",
                rustdb_types::VectorKind::Constant => "CONSTANT_VECTOR",
                rustdb_types::VectorKind::Dictionary => "DICTIONARY_VECTOR",
                rustdb_types::VectorKind::Sequence => "SEQUENCE_VECTOR",
            };
            constant(result, &Value::varchar(name))
        },
    );
    function.null_handling = NullHandling::Special;
    set("vector_type", vec![function])
}

/// `sleep_ms(ms)`: sleeps per row (negative values count as 0) and returns NULL.
fn sleep_ms_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "sleep_ms",
        vec![LogicalType::BIGINT],
        LogicalType::SQLNULL,
        |args, _, result| {
            let count = args.len();
            let input = addressable(args.column(0), count);
            let values = input.unified::<i64>();
            for row in 0..count {
                if let Some(ms) = values.get(row) {
                    std::thread::sleep(std::time::Duration::from_millis(u64::try_from(ms).unwrap_or(0)));
                }
            }
            *result = Vector::constant_null(LogicalType::SQLNULL);
            Ok(())
        },
    );
    function.null_handling = NullHandling::Special;
    function.stability = Stability::Volatile;
    set("sleep_ms", vec![function])
}

/// Every generic function set with its aliases.
pub fn generic_functions() -> Vec<ScalarFunctionSet> {
    vec![
        typeof_functions(),
        get_type_functions(),
        alias_functions(),
        version_functions(),
        can_cast_implicitly_functions(),
        cast_to_type_functions(),
        error_functions(),
        constant_or_null_functions(),
        hash_functions(),
        least_functions(),
        greatest_functions(),
        create_sort_key_functions(),
        replace_type_functions(),
        vector_type_functions(),
        sleep_ms_functions(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdb_function::FunctionState;

    #[test]
    fn typeof_names_the_type() {
        let function = &typeof_functions().functions[0];
        let args = rustdb_types::DataChunk::new(vec![Vector::constant(&Value::bigint(1)).unwrap()], 1);
        let mut result = Vector::flat(LogicalType::VARCHAR, 0);
        (function.kernel)(&args, &mut FunctionState::default(), &mut result).unwrap();
        assert_eq!(result.get_value(0), Value::varchar("BIGINT"));
    }
}
