//! `list_aggregate` and its aliases, `list_distinct`/`array_distinct` and
//! `list_unique`/`array_unique` (DuckDB's `list_aggregates.cpp`). `list_aggregate` runs a
//! builtin aggregate over each list; the other two collect the values in a histogram
//! hash map.
//!
//! DuckDB returns the distinct values in the iteration order of its `std::unordered_map`
//! (libc++), keyed by the value for fixed-size types and by the value's sort key otherwise;
//! [`UnorderedOrder`] reproduces that order.

use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_function::aggregate::AggregateInput;
use rustdb_function::{
    AggregateFunction, BindInput, BoundAggregateFunction, FunctionData, FunctionState, NullHandling,
    OwnedStates, ScalarFunction, ScalarFunctionSet, StatePtr,
};
use rustdb_row::hash::hash_bytes;
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::kernels::scatter;
use rustdb_types::{
    DataChunk, ListEntry, LogicalType, LogicalTypeId, PhysicalType, SelectionVector, StringT, Vector,
};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{ListInput, all_constant, list_child_type};
use crate::signature::template;
use rustdb_common::libcxx::UnorderedOrder;

type BindResult = Result<Option<std::sync::Arc<dyn FunctionData>>>;

/// The key of one value in DuckDB's histogram map: a 64-bit hash plus identity bytes.
#[derive(Clone, Debug, PartialEq)]
enum HistogramKey {
    /// A fixed-size value compared with `==` (floats: `-0.0 == 0.0`, NaN never equal).
    Fixed(u64, Option<f64>),
    /// String or sort-key bytes.
    Bytes(Vec<u8>),
}

/// `std::hash` of a fixed-size value read as `bits` with `float` flags.
fn fixed_keys(child: &Vector, size: usize) -> Option<Vec<Option<(u64, HistogramKey)>>> {
    let physical = child.logical_type().physical_type();
    let read = |row: usize| -> Option<(u64, HistogramKey)> {
        if child.is_null(row) {
            return None;
        }
        let value = child.get_value(row);
        let (hash, key) = match physical {
            PhysicalType::Bool => {
                let v = u64::from(value.as_bool()?);
                (v, HistogramKey::Fixed(v, None))
            }
            PhysicalType::Int8 | PhysicalType::Int16 | PhysicalType::Int32 | PhysicalType::Int64 => {
                let v = value.as_i128()? as i64 as u64;
                (v, HistogramKey::Fixed(v, None))
            }
            PhysicalType::UInt8 | PhysicalType::UInt16 | PhysicalType::UInt32 | PhysicalType::UInt64 => {
                let v = value.as_i128()? as u64;
                (v, HistogramKey::Fixed(v, None))
            }
            PhysicalType::Float => {
                let v = value.as_f32()?;
                let hash = if v == 0.0 { 0 } else { u64::from(v.to_bits()) };
                (hash, HistogramKey::Fixed(0, Some(f64::from(v))))
            }
            PhysicalType::Double => {
                let v = value.as_f64()?;
                let hash = if v == 0.0 { 0 } else { v.to_bits() };
                (hash, HistogramKey::Fixed(0, Some(v)))
            }
            _ => return None,
        };
        Some((hash, key))
    };
    match physical {
        PhysicalType::Bool
        | PhysicalType::Int8
        | PhysicalType::Int16
        | PhysicalType::Int32
        | PhysicalType::Int64
        | PhysicalType::UInt8
        | PhysicalType::UInt16
        | PhysicalType::UInt32
        | PhysicalType::UInt64
        | PhysicalType::Float
        | PhysicalType::Double => Some((0..size).map(read).collect()),
        _ => None,
    }
}

#[allow(clippy::float_cmp, reason = "std::equal_to<double> semantics")]
fn key_equal(a: &HistogramKey, b: &HistogramKey) -> bool {
    match (a, b) {
        (HistogramKey::Fixed(x, None), HistogramKey::Fixed(y, None)) => x == y,
        (HistogramKey::Fixed(_, Some(x)), HistogramKey::Fixed(_, Some(y))) => x == y,
        (HistogramKey::Bytes(x), HistogramKey::Bytes(y)) => x == y,
        _ => false,
    }
}

/// The histogram keys of every child row (`None` for NULL children).
fn histogram_keys(child: &Vector, size: usize) -> Result<Vec<Option<(u64, HistogramKey)>>> {
    if let Some(keys) = fixed_keys(child, size) {
        return Ok(keys);
    }
    if child.logical_type().physical_type() == PhysicalType::Varchar {
        let unified = child.unified::<StringT>();
        return Ok((0..size)
            .map(|row| {
                let value = unified.get(row)?;
                let bytes = child.string_value(&value).to_vec();
                Some((hash_bytes(&bytes), HistogramKey::Bytes(bytes)))
            })
            .collect());
    }
    let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
    let keys = create_sort_keys(&[child], &modifiers, size)?;
    Ok((0..size)
        .map(|row| {
            if child.is_null(row) {
                return None;
            }
            let bytes = keys.key(row).to_vec();
            Some((hash_bytes(&bytes), HistogramKey::Bytes(bytes)))
        })
        .collect())
}

/// The distinct child rows of each list (`None` for a NULL list), whether the input was
/// constant, and the list child vector.
type DistinctRows = (Vec<Option<Vec<usize>>>, bool, Vector);

/// For each row, the child rows of its distinct values in DuckDB's order (`None` for a
/// NULL list).
fn distinct_rows(args: &DataChunk) -> Result<DistinctRows> {
    let count = args.len();
    let constant = all_constant(args.columns());
    let rows = if constant { 1 } else { count };
    let lists = addressable(args.column(0), count);
    let input = ListInput::new(&lists)?;
    let keys = histogram_keys(input.child, input.child_size)?;
    let mut result = Vec::with_capacity(rows);
    for row in 0..rows {
        let Some(entry) = input.get(row) else {
            result.push(None);
            continue;
        };
        let mut table: UnorderedOrder<(HistogramKey, usize)> = UnorderedOrder::default();
        let start = entry.offset as usize;
        let end = (entry.offset + entry.length) as usize;
        for (child, slot) in keys[start..end]
            .iter()
            .enumerate()
            .map(|(i, slot)| (start + i, slot))
        {
            if let Some((hash, key)) = slot {
                table.insert(*hash, (key.clone(), child), |a, b| key_equal(&a.0, &b.0));
            }
        }
        result.push(Some(
            table
                .order()
                .into_iter()
                .map(|index| table.keys()[index].1)
                .collect(),
        ));
    }
    Ok((result, constant, input.child.clone()))
}

fn bind_list_aggregate(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let ty = &input.argument_types[0];
    match ty.id() {
        LogicalTypeId::SqlNull => {
            function.arguments[0] = LogicalType::SQLNULL;
            function.return_type = LogicalType::SQLNULL;
        }
        LogicalTypeId::Unknown => {
            function.arguments[0] = LogicalType::simple(LogicalTypeId::Unknown);
            function.return_type = LogicalType::SQLNULL;
        }
        LogicalTypeId::List | LogicalTypeId::Array | LogicalTypeId::Map => {
            let list = LogicalType::list(list_child_type(ty)?);
            function.arguments[0] = list.clone();
            if function.return_type.id() != LogicalTypeId::UBigInt {
                function.return_type = list;
            }
        }
        _ => {
            return Err(Error::invalid_input(
                "First argument of list aggregate must be a list, map or array",
            ));
        }
    }
    Ok(None)
}

/// `list_distinct(list)`.
fn list_distinct(args: &DataChunk, result: &mut Vector) -> Result<()> {
    if result.logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(LogicalType::SQLNULL);
        return Ok(());
    }
    let (rows, constant, child) = distinct_rows(args)?;
    let mut entries = Vec::with_capacity(rows.len());
    let mut nulls = Vec::new();
    let mut selection = Vec::new();
    for (row, children) in rows.iter().enumerate() {
        let start = selection.len();
        match children {
            Some(children) => selection.extend(children.iter().map(|&child| Some(child as u32))),
            None => nulls.push(row),
        }
        entries.push(ListEntry::new(start, selection.len() - start));
    }
    let size = selection.len();
    let mut out_child = Vector::flat(list_child_type(result.logical_type())?, size.max(1));
    if size > 0 {
        let targets: Vec<u32> = (0..size as u32).collect();
        scatter(&child.select_rows(&selection), &mut out_child, &targets)?;
    }
    let row_count = rows.len();
    let mut output = Vector::flat(result.logical_type().clone(), row_count.max(1));
    output.values_mut::<ListEntry>()[..row_count].copy_from_slice(&entries);
    output.set_list_child(out_child, size);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, row_count);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `list_unique(list)`: the number of distinct non-NULL values.
fn list_unique(args: &DataChunk, result: &mut Vector) -> Result<()> {
    if result.logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(LogicalType::SQLNULL);
        return Ok(());
    }
    let (rows, constant, _) = distinct_rows(args)?;
    let row_count = rows.len();
    let mut output = Vector::flat(LogicalType::UBIGINT, row_count.max(1));
    output.reset_flat(row_count);
    let mut nulls = Vec::new();
    for (row, children) in rows.iter().enumerate() {
        match children {
            Some(children) => output.values_mut::<u64>()[row] = children.len() as u64,
            None => nulls.push(row),
        }
    }
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, row_count);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `list_distinct` and `list_unique`, with their aliases.
pub fn list_distinct_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let element = LogicalType::list(template("T"));
    let mut distinct = ScalarFunctionSet::new("list_distinct");
    distinct.add(
        ScalarFunction::new(
            "list_distinct",
            vec![element.clone()],
            element,
            |args, _, result| list_distinct(args, result),
        )
        .with_bind(bind_list_aggregate),
    );
    let mut unique = ScalarFunctionSet::new("list_unique");
    unique.add(
        ScalarFunction::new(
            "list_unique",
            vec![LogicalType::list(LogicalType::ANY)],
            LogicalType::UBIGINT,
            |args, _, result| list_unique(args, result),
        )
        .with_bind(bind_list_aggregate),
    );
    vec![(distinct, &["array_distinct"]), (unique, &["array_unique"])]
}

/// The aggregate a `list_aggregate` call runs (DuckDB's `ListAggregatesBindData`).
#[derive(Debug)]
struct ListAggregateData {
    function: AggregateFunction,
    bind_data: Option<Arc<dyn FunctionData>>,
    /// The aggregate as the binder bound it (`aggr_expr`), for serialized plans.
    expression: BoundAggregateFunction,
}

impl FunctionData for ListAggregateData {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// DuckDB's `ListAggregatesBind<true>` and `ListAggregatesBindFunction<true>`.
fn bind_list_aggregate_function(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let ty = &input.argument_types[0];
    let child_type = match ty.id() {
        LogicalTypeId::SqlNull => {
            function.arguments[0] = LogicalType::SQLNULL;
            function.return_type = LogicalType::SQLNULL;
            return Ok(None);
        }
        LogicalTypeId::Unknown => None,
        LogicalTypeId::List | LogicalTypeId::Array | LogicalTypeId::Map => Some(list_child_type(ty)?),
        _ => {
            return Err(Error::invalid_input(
                "First argument of list aggregate must be a list, map or array",
            ));
        }
    };
    let Some(name) = &input.constants[1] else {
        return Err(Error::invalid_input("Aggregate function name must be a constant"));
    };
    let name = name.to_varchar()?;
    let Some(child_type) = child_type else {
        function.arguments[0] = LogicalType::simple(LogicalTypeId::Unknown);
        function.return_type = LogicalType::SQLNULL;
        return Ok(None);
    };
    let resolver = input
        .aggregates
        .ok_or_else(|| Error::internal("list_aggregate bound without an aggregate resolver"))?;
    // The aggregated input is a NULL constant of the element type
    // (`ListAggregatesBindFunction`).
    let mut arguments = vec![(child_type.clone(), Some(rustdb_types::Value::null(child_type)))];
    for index in 2..input.argument_types.len() {
        arguments.push((
            input.argument_types[index].clone(),
            input.constants[index].clone(),
        ));
    }
    // `StringAggBind` meets a NULL separator by replacing the aggregated input with
    // NULL, which list_aggr does not evaluate: the default separator remains.
    if name.eq_ignore_ascii_case("string_agg")
        && let Some((ty, Some(separator))) = arguments.get_mut(1)
        && separator.is_null()
    {
        *ty = LogicalType::VARCHAR;
        *separator = rustdb_types::Value::varchar(",");
    }
    let bound = resolver.bind_aggregate(&name, &arguments)?;
    if bound.argument_count > 1 {
        return Err(Error::invalid_input(format!(
            "Aggregate function {} is not supported for list_aggr: extra arguments were not removed during bind",
            bound.display
        )));
    }
    let element = bound
        .function
        .arguments
        .first()
        .cloned()
        .ok_or_else(|| Error::internal("list_aggregate on an aggregate without arguments"))?;
    // The extra arguments are consumed by the aggregate's bind.
    function.arguments = vec![LogicalType::list(element), LogicalType::VARCHAR];
    function.varargs = None;
    function.return_type = bound.function.return_type.clone();
    Ok(Some(Arc::new(ListAggregateData {
        function: bound.function.clone(),
        bind_data: bound.bind_data.clone(),
        expression: bound,
    })))
}

/// `ListAggregatesBindData::SerializeFunction`: the result type and the aggregate, NULL
/// when the bind found no list.
fn serialize_list_aggregate(
    serializer: &mut dyn rustdb_serde::Serializer,
    bind_data: Option<&dyn FunctionData>,
    _: &ScalarFunction,
) -> Result<()> {
    let data = bind_data.and_then(|data| data.as_any().downcast_ref::<ListAggregateData>());
    serializer.write_optional_with(100, "bind_data", data.is_none(), |serializer| {
        serializer.on_nullable_begin(data.is_some());
        if let Some(data) = data {
            serializer.write_object_with(|serializer| {
                serializer.write_property(1, "stype", &data.function.return_type)?;
                serializer.on_property_begin(2, "aggr_expr");
                (data.expression.serialize)(serializer)?;
                serializer.on_property_end();
                Ok(())
            })?;
        }
        serializer.on_nullable_end();
        Ok(())
    })
}

/// DuckDB's `ListAggregatesFunction<AggregateFunctor, true>`: one aggregate state per
/// list, updated with the list's elements in order.
fn list_aggregate(args: &DataChunk, state: &FunctionState, result: &mut Vector) -> Result<()> {
    if args.column(0).logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(result.logical_type().clone());
        return Ok(());
    }
    let data = state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<ListAggregateData>())
        .ok_or_else(|| Error::internal("list_aggregate without bind data"))?;
    let ops = &data.function.ops;
    let aggregate_input = AggregateInput {
        bind_data: data.bind_data.as_deref(),
    };
    let count = args.len();
    let lists = addressable(args.column(0), count);
    let input = ListInput::new(&lists)?;
    let child = input.child.flattened(input.child_size);
    let states = OwnedStates::new(Arc::clone(ops), count);
    let pointers = states.pointers();
    let mut nulls = Vec::new();
    let mut selection: Vec<u32> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
    let mut targets: Vec<StatePtr> = Vec::with_capacity(STANDARD_VECTOR_SIZE);
    let flush = |selection: &mut Vec<u32>, targets: &mut Vec<StatePtr>| -> Result<()> {
        if selection.is_empty() {
            return Ok(());
        }
        let slice = child.slice(&SelectionVector::new(std::mem::take(selection)));
        // SAFETY: every target is a state of `states`, initialised and not yet destroyed.
        unsafe { ops.update(&[slice], aggregate_input, targets, targets.len())? };
        targets.clear();
        Ok(())
    };
    for (row, &pointer) in pointers.iter().enumerate() {
        let Some(entry) = input.get(row) else {
            nulls.push(row);
            continue;
        };
        for index in entry.offset..entry.offset + entry.length {
            if selection.len() == STANDARD_VECTOR_SIZE {
                flush(&mut selection, &mut targets)?;
            }
            selection.push(u32::try_from(index).map_err(|_| Error::internal("list child index overflow"))?);
            targets.push(pointer);
        }
    }
    flush(&mut selection, &mut targets)?;
    let mut output = Vector::flat(result.logical_type().clone(), count.max(1));
    // SAFETY: the states are initialised and owned by `states`.
    unsafe { ops.finalize(&pointers, aggregate_input, &mut output, 0)? };
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    if all_constant(args.columns()) {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

/// `list_aggregate(list, name, ...)` and its aliases.
pub fn list_aggregate_functions() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "list_aggregate",
        vec![LogicalType::list(LogicalType::ANY), LogicalType::VARCHAR],
        LogicalType::ANY,
        |args, state, result| list_aggregate(args, state, result),
    )
    .with_bind(bind_list_aggregate_function)
    .with_serialize(serialize_list_aggregate);
    function.null_handling = NullHandling::Special;
    function.varargs = Some(LogicalType::ANY);
    let mut set = ScalarFunctionSet::new("list_aggregate");
    set.add(function);
    set
}
