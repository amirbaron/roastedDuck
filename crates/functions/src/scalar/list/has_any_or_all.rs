//! `list_has_any`/`array_has_any`/`&&` and `list_has_all`/`array_has_all`/`@>`/`<@`
//! (DuckDB's `list_has_any_or_all.cpp`).

use std::collections::HashSet;

use rustdb_common::Result;
use rustdb_function::{ScalarFunction, ScalarFunctionSet};
use rustdb_row::sort::{NullOrder, OrderModifiers, OrderType, SortKeyBuffer, create_sort_keys};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Vector};

use crate::scalar::executor::{addressable, binary};
use crate::scalar::nested::{ListInput, list_child_type};
use crate::signature::template;

fn keys(input: &ListInput<'_>) -> Result<SortKeyBuffer> {
    let modifiers = [OrderModifiers::new(OrderType::Ascending, NullOrder::NullsLast)];
    create_sort_keys(&[input.child], &modifiers, input.child_size)
}

fn child_is_null_type(vector: &Vector) -> bool {
    list_child_type(vector.logical_type()).is_ok_and(|child| child.id() == LogicalTypeId::SqlNull)
}

/// The non-NULL element keys of a list.
fn element_keys<'a>(input: &ListInput<'_>, keys: &'a SortKeyBuffer, entry: ListEntry) -> HashSet<&'a [u8]> {
    (entry.offset as usize..(entry.offset + entry.length) as usize)
        .filter(|&index| !input.child.is_null(index))
        .map(|index| keys.key(index))
        .collect()
}

/// `list_has_any(left, right)`: whether the lists share a non-NULL element.
fn has_any(args: &DataChunk, result: &mut Vector) -> Result<()> {
    if child_is_null_type(args.column(0)) || child_is_null_type(args.column(1)) {
        let mut constant = Vector::flat(LogicalType::BOOLEAN, 1);
        constant.values_mut::<bool>()[0] = false;
        constant.make_constant();
        *result = constant;
        return Ok(());
    }
    let count = args.len();
    let left_lists = addressable(args.column(0), count);
    let right_lists = addressable(args.column(1), count);
    let left = ListInput::new(&left_lists)?;
    let right = ListInput::new(&right_lists)?;
    let left_keys = keys(&left)?;
    let right_keys = keys(&right)?;
    binary::<ListEntry, ListEntry, bool>(args, result, |l, r, out| {
        if l.length == 0 || r.length == 0 {
            *out = false;
            return Ok(true);
        }
        let (build, build_input, build_keys, probe, probe_input, probe_keys) = if r.length < l.length {
            (r, &right, &right_keys, l, &left, &left_keys)
        } else {
            (l, &left, &left_keys, r, &right, &right_keys)
        };
        let set = element_keys(build_input, build_keys, build);
        *out = (probe.offset as usize..(probe.offset + probe.length) as usize)
            .any(|index| !probe_input.child.is_null(index) && set.contains(probe_keys.key(index)));
        Ok(true)
    })
}

/// `list_has_all(left, right)`: whether every non-NULL element of `right` is in `left`
/// (`<@` swaps the arguments).
fn has_all(args: &DataChunk, result: &mut Vector, swap: bool) -> Result<()> {
    let (left_index, right_index) = if swap { (1, 0) } else { (0, 1) };
    if child_is_null_type(args.column(left_index)) && child_is_null_type(args.column(right_index)) {
        let mut constant = Vector::flat(LogicalType::BOOLEAN, 1);
        constant.values_mut::<bool>()[0] = true;
        constant.make_constant();
        *result = constant;
        return Ok(());
    }
    let count = args.len();
    let left_lists = addressable(args.column(left_index), count);
    let right_lists = addressable(args.column(right_index), count);
    let left = ListInput::new(&left_lists)?;
    let right = ListInput::new(&right_lists)?;
    let left_keys = keys(&left)?;
    let right_keys = keys(&right)?;
    let ordered = DataChunk::new(
        vec![left_lists.as_ref().clone(), right_lists.as_ref().clone()],
        count,
    );
    binary::<ListEntry, ListEntry, bool>(&ordered, result, |build, probe, out| {
        if probe.length == 0 {
            *out = true;
            return Ok(true);
        }
        let set = element_keys(&left, &left_keys, build);
        *out = (probe.offset as usize..(probe.offset + probe.length) as usize)
            .all(|index| right.child.is_null(index) || set.contains(right_keys.key(index)));
        Ok(true)
    })
}

fn signature() -> Vec<LogicalType> {
    let list = LogicalType::list(template("T"));
    vec![list.clone(), list]
}

/// `list_has_any`, `list_has_all` and `<@`, with their aliases.
pub fn has_any_or_all_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    let mut any = ScalarFunctionSet::new("list_has_any");
    any.add(ScalarFunction::new(
        "list_has_any",
        signature(),
        LogicalType::BOOLEAN,
        |args, _, result| has_any(args, result),
    ));
    let mut all = ScalarFunctionSet::new("list_has_all");
    all.add(ScalarFunction::new(
        "list_has_all",
        signature(),
        LogicalType::BOOLEAN,
        |args, _, result| has_all(args, result, false),
    ));
    let mut contained = ScalarFunctionSet::new("<@");
    contained.add(ScalarFunction::new(
        "<@",
        signature(),
        LogicalType::BOOLEAN,
        |args, _, result| has_all(args, result, true),
    ));
    vec![
        (any, &["array_has_any", "&&"]),
        (all, &["array_has_all", "@>"]),
        (contained, &[]),
    ]
}
