//! `json_contains` (DuckDB's `json_contains.cpp`).

use rustdb_common::Result;
use rustdb_function::{FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, LogicalType, Vector};

use crate::json_common::read_document;
use crate::json_executors::{ColumnBuilder, NativeBuilder, StrInput, addressable, all_constant, local_arena};
use crate::json_type;
use crate::yyjson::{Doc, ValId, ValType};

/// `JSONArrayFuzzyEquals`: every needle element equals some haystack element.
fn array_fuzzy_equals(doc: &Doc<'_>, haystack: ValId, needle: ValId) -> bool {
    doc.arr_iter(needle).all(|needle_child| {
        doc.arr_iter(haystack)
            .any(|haystack_child| fuzzy_equals(doc, haystack_child, needle_child))
    })
}

/// `JSONObjectFuzzyEquals`: every needle key is in the haystack with a matching value.
fn object_fuzzy_equals(doc: &Doc<'_>, haystack: ValId, needle: ValId) -> bool {
    doc.obj_iter(needle).all(|(key, needle_child)| {
        doc.obj_getn(haystack, doc.str(key))
            .is_some_and(|haystack_child| fuzzy_equals(doc, haystack_child, needle_child))
    })
}

/// `JSONFuzzyEquals`.
fn fuzzy_equals(doc: &Doc<'_>, haystack: ValId, needle: ValId) -> bool {
    if doc.equals(haystack, doc, needle) {
        return true;
    }
    if doc.tag(needle) != doc.tag(haystack) {
        return false;
    }
    match doc.get_type(haystack) {
        ValType::Arr => array_fuzzy_equals(doc, haystack, needle),
        ValType::Obj => object_fuzzy_equals(doc, haystack, needle),
        _ => false,
    }
}

/// `JSONContains`: the needle matches the haystack or any value nested in it.
fn contains(doc: &Doc<'_>, haystack: ValId, needle: ValId) -> bool {
    if fuzzy_equals(doc, haystack, needle) {
        return true;
    }
    match doc.get_type(haystack) {
        ValType::Arr => doc.arr_iter(haystack).any(|child| contains(doc, child, needle)),
        ValType::Obj => doc
            .obj_iter(haystack)
            .any(|(_, child)| contains(doc, child, needle)),
        _ => false,
    }
}

fn contains_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let haystacks = addressable(args.column(0), rows);
    let needles = addressable(args.column(1), rows);
    let haystack_input = StrInput::new(&haystacks);
    let needle_input = StrInput::new(&needles);
    let arena = local_arena(state);
    let mut builder = NativeBuilder::<bool>::default();
    for row in 0..rows {
        let (Some(haystack), Some(needle)) = (haystack_input.get(row), needle_input.get(row)) else {
            builder.push_null();
            continue;
        };
        arena.reset();
        let needle_doc = read_document(arena, needle)?;
        let haystack_doc = read_document(arena, haystack)?;
        builder.push(contains(&arena.doc(), haystack_doc.root, needle_doc.root));
    }
    *result = builder.finish(&LogicalType::BOOLEAN)?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `JSONFunctions::GetContainsFunction`.
pub(crate) fn contains_function() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("json_contains");
    let json = json_type();
    for (lhs, rhs) in [
        (LogicalType::VARCHAR, LogicalType::VARCHAR),
        (LogicalType::VARCHAR, json.clone()),
        (json.clone(), LogicalType::VARCHAR),
        (json.clone(), json),
    ] {
        set.add(ScalarFunction::new(
            "json_contains",
            vec![lhs, rhs],
            LogicalType::BOOLEAN,
            contains_rows,
        ));
    }
    set
}
