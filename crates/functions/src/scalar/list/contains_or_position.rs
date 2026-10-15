//! `list_contains`/`list_has`/`array_contains`/`array_has`, `list_position`/
//! `list_indexof`/`array_position`/`array_indexof` (DuckDB's `contains_or_position.cpp`)
//! and `map_contains` (`map_contains.cpp`).

use rustdb_common::Result;
use rustdb_function::{NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, create_sort_keys};
use rustdb_types::{DataChunk, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::addressable;
use crate::scalar::nested::{ListInput, struct_field};
use crate::signature::template;

/// DuckDB's `ListSearchOp`: searches each list (whose elements are `source`, a vector
/// parallel to the list's child) for the target, comparing values by sort key. Returns a
/// BOOLEAN (contains) or the 1-based INTEGER position; `find_nulls` lets a NULL target
/// match a NULL element.
pub(crate) fn list_search(
    lists: &Vector,
    source: &Vector,
    source_size: usize,
    target: &Vector,
    count: usize,
    position: bool,
    find_nulls: bool,
    result: &mut Vector,
) -> Result<()> {
    let input = ListInput::new(lists)?;
    let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
    let source_keys = create_sort_keys(&[source], &modifiers, source_size)?;
    let target_keys = create_sort_keys(&[target], &modifiers, count)?;
    let ty = if position {
        LogicalType::INTEGER
    } else {
        LogicalType::BOOLEAN
    };
    let mut out = Vector::flat(ty, count.max(1));
    out.reset_flat(count);
    let mut nulls = Vec::new();
    for row in 0..count {
        let Some(entry) = input.get(row) else {
            nulls.push(row);
            continue;
        };
        let target_valid = !target.is_null(row);
        let finished = !find_nulls && !target_valid;
        if finished || entry.length == 0 {
            if finished || position {
                nulls.push(row);
            } else {
                out.values_mut::<bool>()[row] = false;
            }
            continue;
        }
        let (offset, length) = (entry.offset as usize, entry.length as usize);
        let found = (offset..offset + length).find(|&child| {
            let child_valid = !source.is_null(child);
            (find_nulls && !child_valid && !target_valid)
                || (child_valid && target_valid && source_keys.key(child) == target_keys.key(row))
        });
        match (found, position) {
            (Some(child), true) => out.values_mut::<i32>()[row] = (1 + child - offset) as i32,
            (Some(_), false) => out.values_mut::<bool>()[row] = true,
            (None, true) => nulls.push(row),
            (None, false) => out.values_mut::<bool>()[row] = false,
        }
    }
    let validity = out.validity_mut();
    for row in nulls {
        validity.set_invalid(row, count);
    }
    if count == 1 {
        out.make_constant();
    }
    *result = out;
    Ok(())
}

/// `ListSearchFunction` over the list's own elements.
fn list_search_function(args: &DataChunk, result: &mut Vector, position: bool) -> Result<()> {
    if result.logical_type().id() == LogicalTypeId::SqlNull {
        *result = Vector::constant_null(LogicalType::SQLNULL);
        return Ok(());
    }
    let count = args.len();
    let lists = addressable(args.column(0), count);
    let target = addressable(args.column(1), count);
    let (child, child_size) = lists
        .list_child()
        .map(|(child, size)| (child.clone(), size))
        .ok_or_else(|| rustdb_common::Error::internal("list search on a non-list vector"))?;
    list_search(
        &lists, &child, child_size, &target, count, position, position, result,
    )
}

/// The `list_contains(LIST(T), T)` overload (also used by `contains`).
pub(crate) fn list_contains_function(name: &str) -> ScalarFunction {
    let element = template("T");
    ScalarFunction::new(
        name,
        vec![LogicalType::list(element.clone()), element],
        LogicalType::BOOLEAN,
        |args, _, result| list_search_function(args, result, false),
    )
}

/// `list_contains`.
pub fn list_contains_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("list_contains");
    set.add(list_contains_function("list_contains"));
    set
}

/// `list_position`: a NULL target finds the first NULL element.
pub fn list_position_functions() -> ScalarFunctionSet {
    let element = template("T");
    let mut function = ScalarFunction::new(
        "list_position",
        vec![LogicalType::list(element.clone()), element],
        LogicalType::INTEGER,
        |args, _, result| list_search_function(args, result, true),
    );
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("list_position");
    set.add(function);
    set
}

/// The `map_contains(MAP(K, V), K)` overload (also used by `contains`).
pub(crate) fn map_contains_function(name: &str) -> ScalarFunction {
    let key = template("K");
    let value = template("V");
    ScalarFunction::new(
        name,
        vec![LogicalType::map(key.clone(), value), key],
        LogicalType::BOOLEAN,
        |args, _, result| {
            let count = args.len();
            let maps = addressable(args.column(0), count);
            let target = addressable(args.column(1), count);
            let (child, child_size) = maps
                .list_child()
                .map(|(child, size)| (child.clone(), size))
                .ok_or_else(|| rustdb_common::Error::internal("map_contains on a non-map vector"))?;
            let keys = struct_field(&child, 0, child_size)?;
            list_search(&maps, &keys, child_size, &target, count, false, false, result)
        },
    )
}

/// `map_contains`.
pub fn map_contains_functions() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("map_contains");
    set.add(map_contains_function("map_contains"));
    set
}
