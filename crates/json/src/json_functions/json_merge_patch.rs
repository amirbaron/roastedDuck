//! `json_merge_patch` (DuckDB's `json_merge_patch.cpp`, MySQL semantics): applies each
//! following argument to the first as an RFC 7386 merge patch.

use rustdb_common::Result;
use rustdb_function::{FunctionState, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, Vector};

use crate::json_common::read_document;
use crate::json_executors::{ColumnBuilder, StrBuilder, StrInput, addressable, all_constant, local_arena};
use crate::json_type;
use crate::yyjson::{MutVal, merge_patch};

/// `MergePatch`: objects are merged, anything else replaces the original.
fn merge(orig: &MutVal, patch: MutVal) -> MutVal {
    if matches!(orig, MutVal::Obj(_)) && matches!(patch, MutVal::Obj(_)) {
        merge_patch(Some(orig), &patch)
    } else {
        patch
    }
}

fn merge_patch_rows(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let constant = all_constant(args);
    let rows = if constant { 1 } else { args.len() };
    let columns: Vec<_> = args
        .columns()
        .iter()
        .map(|column| addressable(column, rows))
        .collect();
    let inputs: Vec<StrInput<'_>> = columns.iter().map(|column| StrInput::new(column)).collect();
    let arena = local_arena(state);
    let mut builder = StrBuilder::default();
    let mut read = |json: &[u8]| -> Result<MutVal> {
        arena.reset();
        let doc = read_document(arena, json)?;
        Ok(MutVal::from_doc(&arena.doc(), doc.root))
    };
    for row in 0..rows {
        let mut orig = match inputs.first().and_then(|input| input.get(row)) {
            Some(json) => Some(read(json)?),
            None => None,
        };
        for input in inputs.iter().skip(1) {
            let patch = match input.get(row) {
                Some(json) => Some(read(json)?),
                None => None,
            };
            orig = match (orig, patch) {
                (_, None) => None,
                (None, Some(patch)) => Some(patch),
                (Some(orig), Some(patch)) => Some(merge(&orig, patch)),
            };
        }
        match orig {
            Some(value) => builder.push_with(|buf| value.write(buf)),
            None => builder.push_null(),
        }
    }
    *result = builder.finish(&json_type())?;
    if constant {
        result.make_constant();
    }
    Ok(())
}

/// `JSONFunctions::GetMergePatchFunction`.
pub(crate) fn merge_patch_function() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "json_merge_patch",
        vec![json_type(), json_type()],
        json_type(),
        merge_patch_rows,
    );
    function.varargs = Some(json_type());
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("json_merge_patch");
    set.add(function);
    set
}
