//! `list_distinct`/`array_distinct` and `list_unique`/`array_unique` (DuckDB's
//! `list_aggregates.cpp`, which collects the values in a histogram hash map).
//!
//! DuckDB returns the distinct values in the iteration order of its `std::unordered_map`
//! (libc++), keyed by the value for fixed-size types and by the value's sort key otherwise;
//! [`UnorderedOrder`] reproduces that order.

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, ScalarFunction, ScalarFunctionSet};
use rustdb_row::hash::hash_bytes;
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::kernels::scatter;
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, PhysicalType, StringT, Vector};

use crate::libcxx::UnorderedOrder;
use crate::scalar::executor::addressable;
use crate::scalar::nested::{ListInput, all_constant, list_child_type};
use crate::signature::template;

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

/// For each row, the child rows of its distinct values in DuckDB's order (`None` for a
/// NULL list).
fn distinct_rows(args: &DataChunk) -> Result<(Vec<Option<Vec<usize>>>, bool, Vector)> {
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
        for child in entry.offset as usize..(entry.offset + entry.length) as usize {
            if let Some((hash, key)) = &keys[child] {
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
